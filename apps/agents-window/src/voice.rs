//! Dictation for the composer. One worker thread owns the microphone and the
//! recognizer: SAPI on Windows, whisper.cpp everywhere else. The UI receives
//! events only.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

#[cfg(not(windows))]
#[path = "voice_whisper.rs"]
mod whisper_dictation;

// SAPI reports completed phrases alone, with nothing to prepare.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VoiceEvent {
    /// What the session is doing before it listens, or after.
    Status(String),
    Listening,
    /// The phrase being spoken, so far. The next one replaces it.
    Interim(String),
    /// A completed phrase, which also ends the preview of it.
    Final(String),
    Error(String),
    Stopped,
}

pub(crate) enum Stop {
    /// Close the microphone and report what was said up to here.
    Finish,
    /// End now and discard what was not reported yet.
    Cancel,
}

pub(crate) struct VoiceHandle {
    stop: Sender<Stop>,
    events: Receiver<VoiceEvent>,
}

impl VoiceHandle {
    /// `Stopped` follows once the last phrase has been reported.
    pub(crate) fn finish(&self) {
        let _ = self.stop.send(Stop::Finish);
    }

    pub(crate) fn cancel(&self) {
        let _ = self.stop.send(Stop::Cancel);
    }

    pub(crate) fn try_recv(&self) -> Result<VoiceEvent, TryRecvError> {
        self.events.try_recv()
    }
}

impl Drop for VoiceHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Starts continuous dictation. Each finalized phrase arrives as `Final`;
/// the caller can insert it at the editor's retained caret. `Stopped` closes
/// the session after it finishes, is cancelled or fails. Starting does not
/// submit a turn. `models` is where a recognizer that needs one keeps it.
pub(crate) fn start(models: &Path) -> Result<VoiceHandle, String> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let models = models.to_owned();
    std::thread::Builder::new()
        .name("openide-dictation".to_owned())
        .spawn(move || {
            #[cfg(windows)]
            let result = {
                let _ = &models;
                windows_dictation::run(&stop_rx, &event_tx)
            };
            #[cfg(not(windows))]
            let result = whisper_dictation::run(&models, &stop_rx, &event_tx);
            if let Err(error) = result {
                let _ = event_tx.send(VoiceEvent::Error(error));
            }
            let _ = event_tx.send(VoiceEvent::Stopped);
        })
        .map_err(|error| format!("No se pudo iniciar el reconocimiento de voz: {error}"))?;
    Ok(VoiceHandle {
        stop: stop_tx,
        events: event_rx,
    })
}

#[cfg(windows)]
mod windows_dictation {
    use super::{Receiver, Sender, Stop, TryRecvError, VoiceEvent};
    use std::ffi::c_void;
    use windows::core::{IUnknown, Interface, PCWSTR, PWSTR};
    use windows::Win32::Media::Speech::{
        ISpRecoContext, ISpRecoResult, SpSharedRecoContext, SPEI_RECOGNITION,
        SPET_LPARAM_IS_OBJECT, SPET_LPARAM_IS_POINTER, SPET_LPARAM_IS_STRING, SPET_LPARAM_IS_TOKEN,
        SPEVENT, SPLO_STATIC, SPRS_ACTIVE, SPRS_INACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
        COINIT_MULTITHREADED,
    };

    const NOTIFY_POLL_MS: u32 = 150;

    pub(super) fn run(stop: &Receiver<Stop>, events: &Sender<VoiceEvent>) -> Result<(), String> {
        // No COM interface leaves this worker. MTA fits the event based wait
        // without requiring an STA message pump on the egui thread.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|error| format!("No se pudo inicializar COM: {error}"))?;
        let result = run_initialized(stop, events);
        unsafe { CoUninitialize() };
        result
    }

    fn run_initialized(stop: &Receiver<Stop>, events: &Sender<VoiceEvent>) -> Result<(), String> {
        let context: ISpRecoContext =
            unsafe { CoCreateInstance(&SpSharedRecoContext, None, CLSCTX_ALL) }.map_err(
                |error| format!("El reconocedor de voz de Windows no está disponible: {error}"),
            )?;

        let grammar = unsafe { context.CreateGrammar(1) }
            .map_err(|error| format!("No se pudo crear la gramática de dictado: {error}"))?;
        unsafe { grammar.LoadDictation(PCWSTR::null(), SPLO_STATIC) }
            .map_err(|error| format!("No hay una gramática de dictado instalada: {error}"))?;

        let interest = 1_u64 << SPEI_RECOGNITION.0;
        unsafe { context.SetInterest(interest, interest) }
            .map_err(|error| format!("No se pudo escuchar el dictado: {error}"))?;
        unsafe { context.SetNotifyWin32Event() }
            .map_err(|error| format!("No se pudo iniciar la notificación de voz: {error}"))?;
        unsafe { grammar.SetDictationState(SPRS_ACTIVE) }
            .map_err(|error| format!("No se pudo abrir el micrófono: {error}"))?;
        let _ = events.send(VoiceEvent::Listening);

        let result = receive_phrases(&context, stop, events);
        let _ = unsafe { grammar.SetDictationState(SPRS_INACTIVE) };
        result
    }

    fn receive_phrases(
        context: &ISpRecoContext,
        stop: &Receiver<Stop>,
        events: &Sender<VoiceEvent>,
    ) -> Result<(), String> {
        loop {
            match stop.try_recv() {
                Ok(_) | Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => {}
            }
            unsafe { context.WaitForNotifyEvent(NOTIFY_POLL_MS) }
                .map_err(|error| format!("Falló la espera del micrófono: {error}"))?;
            loop {
                match stop.try_recv() {
                    Ok(_) | Err(TryRecvError::Disconnected) => return Ok(()),
                    Err(TryRecvError::Empty) => {}
                }
                let mut event = SPEVENT::default();
                let mut fetched = 0;
                unsafe { context.GetEvents(1, &mut event, &mut fetched) }
                    .map_err(|error| format!("No se pudo leer el resultado de voz: {error}"))?;
                if fetched == 0 {
                    break;
                }
                if let Some(text) = unsafe { finalized_text(event) }? {
                    if events.send(VoiceEvent::Final(text)).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// SAPI's two WORD fields are projected as one i32 bitfield by windows-rs.
    /// The event owns its lParam; consuming it here releases the COM reference.
    unsafe fn finalized_text(event: SPEVENT) -> Result<Option<String>, String> {
        let event_id = (event._bitfield as u32 & 0xffff) as i32;
        let parameter_type = ((event._bitfield as u32 >> 16) & 0xffff) as i32;
        let parameter = event.lParam.0 as *mut c_void;
        if parameter.is_null() {
            return Ok(None);
        }
        if event_id == SPEI_RECOGNITION.0 && parameter_type == SPET_LPARAM_IS_OBJECT.0 {
            let result = unsafe { ISpRecoResult::from_raw(parameter) };
            let mut text = PWSTR::null();
            if let Err(error) = unsafe { result.GetText(u32::MAX, u32::MAX, true, &mut text, None) }
            {
                if !text.is_null() {
                    unsafe { CoTaskMemFree(Some(text.0.cast())) };
                }
                return Err(format!("No se pudo convertir voz a texto: {error}"));
            }
            if text.is_null() {
                return Ok(None);
            }
            let converted = unsafe { text.to_string() }
                .map_err(|error| format!("El dictado devolvió texto no válido: {error}"));
            unsafe { CoTaskMemFree(Some(text.0.cast())) };
            return converted.map(|value| (!value.trim().is_empty()).then_some(value));
        }
        if parameter_type == SPET_LPARAM_IS_OBJECT.0 || parameter_type == SPET_LPARAM_IS_TOKEN.0 {
            drop(unsafe { IUnknown::from_raw(parameter) });
        } else if parameter_type == SPET_LPARAM_IS_POINTER.0
            || parameter_type == SPET_LPARAM_IS_STRING.0
        {
            unsafe { CoTaskMemFree(Some(parameter.cast())) };
        }
        Ok(None)
    }
}
