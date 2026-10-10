//! Read-only copies of a process-local command's retained pending output.

use crate::{
    theme,
    view::{MainPage, SideTab, ViewState},
};
use base64::Engine;
use openide_harness::{
    PersistentCommandEvent, PersistentCommandExit, PersistentCommandInspection,
    PersistentCommandStream,
};
use openide_ui::{Button, ButtonGroup, ButtonVariant, Component, ControlSize, Element, FontFamily, Modal};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub(crate) struct NativeCommandOutputUiState {
    pub owner_session_id: Option<String>,
    pub command_id: Option<String>,
    pub workspace: Option<PathBuf>,
    pub request_id: Option<String>,
    pub loading: bool,
    pub snapshot: Option<NativeCommandOutputSnapshot>,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeCommandOutputSnapshot {
    pub inspection: PersistentCommandInspection,
    pub observed_at_unix_ms: u64,
    pub details_json: String,
}

impl NativeCommandOutputSnapshot {
    pub(crate) fn new(inspection: PersistentCommandInspection) -> Self {
        let observed_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let events = inspection.events.iter().enumerate().map(|(index, event)| {
            match event {
                PersistentCommandEvent::Output { stream, bytes } => {
                    let stream = match stream {
                        PersistentCommandStream::Stdout => "stdout",
                        PersistentCommandStream::Stderr => "stderr",
                        PersistentCommandStream::Terminal => "terminal",
                    };
                    match std::str::from_utf8(bytes) {
                        Ok(text) => serde_json::json!({"index":index,"stream":stream,"bytes":bytes.len(),"encoding":"utf8","text":text}),
                        Err(_) => serde_json::json!({"index":index,"stream":stream,"bytes":bytes.len(),"encoding":"base64","data":base64::engine::general_purpose::STANDARD.encode(bytes)}),
                    }
                }
                PersistentCommandEvent::Omitted { bytes } => serde_json::json!({"index":index,"omitted_bytes":bytes}),
            }
        }).collect::<Vec<_>>();
        let details_json =
            serde_json::to_string_pretty(&events).expect("output copy JSON serializes");
        Self {
            inspection,
            observed_at_unix_ms,
            details_json,
        }
    }
}

impl NativeCommandOutputUiState {
    pub(crate) fn matches_scope(&self, owner: Option<&str>, workspace: &Path) -> bool {
        self.owner_session_id.as_deref() == owner
            && self.owner_session_id.is_some()
            && self.command_id.is_some()
            && workspace.canonicalize().ok().as_deref() == self.workspace.as_deref()
    }
}

pub(crate) fn modal(state: &ViewState, base: Element, viewport_width: f32) -> Element {
    let local = &state.native_command_output;
    if state.show_settings
        || state.main_page != MainPage::Workspace
        || !state.show_side_pane
        || state.side_tab != SideTab::Activity
        || !local.matches_scope(
            state.selected_session.as_deref(),
            Path::new(&state.workspace),
        )
    {
        return base;
    }
    let width = Modal::fit_width(viewport_width, 820.0);
    let body_width = crate::ui_sizing::form_width(width).max(0.0);
    let text = |value: String, size| {
        Element::text(value)
            .font_family(FontFamily::Sans)
            .foreground(theme::FOREGROUND)
            .font_size(size)
    };
    let note = |value: &str| {
        Element::text(value)
            .font_family(FontFamily::Sans)
            .foreground(theme::SETTINGS_FOREGROUND_SUBTLE)
            .font_size(11.0)
    };
    let owner = local.owner_session_id.as_deref().expect("scope checked");
    let command_id = local.command_id.as_deref().expect("scope checked");
    let mut rows = vec![
        text(
            format!(
                "Conversación: {owner}\nComando: {command_id}\nProyecto: {}",
                local.workspace.as_ref().expect("scope checked").display()
            ),
            12.0,
        )
        .font_family(FontFamily::Monospace),
        note(
            "Copia de la salida pendiente al consultar. Actualizar reemplaza esta copia; las lecturas del agente pueden consumir el buffer mientras la revisas. Esta vista no envía entrada ni ejecuta comandos.",
        ),
    ];
    if local.loading {
        rows.push(note("Leyendo una copia del buffer…"));
    }
    if let Some(error) = &local.error {
        rows.push(text(
            format!("No se pudo inspeccionar la salida: {error}"),
            12.0,
        ));
        rows.push(note("El ID puede haberse retirado al entregar el resultado final al agente. No se recupera el proceso al consultar o reabrir la aplicación."));
    }
    if let Some(snapshot) = &local.snapshot {
        let inspected = &snapshot.inspection;
        let observed_at = i64::try_from(snapshot.observed_at_unix_ms)
            .ok()
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%d/%m/%Y %H:%M:%S %:z")
                    .to_string()
            })
            .unwrap_or_else(|| "fecha fuera del rango representable".into());
        let status = match &inspected.exit {
            None => "En ejecución".into(),
            Some(PersistentCommandExit::Exited { code }) => format!(
                "Finalizado · código {}",
                code.map_or_else(|| "?".into(), |code| code.to_string())
            ),
            Some(PersistentCommandExit::Terminated) => "Terminado".into(),
            Some(PersistentCommandExit::TimedOut) => "Tiempo agotado".into(),
            Some(PersistentCommandExit::WaitFailed(error)) => format!("Estado incierto: {error}"),
        };
        rows.push(text(format!("{} · {status}\nObservado: {observed_at} · Unix ms {}\nBuffer retenido: {} / {} bytes · {} eventos", if inspected.pty { "PTY" } else { "Tuberías" }, snapshot.observed_at_unix_ms, inspected.retained_bytes, inspected.max_buffered_output_bytes, inspected.events.len()), 12.0));
        if let Some(error) = &inspected.reader_error {
            rows.push(text(format!("Error al leer el proceso: {error}"), 12.0));
        }
        if inspected
            .events
            .iter()
            .any(|event| matches!(event, PersistentCommandEvent::Omitted { .. }))
        {
            rows.push(text(
                "Hay bytes omitidos por el límite del buffer; la salida está incompleta.".into(),
                12.0,
            ));
        }
        if inspected.events.is_empty() {
            rows.push(note("La copia no tiene eventos pendientes. El agente puede haber consumido la salida anterior."));
        }
        rows.push(text(snapshot.details_json.clone(), 12.0).font_family(FontFamily::Monospace));
        rows.push(note("JSON íntegro de los eventos retenidos, en su orden observado. UTF-8 conserva el texto; los demás bytes usan base64. Los controles del terminal aparecen como datos. El índice pertenece sólo a esta copia: no es un cursor ni un historial completo. Los resultados ya entregados por poll_command se consultan en el transcript."));
    }
    let close = Button::new(
        "native_command_output_dismiss",
        "Cerrar",
        theme::component_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .render();
    let refresh = Button::new(
        "native_command_output_refresh",
        "Actualizar copia",
        theme::component_palette(),
    )
    .variant(ButtonVariant::Outline)
    .size(ControlSize::Compact)
    .enabled(!local.loading)
    .render();
    let footer = ButtonGroup::new(vec![refresh, close])
        .align_end()
        .stacked(width < 400.0)
        .render();
    Modal::new(
        base,
        true,
        "Salida pendiente del comando",
        "Consulta sin consumir salida del agente.",
        "native_command_output_dismiss",
        theme::component_palette(),
    )
    .width(width)
    .height(600.0)
    .body(Element::column(rows).gap(10.0).size(Some(body_width), None))
    .footer(footer)
    .initial_focus("native_command_output_dismiss")
    .return_focus(format!(
        "native_command_output_open:{}",
        serde_json::to_string(&(owner, command_id)).expect("return focus serializes")
    ))
    .render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{HitKind, Key, Point, Size, UiAction, UiEvent, UiTree};

    fn output_state() -> (ViewState, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut state = crate::view::tests::empty_state();
        state.workspace = root.path().display().to_string();
        state.main_page = MainPage::Workspace;
        state.show_settings = false;
        state.side_tab = SideTab::Activity;
        state.show_side_pane = true;
        state.selected_session = Some("owner-session".into());
        state.native_command_output = NativeCommandOutputUiState {
            owner_session_id: state.selected_session.clone(),
            command_id: Some("opaque-command-complete-id".into()),
            workspace: Some(root.path().canonicalize().unwrap()),
            snapshot: Some(NativeCommandOutputSnapshot::new(
                PersistentCommandInspection {
                    id: "opaque-command-complete-id".into(),
                    pty: false,
                    events: vec![
                        PersistentCommandEvent::Output {
                            stream: PersistentCommandStream::Stdout,
                            bytes: format!(
                                "{}FIN-SALIDA-ñ-終わり",
                                "Unicode completo y conservado\\n".repeat(100)
                            )
                            .into_bytes(),
                        },
                        PersistentCommandEvent::Output {
                            stream: PersistentCommandStream::Stderr,
                            bytes: vec![255, 0, 27, 65],
                        },
                        PersistentCommandEvent::Omitted { bytes: 12345 },
                    ],
                    exit: Some(PersistentCommandExit::WaitFailed(
                        "Error completo al esperar proceso".into(),
                    )),
                    reader_error: Some("Error completo de reader ñ".into()),
                    retained_bytes: 3200,
                    max_buffered_output_bytes: 4096,
                },
            )),
            ..Default::default()
        };
        (state, root)
    }

    #[test]
    fn pending_output_inspection_json_preserves_exact_binary_text_and_omissions() {
        let (state, _root) = output_state();
        let snapshot = state.native_command_output.snapshot.as_ref().unwrap();
        let events: serde_json::Value = serde_json::from_str(&snapshot.details_json).unwrap();
        assert_eq!(events[0]["encoding"], "utf8");
        let PersistentCommandEvent::Output { bytes, .. } = &snapshot.inspection.events[0] else {
            unreachable!()
        };
        assert_eq!(events[0]["text"].as_str().unwrap().as_bytes(), bytes);
        assert_eq!(events[1]["stream"], "stderr");
        assert_eq!(events[1]["encoding"], "base64");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(events[1]["data"].as_str().unwrap())
                .unwrap(),
            [255, 0, 27, 65]
        );
        assert_eq!(events[2]["omitted_bytes"], 12345);
        assert_eq!(
            events.as_array().unwrap().len(),
            snapshot.inspection.events.len()
        );
    }

    #[test]
    fn pending_output_inspection_modal_scrolls_complete_copy_and_errors_with_safe_actions() {
        for width in [360.0, 900.0] {
            let (state, _root) = output_state();
            let mut tree = UiTree::new(modal(&state, Element::button("behind", "Detrás"), width));
            let start = std::time::Instant::now();
            let frame = tree.layout_at(Size::new(width, 720.0), start);
            assert_eq!(tree.focused_id(), Some("native_command_output_dismiss"));
            for _ in 0..4 {
                tree.handle_event_at(
                    UiEvent::Key {
                        key: Key::Tab,
                        shift: false,
                        ctrl: false,
                    },
                    start,
                );
                assert_ne!(tree.focused_id(), Some("behind"));
            }
            for id in [
                "native_command_output_refresh",
                "native_command_output_dismiss",
            ] {
                let rect = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id && region.kind == HitKind::Button)
                    .unwrap()
                    .rect;
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width);
                assert!(rect.y >= 0.0 && rect.y + rect.height <= 720.0);
                assert_eq!(rect.height, 28.0);
                let point = Point::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
                tree.handle_event_at(UiEvent::PointerDown(point), start);
                assert!(tree.handle_event_at(UiEvent::PointerUp(point), start).iter().any(
                    |action| matches!(action, UiAction::Clicked { id: clicked } if clicked == id)
                ));
            }
            let scroll_id = "modal_body:native_command_output_dismiss";
            let mut sources = std::collections::BTreeSet::new();
            let mut end_seen = false;
            // Model a second of smooth scrolling at 120 Hz independently of
            // CPU speed; optimized builds must observe the same visible text.
            for step in 0..120 {
                let now = start + std::time::Duration::from_secs_f64(step as f64 / 120.0);
                let frame = tree.layout_at(Size::new(width, 720.0), now);
                for block in frame.text_layouts() {
                    sources.insert(block.source.clone());
                    end_seen |= block
                        .runs
                        .iter()
                        .any(|run| run.text.contains("FIN-SALIDA") && run.visible.is_some());
                }
                let body = frame
                    .regions
                    .iter()
                    .find(|region| region.id == scroll_id && region.kind == HitKind::Scroll)
                    .unwrap()
                    .rect;
                tree.handle_event_at(
                    UiEvent::Scroll {
                        position: Point::new(body.x + body.width / 2.0, body.y + body.height / 2.0),
                        delta_y: -300.0,
                    },
                    now,
                );
            }
            let snapshot = state.native_command_output.snapshot.as_ref().unwrap();
            assert!(sources.contains(&snapshot.details_json));
            assert!(
                sources
                    .iter()
                    .any(|source| source.contains("Error completo al esperar proceso"))
            );
            assert!(
                sources
                    .iter()
                    .any(|source| source.contains("Error completo de reader ñ"))
            );
            assert!(end_seen, "final Unicode output unreachable at {width}px");
            assert!(tree.scroll_offset(scroll_id) > 0.0);
            assert_eq!(
                tree.handle_event_at(
                    UiEvent::Key {
                        key: Key::Escape,
                        shift: false,
                        ctrl: false
                    },
                    start + std::time::Duration::from_secs(1)
                ),
                vec![UiAction::Clicked {
                    id: "native_command_output_dismiss".into()
                }]
            );
        }
    }

    #[test]
    fn pending_output_inspection_modal_hides_changed_scope_and_shows_unavailable_without_old_data()
    {
        let (mut state, _root) = output_state();
        state.native_command_output.snapshot = None;
        state.native_command_output.error =
            Some("persistent command session was already drained".into());
        let frame =
            UiTree::new(modal(&state, Element::spacer(), 360.0)).layout(Size::new(360.0, 720.0));
        assert!(
            frame
                .text_layouts()
                .iter()
                .any(|text| text.source.contains("already drained"))
        );
        assert!(
            !frame
                .text_layouts()
                .iter()
                .any(|text| text.source.contains("FIN-SALIDA"))
        );
        state.selected_session = Some("another-owner".into());
        assert!(
            !UiTree::new(modal(&state, Element::spacer(), 360.0))
                .focus("native_command_output_dismiss")
        );
        state.selected_session = state.native_command_output.owner_session_id.clone();
        state.workspace += "/another-workspace";
        assert!(
            !UiTree::new(modal(&state, Element::spacer(), 360.0))
                .focus("native_command_output_dismiss")
        );
    }
}
