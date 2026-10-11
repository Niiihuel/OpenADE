//! Dictation off Windows. cpal owns the microphone and whisper.cpp transcribes
//! it on this machine, both on the one worker thread; no audio leaves it. The
//! model is downloaded once into the data directory.

use super::{Stop, VoiceEvent};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    time::Duration,
};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

// The quantized multilingual `small`: the smallest model that holds up in
// Spanish, and quick enough on a CPU for a preview while speaking.
const MODEL: &str = "ggml-small-q5_1.bin";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin";
/// The rate whisper.cpp takes.
const RATE: u32 = 16_000;

pub(super) fn run(
    models: &Path,
    stop: &Receiver<Stop>,
    events: &Sender<VoiceEvent>,
) -> Result<(), String> {
    let Some(model) = ensure_model(models, stop, events)? else {
        return Ok(());
    };
    let status = |text: &str| {
        let _ = events.send(VoiceEvent::Status(text.to_owned()));
    };
    status("Cargando el modelo de voz…");
    whisper_rs::install_logging_hooks();
    let context = WhisperContext::new_with_params(
        model
            .to_str()
            .ok_or("La ruta del modelo de voz no es UTF-8")?,
        WhisperContextParameters::default(),
    )
    .map_err(|error| format!("No se pudo cargar el modelo de voz: {error}"))?;
    let mut state = context
        .create_state()
        .map_err(|error| format!("No se pudo preparar el reconocedor de voz: {error}"))?;
    let language = language();

    let (audio_tx, audio) = mpsc::channel();
    let (stream, rate) = open_microphone(audio_tx, events.clone())?;
    stream
        .play()
        .map_err(|error| format!("No se pudo abrir el micrófono: {error}"))?;
    let _ = events.send(VoiceEvent::Listening);

    let mut segmenter = Segmenter::new(rate);
    loop {
        match stop.try_recv() {
            Ok(Stop::Finish) => {
                // Closing the stream first keeps what was said up to the click.
                drop(stream);
                for chunk in audio.try_iter() {
                    segmenter.push(&chunk);
                }
                if let Some(samples) = segmenter.take() {
                    status("Transcribiendo…");
                    if let Some(text) = transcribe(&mut state, &samples, &language)? {
                        let _ = events.send(VoiceEvent::Final(text));
                    }
                }
                return Ok(());
            }
            Ok(Stop::Cancel) | Err(TryRecvError::Disconnected) => return Ok(()),
            Err(TryRecvError::Empty) => {}
        }
        let chunk = match audio.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => chunk,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err("El micrófono se desconectó".to_owned())
            }
        };
        let event = match segmenter.push(&chunk) {
            Some(Cut::Interim) => {
                transcribe(&mut state, &segmenter.speech(), &language)?.map(VoiceEvent::Interim)
            }
            Some(Cut::Final) => {
                let samples = segmenter.take().unwrap_or_default();
                // An empty final still clears the preview of a phrase that
                // turned out to be noise.
                Some(VoiceEvent::Final(
                    transcribe(&mut state, &samples, &language)?.unwrap_or_default(),
                ))
            }
            None => None,
        };
        if let Some(event) = event {
            if events.send(event).is_err() {
                return Ok(());
            }
        }
    }
}

/// The model's path, downloading it on first use. `None` when the session
/// was stopped meanwhile.
fn ensure_model(
    models: &Path,
    stop: &Receiver<Stop>,
    events: &Sender<VoiceEvent>,
) -> Result<Option<PathBuf>, String> {
    let path = models.join(MODEL);
    if path.is_file() {
        return Ok(Some(path));
    }
    let failed =
        |error: &dyn std::fmt::Display| format!("No se pudo descargar el modelo de voz: {error}");
    fs::create_dir_all(models).map_err(|error| failed(&error))?;
    let part = models.join(format!("{MODEL}.part"));
    let mut response = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        // The default total timeout would cut a download of this size.
        .timeout(None)
        .build()
        .and_then(|client| client.get(MODEL_URL).send())
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| failed(&error))?;
    let total = response.content_length();
    let mut file = fs::File::create(&part).map_err(|error| failed(&error))?;
    let mut buffer = vec![0; 1 << 16];
    let (mut done, mut shown) = (0_u64, None);
    loop {
        if !matches!(stop.try_recv(), Err(TryRecvError::Empty)) {
            drop(file);
            let _ = fs::remove_file(&part);
            return Ok(None);
        }
        let read = response.read(&mut buffer).map_err(|error| failed(&error))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .map_err(|error| failed(&error))?;
        done += read as u64;
        let megabytes = done >> 20;
        if shown != Some(megabytes) {
            shown = Some(megabytes);
            let _ = events.send(VoiceEvent::Status(match total {
                Some(total) if total > 0 => format!(
                    "Descargando el modelo de voz (una sola vez)… {} %",
                    done * 100 / total
                ),
                _ => format!("Descargando el modelo de voz (una sola vez)… {megabytes} MB"),
            }));
        }
    }
    file.sync_all().map_err(|error| failed(&error))?;
    drop(file);
    if total.is_some_and(|total| total != done) {
        let _ = fs::remove_file(&part);
        return Err(failed(&"la descarga quedó incompleta"));
    }
    fs::rename(&part, &path).map_err(|error| failed(&error))?;
    Ok(Some(path))
}

/// The language of the session's locale, or whisper's own detection.
fn language() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.is_empty())
        .and_then(|value| {
            let code = value.split(['_', '.', '@']).next()?.to_ascii_lowercase();
            whisper_rs::get_lang_id(&code).map(|_| code)
        })
        .unwrap_or_else(|| "auto".to_owned())
}

/// The default input device, delivering mono chunks at its own rate.
fn open_microphone(
    audio: Sender<Vec<f32>>,
    events: Sender<VoiceEvent>,
) -> Result<(cpal::Stream, u32), String> {
    fn build<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        audio: Sender<Vec<f32>>,
        events: Sender<VoiceEvent>,
    ) -> Result<cpal::Stream, cpal::BuildStreamError>
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        let channels = usize::from(config.channels.max(1));
        device.build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mono = data
                    .chunks(channels)
                    .map(|frame| {
                        frame
                            .iter()
                            .map(|sample| <f32 as cpal::FromSample<T>>::from_sample_(*sample))
                            .sum::<f32>()
                            / frame.len() as f32
                    })
                    .collect();
                let _ = audio.send(mono);
            },
            move |error| {
                let _ = events.send(VoiceEvent::Error(format!("Falló el micrófono: {error}")));
            },
            None,
        )
    }

    let device = cpal::default_host()
        .default_input_device()
        .ok_or("No hay un micrófono disponible")?;
    let supported = device
        .default_input_config()
        .map_err(|error| format!("No se pudo leer la configuración del micrófono: {error}"))?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, audio, events),
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, audio, events),
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, audio, events),
        cpal::SampleFormat::I32 => build::<i32>(&device, &config, audio, events),
        format => return Err(format!("Formato de micrófono no admitido: {format}")),
    }
    .map_err(|error| format!("No se pudo abrir el micrófono: {error}"))?;
    Ok((stream, config.sample_rate))
}

fn transcribe(
    state: &mut WhisperState,
    samples: &[f32],
    language: &str,
) -> Result<Option<String>, String> {
    // whisper.cpp refuses less than a second of audio.
    let mut samples = samples.to_vec();
    samples.resize(samples.len().max(RATE as usize * 11 / 10), 0.0);
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(language));
    params.set_n_threads(
        std::thread::available_parallelism().map_or(4, |count| count.get().min(8)) as i32,
    );
    // The encoder works through 30 s (1500 frames) whatever it is given.
    // A phrase is shorter: encode its length and a margin, as whisper.cpp's
    // own streaming does, which is most of the time a preview takes.
    params.set_audio_ctx((samples.len() / (RATE as usize / 50) + 128).clamp(384, 1500) as i32);
    // Each phrase stands alone, so a misheard one cannot steer the next.
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, &samples)
        .map_err(|error| format!("No se pudo convertir voz a texto: {error}"))?;
    let text: String = state
        .as_iter()
        .filter_map(|segment| segment.to_str_lossy().ok().map(|text| text.into_owned()))
        .collect();
    Ok(clean(&text))
}

/// The spoken words alone: whisper writes sounds as `[música]` or `(risas)`.
fn clean(text: &str) -> Option<String> {
    let mut depth = 0_u32;
    let mut spoken = String::new();
    for character in text.chars() {
        match character {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => spoken.push(character),
            _ => {}
        }
    }
    let spoken = spoken.split_whitespace().collect::<Vec<_>>().join(" ");
    (!spoken.is_empty()).then_some(spoken)
}

/// Average each window of input into one output sample, which also filters
/// what the lower rate cannot carry.
fn resample(input: &[f32], from: u32) -> Vec<f32> {
    if from == RATE || from == 0 {
        return input.to_vec();
    }
    let step = f64::from(from) / f64::from(RATE);
    (0..(input.len() as f64 / step) as usize)
        .map(|index| {
            let start = (index as f64 * step) as usize;
            let end = (((index + 1) as f64 * step) as usize).clamp(start + 1, input.len());
            input[start..end].iter().sum::<f32>() / (end - start) as f32
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Cut {
    /// Enough new speech for a preview of the phrase so far.
    Interim,
    /// The phrase ended: transcribe it and start the next.
    Final,
}

/// Cuts the microphone's stream into phrases at its pauses.
// shortcut: an energy gate against a running noise floor, which a loud room
// can hold open until the length cap; upgrade to whisper.cpp's VAD model if
// phrases stop being cut.
struct Segmenter {
    rate: u32,
    samples: Vec<f32>,
    floor: f32,
    heard: bool,
    quiet: usize,
    since_interim: usize,
}

impl Segmenter {
    fn new(rate: u32) -> Self {
        Self {
            rate: rate.max(1),
            samples: Vec::new(),
            floor: 0.0,
            heard: false,
            quiet: 0,
            since_interim: 0,
        }
    }

    fn seconds(&self, seconds: f32) -> usize {
        (self.rate as f32 * seconds) as usize
    }

    fn push(&mut self, chunk: &[f32]) -> Option<Cut> {
        if chunk.is_empty() {
            return None;
        }
        let level =
            (chunk.iter().map(|sample| sample * sample).sum::<f32>() / chunk.len() as f32).sqrt();
        let loud = level > (self.floor * 3.0).max(0.004);
        self.samples.extend_from_slice(chunk);
        if loud {
            self.heard = true;
            self.quiet = 0;
        } else {
            self.floor = self.floor * 0.95 + level * 0.05;
            self.quiet += chunk.len();
        }
        if !self.heard {
            // Keep a moment before the first word, so its onset is not cut.
            let keep = self.seconds(0.3);
            let excess = self.samples.len().saturating_sub(keep);
            self.samples.drain(..excess);
            return None;
        }
        self.since_interim += chunk.len();
        if self.quiet >= self.seconds(0.8) || self.samples.len() >= self.seconds(25.0) {
            return Some(Cut::Final);
        }
        if self.since_interim >= self.seconds(2.0) {
            self.since_interim = 0;
            return Some(Cut::Interim);
        }
        None
    }

    /// The phrase so far, at whisper's rate.
    fn speech(&self) -> Vec<f32> {
        resample(&self.samples, self.rate)
    }

    /// End the phrase. `None` when nothing was said.
    fn take(&mut self) -> Option<Vec<f32>> {
        let speech = self.heard.then(|| self.speech());
        self.samples.clear();
        self.heard = false;
        self.quiet = 0;
        self.since_interim = 0;
        speech
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICE: u32 = 48_000;

    /// 100 ms chunks of a tone, or of faint noise.
    fn chunks(seconds: f32, voiced: bool) -> Vec<Vec<f32>> {
        let chunk: Vec<f32> = (0..DEVICE / 10)
            .map(|index| {
                let wave = (index as f32 * 0.05).sin();
                wave * if voiced { 0.2 } else { 0.001 }
            })
            .collect();
        vec![chunk; (seconds * 10.0) as usize]
    }

    fn feed(segmenter: &mut Segmenter, seconds: f32, voiced: bool) -> Vec<Cut> {
        chunks(seconds, voiced)
            .iter()
            .filter_map(|chunk| segmenter.push(chunk))
            .collect()
    }

    #[test]
    fn a_phrase_is_previewed_while_spoken_and_cut_at_its_pause() {
        let mut segmenter = Segmenter::new(DEVICE);
        assert_eq!(feed(&mut segmenter, 3.0, false), []);
        // Silence before the first word is not kept, beyond its onset.
        assert!(segmenter.samples.len() <= segmenter.seconds(0.3));
        assert_eq!(segmenter.take(), None);

        assert_eq!(
            feed(&mut segmenter, 4.5, true),
            [Cut::Interim, Cut::Interim]
        );
        assert_eq!(feed(&mut segmenter, 0.8, false), [Cut::Final]);
        let speech = segmenter.take().unwrap();
        // 4.5 s of speech and the 0.8 s pause that ended it, at 16 kHz.
        assert_eq!(speech.len(), (RATE as f32 * 5.3) as usize);
        assert_eq!(segmenter.take(), None);
        assert_eq!(feed(&mut segmenter, 2.0, false), []);
    }

    #[test]
    fn speech_without_a_pause_is_cut_at_the_length_cap() {
        let mut segmenter = Segmenter::new(DEVICE);
        let cuts = feed(&mut segmenter, 25.0, true);
        assert_eq!(cuts.last(), Some(&Cut::Final));
        assert_eq!(cuts.iter().filter(|cut| **cut == Cut::Final).count(), 1);
    }

    #[test]
    fn resampling_keeps_the_duration_and_the_level() {
        assert_eq!(resample(&[0.5; 48_000], 48_000).len(), 16_000);
        assert_eq!(resample(&[0.5; 44_100], 44_100).len(), 16_000);
        assert_eq!(resample(&[0.5; 8_000], 8_000).len(), 16_000);
        assert_eq!(resample(&[0.5; 16_000], 16_000).len(), 16_000);
        assert!(resample(&[0.5; 44_100], 44_100)
            .iter()
            .all(|sample| (sample - 0.5).abs() < 1e-6));
        assert!(resample(&[], 48_000).is_empty());
    }

    #[test]
    fn only_spoken_words_survive_cleaning() {
        assert_eq!(clean(" Hola,  mundo. "), Some("Hola, mundo.".to_owned()));
        assert_eq!(clean(" [BLANK_AUDIO]"), None);
        assert_eq!(
            clean("(música) Revisá [risas] las pruebas"),
            Some("Revisá las pruebas".to_owned())
        );
        assert_eq!(clean(""), None);
    }

    #[test]
    #[ignore = "needs a microphone"]
    fn the_default_microphone_delivers_audio() {
        let (audio_tx, audio) = mpsc::channel();
        let (events, _events) = mpsc::channel();
        let (stream, rate) = open_microphone(audio_tx, events).unwrap();
        stream.play().unwrap();
        let mut heard = 0;
        while heard < rate as usize / 2 {
            heard += audio.recv_timeout(Duration::from_secs(3)).unwrap().len();
        }
        println!("{rate} Hz");
    }

    /// `OPENIDE_DICTATION_WAV=<16 kHz mono s16 wav> cargo test … -- --ignored`
    /// transcribes a recording with the real model, downloading it if needed.
    #[test]
    #[ignore = "downloads the voice model and needs a recording"]
    fn a_recording_is_transcribed_by_the_real_model() {
        let wav = fs::read(std::env::var("OPENIDE_DICTATION_WAV").unwrap()).unwrap();
        let data = wav.windows(4).position(|tag| tag == b"data").unwrap() + 8;
        let samples: Vec<f32> = wav[data..]
            .chunks_exact(2)
            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
            .collect();
        let models = std::env::temp_dir().join("openide-dictation-models");
        let (_stop, stop) = mpsc::channel();
        let (events, _events) = mpsc::channel();
        let model = ensure_model(&models, &stop, &events).unwrap().unwrap();
        let context = WhisperContext::new_with_params(
            model.to_str().unwrap(),
            WhisperContextParameters::default(),
        )
        .unwrap();
        let mut state = context.create_state().unwrap();
        // The session's own path: phrases cut at the speaker's pauses.
        let mut segmenter = Segmenter::new(RATE);
        let (mut text, mut previews) = (String::new(), 0);
        let started = std::time::Instant::now();
        for chunk in samples.chunks(RATE as usize / 10) {
            match segmenter.push(chunk) {
                Some(Cut::Interim) => {
                    let preview = transcribe(&mut state, &segmenter.speech(), "auto").unwrap();
                    println!("  … {}", preview.unwrap_or_default());
                    previews += 1;
                }
                Some(Cut::Final) => {
                    let phrase = segmenter.take().unwrap();
                    let phrase = transcribe(&mut state, &phrase, "auto").unwrap();
                    println!("  ✓ {}", phrase.clone().unwrap_or_default());
                    text.extend(phrase.map(|phrase| phrase + " "));
                }
                None => {}
            }
        }
        if let Some(rest) = segmenter.take() {
            text.extend(transcribe(&mut state, &rest, "auto").unwrap());
        }
        println!(
            "{:.1} s of audio in {:.1?}: {text}",
            samples.len() as f32 / RATE as f32,
            started.elapsed()
        );
        assert!(previews > 0);
        assert!(text.to_lowercase().contains("country"), "{text}");
    }
}
