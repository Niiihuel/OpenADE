//! A `mermaid` flowchart in an answer, as the native tree paints it with the
//! bundled fonts. Fixtures are local view projections: no engine starts.

use super::plan_visual_tests::{export, fixture, native_tree, root};
use crate::{
    rich_text::{toggle_diagram_source, DIAGRAM_ACTION_PREFIX},
    transcript::TranscriptItem,
    view::ViewState,
};
use openide_ui::{
    Color, Component, ComponentPalette, DrawCommand, Element, Flowchart, Frame, Markdown, Size, Text,
};
use std::path::Path;

const SHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/ui-separation/shots/diagrams");
const WIDE: Size = Size::new(1100.0, 1000.0);
const NARROW: Size = Size::new(520.0, 1000.0);

const DEMO: &str = "flowchart TD
    A[Se inicializa la demo] --> B[workspace-preview.js prepara la experiencia]
    B --> C[workspace-dock.js gestiona paneles y pestañas]
    C --> D[Archivos, vistas y terminales de ejemplo]
    A --> E[Recorrido guiado automático o interacción manual]
    E --> F[El usuario explora una solicitud preparada]
    F --> G[Recorre planificación, aprobaciones y resultado de ejemplo]
    D --> H[El usuario cambia archivos o vistas del ejemplo]
    G --> I[La experiencia muestra un resultado simulado]
    H --> I";

const DOCS: &str = "flowchart LR
    A[Contenido en docs/sections/] --> B[docs/content.js organiza guías y navegación]
    B --> C[scripts/build-docs.mjs]
    C --> D[Genera páginas HTML estáticas]
    C --> E[Genera índice de búsqueda local]
    D --> F[Salida dentro de dist/]
    E --> F
    F --> G[El navegador muestra la documentación]";

const DECISION: &str = "flowchart TD
    start([Llega una solicitud]) --> check{¿Requiere aprobación?}
    check -->|Sí| ask[Pide permiso al usuario]
    check -->|No| run[Ejecuta la herramienta]
    ask --> answer{¿Aprobada?}
    answer -->|Sí| run
    answer -->|No| stop((Fin))
    run -.-> log(Registra el resultado)
    log ==> stop";

const PIPELINE: &str = "flowchart LR
    push[Push a main] --> lint[Lint y formato] --> unit[Pruebas unitarias] --> build[Compilación de escritorio]
    build --> sign[Firma del binario] --> pack[Empaquetado por plataforma] --> smoke[Prueba de humo]
    smoke --> publish[Publicación de la versión] --> notify[Aviso al equipo]";

fn answer(chart: &str, closed: bool) -> String {
    format!("El flujo queda así:\n\n```mermaid\n{chart}{}", if closed { "\n```\n\nCada paso depende del anterior." } else { "" })
}

fn conversation(chart: &str, streaming: bool) -> ViewState {
    let mut state = fixture(false);
    state.selected_backend = "native".into();
    state.prompt.clear();
    state.transcript = vec![
        TranscriptItem::User { content: "¿Cómo fluye esto?".into(), images: vec![] },
        TranscriptItem::Assistant { content: answer(chart, !streaming), streaming },
    ];
    state
}

fn lines(frame: &Frame) -> usize {
    frame.commands.iter().filter(|command| matches!(command, DrawCommand::Polyline { .. })).count()
}

fn painted(frame: &Frame, value: &str) -> bool {
    frame.commands.iter().any(|command| matches!(command, DrawCommand::Text { text, .. } if text.contains(value)))
}

#[test]
fn a_closed_flowchart_is_drawn_and_switches_to_its_source_and_back() {
    for viewport in [WIDE, NARROW] {
        let state = conversation(DOCS, false);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        assert_eq!(lines(&frame), 7, "one connector per edge at {}", viewport.width);
        assert!(painted(&frame, "DIAGRAMA") && !painted(&frame, "flowchart LR"));
        let switch = frame
            .regions
            .iter()
            .find(|region| region.id.starts_with(DIAGRAM_ACTION_PREFIX) && !region.id.contains("expand:"))
            .expect("the diagram carries its switch")
            .clone();
        assert!(switch.rect.x >= 0.0 && switch.rect.x + switch.rect.width <= viewport.width);
        context.end_pass().textures_delta.clear();

        assert!(toggle_diagram_source(&switch.id));
        let (context, mut tree) = native_tree(root(&state, viewport));
        let source = tree.layout(viewport);
        assert_eq!(lines(&source), 0);
        assert!(painted(&source, "flowchart LR") && painted(&source, "Ver diagrama"));
        context.end_pass().textures_delta.clear();
        assert!(toggle_diagram_source(&switch.id));
        assert!(!toggle_diagram_source("chat_link_open:0:0:https://example.test/"));
    }
}

#[test]
fn a_flowchart_opens_over_the_window_and_closes_again() {
    let state = conversation(DOCS, false);
    let (context, mut tree) = native_tree(root(&state, NARROW));
    let frame = tree.layout(NARROW);
    let expand = frame.regions.iter().find(|region| region.id.contains("expand:")).expect("Ampliar").id.clone();
    context.end_pass().textures_delta.clear();
    assert!(toggle_diagram_source(&expand));
    // The window wraps its content in the viewer, as `build_window_view` does.
    let window = |state| crate::rich_text::diagram_viewer(root(state, NARROW), NARROW.width, NARROW.height);
    let (context, mut tree) = native_tree(window(&state));
    let open = tree.layout(NARROW);
    assert_eq!(lines(&open), 14, "the chart is drawn in the transcript and again over it");
    let close = open
        .regions
        .iter()
        .rev()
        .find(|region| region.id == "chat_diagram_source:close" && region.kind == openide_ui::HitKind::Button)
        .expect("the viewer has a close control")
        .clone();
    assert!(close.rect.x + close.rect.width <= NARROW.width && close.rect.y >= 0.0);
    context.end_pass().textures_delta.clear();
    assert!(toggle_diagram_source(&close.id));
    let (context, mut tree) = native_tree(window(&state));
    assert_eq!(lines(&tree.layout(NARROW)), 7);
    context.end_pass().textures_delta.clear();
}

#[test]
fn a_flowchart_still_being_written_or_of_another_kind_stays_a_code_block() {
    for (chart, streaming) in [(DOCS, true), ("sequenceDiagram\n    A->>B: hola", false)] {
        let state = conversation(chart, streaming);
        let (context, mut tree) = native_tree(root(&state, WIDE));
        let frame = tree.layout(WIDE);
        assert_eq!(lines(&frame), 0);
        assert!(painted(&frame, "MERMAID") && !painted(&frame, "DIAGRAMA"));
        assert!(!frame.regions.iter().any(|region| region.id.starts_with(DIAGRAM_ACTION_PREFIX)));
        context.end_pass().textures_delta.clear();
    }
}

#[test]
#[ignore = "writes PNG/SVG previews under docs/ui-separation/shots/diagrams"]
fn export_chat_diagrams_visual_review() {
    let directory = Path::new(SHOTS);
    std::fs::create_dir_all(directory).unwrap();
    for viewport in [WIDE, NARROW] {
        for (name, chart) in [
            ("d1-down-branch-merge", DEMO),
            ("d2-right", DOCS),
            ("d3-decision-labels", DECISION),
            ("d4-wide-scrolls", PIPELINE),
        ] {
            let state = conversation(chart, false);
            let (context, mut tree) = native_tree(root(&state, viewport));
            export(&tree.layout(viewport), directory, name, viewport);
            context.end_pass().textures_delta.clear();
        }
        let state = conversation(DOCS, true);
        let (context, mut tree) = native_tree(root(&state, viewport));
        export(&tree.layout(viewport), directory, "d5-streaming-code", viewport);
        context.end_pass().textures_delta.clear();

        let state = conversation(DOCS, false);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let expand = tree.layout(viewport).regions.iter().find(|region| region.id.contains("expand:")).unwrap().id.clone();
        context.end_pass().textures_delta.clear();
        toggle_diagram_source(&expand);
        let (context, mut tree) = native_tree(crate::rich_text::diagram_viewer(root(&state, viewport), viewport.width, viewport.height));
        tree.set_reduced_motion(true);
        export(&tree.layout(viewport), directory, "d7-expanded", viewport);
        context.end_pass().textures_delta.clear();
        toggle_diagram_source("chat_diagram_source:close");

        // The app ships one dark scheme; the same component on a light
        // palette shows that no ink is the diagram's own.
        let light = ComponentPalette {
            surface: Color::rgb(255, 255, 255),
            raised: Color::rgb(246, 246, 244),
            hover: Color::rgb(236, 236, 233),
            selected: Color::rgb(226, 226, 222),
            border: Color::rgb(214, 214, 209),
            text: Color::rgb(28, 28, 26),
            muted: Color::rgb(110, 110, 104),
            shadow: Color::rgba(0, 0, 0, 40),
        };
        let page = Element::panel(
            Element::column(vec![
                Text::new("Paleta clara", light).render(),
                Markdown::new(&answer(DECISION, true), light).width(viewport.width - 48.0).render().element,
                Flowchart::parse(DOCS, light).unwrap().backdrop(light.surface).scrollable("light"),
            ])
            .gap(16.0),
        )
        .padding(24.0)
        .background(light.surface);
        let (context, mut tree) = native_tree(page);
        export(&tree.layout(viewport), directory, "d6-light-palette", viewport);
        context.end_pass().textures_delta.clear();
    }
}
