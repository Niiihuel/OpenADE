//! A `mermaid` flowchart or sequence diagram in an answer, as the native tree
//! paints it with the bundled fonts. Fixtures are local view projections: no
//! engine starts.

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

const LOGIN: &str = "sequenceDiagram
    participant U as Usuario
    participant A as App
    participant S as Servidor
    U->>A: Ingresa sus credenciales
    A->>+S: POST /login
    S-->>-A: 200 OK con el token
    A-->>U: Muestra el panel";

const CHECKOUT: &str = "sequenceDiagram
    autonumber
    actor C as Cliente
    participant W as Tienda web
    participant P as Pagos
    participant D as Depósito
    C->>W: Confirma el carrito
    W->>+P: Cobra el total
    Note right of P: Valida la tarjeta<br/>con el banco
    alt Pago aprobado
        P-->>W: Aprobado
        W->>D: Reserva los productos
        loop Por cada producto
            D->>D: Descuenta del stock
        end
        D-->>W: Reserva confirmada
    else Pago rechazado
        P-->>-W: Rechazado
        W-->>C: Pide otro medio de pago
    end
    opt El cliente dejó su correo
        W-)C: Envía el comprobante
    end";

const AGENT: &str = "sequenceDiagram
    title Una tarea del agente, de punta a punta
    actor U as Usuario
    participant V as Ventana
    participant M as Motor
    participant H as Herramientas
    participant L as Modelo
    U->>V: Escribe la tarea
    V->>+M: Inicia el turno
    Note over V,M: La ventana solo dibuja,<br/>el motor decide
    loop Hasta terminar la tarea
        M->>+L: Envía el contexto
        L-->>-M: Responde con texto o una llamada
        alt Pide una herramienta
            M->>V: Solicita aprobación
            V-->>M: Aprobada
            par Lectura de archivos
                M->>+H: read_file
                H-->>-M: Contenido
            and Búsqueda
                M->>+H: grep
                H-->>-M: Coincidencias
            end
        else Responde al usuario
            M-->>V: Muestra la respuesta
        end
    end
    critical Guardar la sesión
        M->>M: Escribe el registro
    option Disco lleno
        M--xV: Avisa del error
    end
    break Si el usuario cancela
        V-xM: Cancela el turno
    end
    M-->>-V: Turno terminado
    V-->>U: Muestra el resultado";

const EXPORT: &str = "sequenceDiagram
    participant navegador as Navegador del usuario
    participant puerta as Puerta de enlace de la API
    participant cola as Cola de trabajos en segundo plano
    navegador->>puerta: Solicita la exportación completa del historial de conversaciones en formato comprimido
    puerta->>cola: Encola el trabajo
    Note left of navegador: El usuario puede cerrar la pestaña: recibirá un correo cuando termine
    cola-->>navegador: Notifica por correo con el enlace de descarga, válido durante veinticuatro horas
    Note over puerta: Registra la auditoría
    rect rgb(40, 40, 60)
        puerta->>puerta: Limpia los archivos temporales
        activate cola
        cola->>cola: Comprime
        deactivate cola
    end";

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
    for (chart, streaming) in [
        (DOCS, true),
        (LOGIN, true),
        ("stateDiagram-v2\n    [*] --> Activo", false),
        // Inside the syntax of a sequence diagram, outside what is drawn.
        ("sequenceDiagram\n    create participant B\n    A->>B: hola", false),
    ] {
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
fn a_closed_sequence_diagram_is_drawn_switches_to_its_source_and_opens_over_the_window() {
    for viewport in [WIDE, NARROW] {
        let state = conversation(LOGIN, false);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let frame = tree.layout(viewport);
        // Three lifelines and four messages.
        assert_eq!(lines(&frame), 7, "at {}", viewport.width);
        assert!(painted(&frame, "DIAGRAMA") && !painted(&frame, "sequenceDiagram"));
        for text in ["Usuario", "Servidor", "POST /login", "200 OK con el token"] {
            assert!(painted(&frame, text), "{text} at {}", viewport.width);
        }
        let action = |frame: &Frame, expand: bool| {
            frame
                .regions
                .iter()
                .find(|region| region.id.starts_with(DIAGRAM_ACTION_PREFIX) && region.id.contains("expand:") == expand)
                .expect("the diagram carries its actions")
                .id
                .clone()
        };
        let (switch, expand) = (action(&frame, false), action(&frame, true));
        context.end_pass().textures_delta.clear();

        assert!(toggle_diagram_source(&switch));
        let (context, mut tree) = native_tree(root(&state, viewport));
        let source = tree.layout(viewport);
        assert_eq!(lines(&source), 0);
        assert!(painted(&source, "sequenceDiagram") && painted(&source, "Ver diagrama"));
        context.end_pass().textures_delta.clear();
        assert!(toggle_diagram_source(&switch));

        assert!(toggle_diagram_source(&expand));
        let window = crate::rich_text::diagram_viewer(root(&state, viewport), viewport.width, viewport.height);
        let (context, mut tree) = native_tree(window);
        let open = tree.layout(viewport);
        assert_eq!(lines(&open), 14, "the diagram is drawn in the transcript and again over it");
        assert!(open.regions.iter().any(|region| region.id == "chat_diagram_source:close"));
        context.end_pass().textures_delta.clear();
        assert!(toggle_diagram_source("chat_diagram_source:close"));
    }
}

/// How far down a frame paints, to cut a review image to its content.
fn painted_height(frame: &Frame) -> f32 {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            // An outline, not a fill: the page's own fill is as tall as the viewport.
            DrawCommand::StrokeRect { rect, .. } => Some(rect.y + rect.height),
            _ => None,
        })
        .fold(0.0, f32::max)
}

#[test]
#[ignore = "writes PNG/SVG previews of sequence diagrams under /tmp/openade-sequence-diagrams-review"]
fn export_sequence_diagrams_visual_review() {
    let directory = Path::new("/tmp/openade-sequence-diagrams-review");
    std::fs::create_dir_all(directory).unwrap();
    let dark = crate::theme::panel_card_palette();
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
    // Each diagram alone in its card, as wide as it wants, on both palettes.
    for (name, chart) in [("s1-short", LOGIN), ("s2-blocks-notes-numbers", CHECKOUT), ("s3-long-nested", AGENT), ("s4-long-labels", EXPORT)] {
        for (theme, palette, page) in [("dark", dark, crate::theme::BACKGROUND), ("light", light, light.surface)] {
            let card = || {
                Element::panel(Markdown::new(&format!("```mermaid\n{chart}\n```\n"), palette).width(1_000.0).render().element)
                    .padding(24.0)
                    .background(page)
            };
            let (context, mut tree) = native_tree(card());
            let height = painted_height(&tree.layout(Size::new(1_048.0, 4_000.0))) + 36.0;
            context.end_pass().textures_delta.clear();
            let viewport = Size::new(1_048.0, height);
            let (context, mut tree) = native_tree(card());
            export(&tree.layout(viewport), directory, &format!("{name}-{theme}"), viewport);
            context.end_pass().textures_delta.clear();
        }
    }
    // In an answer, at both widths, and over the window.
    for viewport in [WIDE, NARROW] {
        for (name, chart) in [("s5-chat-short", LOGIN), ("s6-chat-blocks", CHECKOUT)] {
            let state = conversation(chart, false);
            let (context, mut tree) = native_tree(root(&state, viewport));
            tree.scroll_to_end("transcript");
            export(&tree.layout(viewport), directory, name, viewport);
            context.end_pass().textures_delta.clear();
        }
        let state = conversation(AGENT, false);
        let (context, mut tree) = native_tree(root(&state, viewport));
        let expand = tree.layout(viewport).regions.iter().find(|region| region.id.contains("expand:")).unwrap().id.clone();
        context.end_pass().textures_delta.clear();
        toggle_diagram_source(&expand);
        let (context, mut tree) = native_tree(crate::rich_text::diagram_viewer(root(&state, viewport), viewport.width, viewport.height));
        tree.set_reduced_motion(true);
        export(&tree.layout(viewport), directory, "s7-expanded", viewport);
        context.end_pass().textures_delta.clear();
        toggle_diagram_source("chat_diagram_source:close");
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
