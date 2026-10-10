//! Native chrome for the conversation-owned browser; the actor owns its page.

use openide_ui::{
    Button, ButtonSize, ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuEntry,
    Element, FontFamily, Icon, IconButton, PopoverPlacement, RasterImage, ScrollArea, Separator,
    Spinner, Tooltip, TooltipPlacement,
};

use crate::{theme, view::ViewState};

pub(crate) const TOOLBAR_HEIGHT: f32 = crate::dock_tab_ui::HEADER_HEIGHT;
pub(crate) const STATUS_HEIGHT: f32 = 20.0;
const NAVIGATION_SIZE: f32 = crate::dock_tab_ui::TOOL_SIZE;
const NAVIGATION_INSET: f32 = (TOOLBAR_HEIGHT - NAVIGATION_SIZE) / 2.0;

#[derive(Default)]
pub(crate) struct BrowserUiState {
    pub tab_id: Option<u64>,
    pub generation: u64,
    pub url_draft: String,
    pub url: String,
    pub title: String,
    pub image: Option<RasterImage>,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub loading: bool,
    pub navigating: bool,
    pub started: bool,
    pub error: Option<String>,
    pub dependency_missing: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub installing: bool,
    pub queued_inputs: usize,
    pub input_rejection: Option<String>,
    pub more_open: bool,
}

fn label(value: impl Into<String>, muted: bool) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .font_size(12.0)
        .foreground(if muted {
            theme::FOREGROUND_SUBTLE
        } else {
            theme::FOREGROUND
        })
}

fn action(id: &str, icon: Icon, tooltip: &str, enabled: bool) -> Element {
    Tooltip::new(
        theme::interactive_surface(
            IconButton::new(id, icon, theme::component_palette())
                .variant(ButtonVariant::Ghost)
                .button_size(ButtonSize::IconExtraSmall)
                .enabled(enabled)
                .render(),
        ),
        id,
        tooltip,
        theme::component_palette(),
    )
    .placement(TooltipPlacement::Below)
    .render()
}

fn menu_action(
    id: &str,
    icon: Icon,
    title: &str,
    description: &str,
    enabled: bool,
) -> DropdownMenuEntry {
    let palette = theme::menu_palette();
    let ink = if enabled { palette.text } else { palette.muted };
    let visual = Element::row(vec![
        Element::icon(icon)
            .foreground(ink)
            .size(Some(16.0), Some(20.0)),
        Element::truncated_text(title)
            .foreground(ink)
            .font_family(FontFamily::Sans)
            .font_size(13.0)
            .flex(1.0)
            .size(None, Some(20.0)),
    ])
    .gap(8.0)
    .clip_children();
    let visual = Element::row(vec![
        Element::spacer().size(Some(5.0), None),
        visual.flex(1.0),
        Element::spacer().size(Some(5.0), None),
    ]);
    let row = if enabled {
        Element::clickable(id, visual).action_menu_item()
    } else {
        Element::panel(visual)
    };
    DropdownMenuEntry::custom(
        Tooltip::new(
            palette
                .interactive_surface(row)
                .padding(3.0)
                .radius(4.0)
                .transition_ms(0)
                .size(None, Some(26.0)),
            id,
            title,
            theme::component_palette(),
        )
        .description(description)
        .placement(TooltipPlacement::Left)
        .max_width(320.0)
        .render(),
    )
}

fn more_menu(browser: &BrowserUiState, width: f32) -> Element {
    let ready = browser.started && !browser.navigating;
    let address = !browser.url_draft.is_empty() || !browser.url.is_empty();
    DropdownMenu::new(
        action(
            "browser_more_toggle",
            Icon::More,
            "Más opciones del navegador",
            true,
        ),
        vec![
            menu_action("browser_copy_url", Icon::File, "Copiar dirección", "Copia la dirección visible en la barra al portapapeles.", address),
            menu_action("browser_snapshot", Icon::Refresh, "Actualizar vista", "Obtiene una captura nueva de la página sin recargarla.", ready),
            DropdownMenuEntry::Separator,
            menu_action("browser_mcp_config", Icon::Code, "Copiar conexión MCP", "Copia la configuración para conectar herramientas a esta página. La conexión existe mientras esta pestaña siga abierta.", ready),
            menu_action("browser_restart", Icon::Refresh, "Reiniciar navegador", "Abre un contexto nuevo y vuelve a la dirección actual. El estado de la página anterior se descarta.", true),
        ],
        browser.more_open,
        "browser_more_dismiss",
        theme::menu_palette(),
    )
    .context_menu_style()
    .width((width - 16.0).clamp(192.0, 252.0))
    .placement(PopoverPlacement::BottomEnd)
    .render()
}

fn navigation(browser: &BrowserUiState, width: f32) -> Element {
    let ready = browser.started && !browser.navigating;
    let address = Tooltip::new(
        Element::text_input("browser_url", &browser.url_draft, "URL o localhost:3000")
            .background(theme::INPUT)
            .foreground(theme::FOREGROUND)
            .border(theme::BORDER, 1.0)
            .focus_border(theme::FOREGROUND_SUBTLE, 1.0)
            .radius(6.0)
            .font_family(FontFamily::Sans)
            .font_size(12.0)
            .padding(4.0)
            .size(None, Some(NAVIGATION_SIZE)),
        "browser_url",
        "Dirección de la página",
        theme::component_palette(),
    )
    .description("Escribí una URL o una dirección local y presioná Enter.")
    .placement(TooltipPlacement::Below)
    .render()
    .flex(1.0);
    Element::row(vec![
        action(
            "browser_back",
            Icon::ArrowLeft,
            "Atrás",
            ready && browser.can_go_back,
        ),
        action(
            "browser_forward",
            Icon::ArrowRight,
            "Adelante",
            ready && browser.can_go_forward,
        ),
        action("browser_reload", Icon::Refresh, "Recargar página", ready),
        address,
        more_menu(browser, width),
    ])
    .gap(4.0)
    .padding(NAVIGATION_INSET)
    .size(Some(width), Some(TOOLBAR_HEIGHT))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Surface {
    Page,
    Empty,
    Loading,
    Error,
}

fn surface(browser: &BrowserUiState) -> Surface {
    let chrome_error = browser.url.starts_with("chrome-error:")
        || browser.url.starts_with("chrome://network-error");
    let load_error = browser.error.as_deref().is_some_and(|error| {
        !browser.started
            || browser.dependency_missing
            || error.contains("net::ERR_")
            || error.contains("page.goto:")
            || error.contains("browserType.launch:")
    });
    if chrome_error || load_error || (browser.error.is_some() && browser.image.is_none()) {
        Surface::Error
    } else if browser.navigating || (browser.loading && browser.image.is_none()) {
        Surface::Loading
    } else if browser.url.is_empty() || browser.url == "about:blank" {
        Surface::Empty
    } else if browser.image.is_some() {
        Surface::Page
    } else if browser.started {
        Surface::Loading
    } else {
        Surface::Empty
    }
}

fn address_host(browser: &BrowserUiState) -> Option<String> {
    let address = if browser.url_draft.is_empty() {
        &browser.url
    } else {
        &browser.url_draft
    };
    let url = reqwest::Url::parse(address).ok()?;
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn local_address(browser: &BrowserUiState) -> bool {
    let address = if browser.url_draft.is_empty() {
        &browser.url
    } else {
        &browser.url_draft
    };
    reqwest::Url::parse(address)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            host == "localhost"
                || host.ends_with(".localhost")
                || host == "[::1]"
                || host == "::1"
                || host.starts_with("127.")
        })
}

fn error_copy(browser: &BrowserUiState) -> (String, String) {
    let error = browser.error.as_deref().unwrap_or("");
    if browser.dependency_missing {
        (
            "Prepará el navegador".into(),
            "Instalá el motor del navegador para abrir páginas en esta conversación.".into(),
        )
    } else if error.contains("ERR_CONNECTION_REFUSED") && local_address(browser) {
        (format!("No se pudo conectar con {}", address_host(browser).unwrap_or_else(|| "localhost".into())),
        "El equipo rechazó la conexión. Verificá que el servidor del proyecto esté iniciado y que use ese puerto.".into())
    } else if error.contains("ERR_CONNECTION_REFUSED") {
        (
            "El sitio rechazó la conexión".into(),
            "Revisá la dirección e intentá nuevamente cuando el sitio esté disponible.".into(),
        )
    } else if error.contains("ERR_NAME_NOT_RESOLVED") {
        (
            "No se encontró este sitio".into(),
            "Revisá el nombre de la dirección y la conexión a internet.".into(),
        )
    } else if error.contains("ERR_CERT_") || error.contains("ERR_SSL_") {
        ("No se pudo verificar la conexión".into(), "El sitio tiene un problema con su certificado. Revisá la dirección o la configuración HTTPS del servidor.".into())
    } else if error.contains("error while loading shared libraries") {
        (
            "El navegador no pudo iniciarse".into(),
            "Faltan bibliotecas del sistema. Usá un Chromium instalado en este equipo.".into(),
        )
    } else if !browser.started {
        (
            "El navegador no pudo iniciarse".into(),
            "Volvé a intentarlo para abrir una nueva instancia de esta conversación.".into(),
        )
    } else {
        (
            "No se pudo abrir esta página".into(),
            "Revisá la dirección y la conexión, y volvé a intentarlo.".into(),
        )
    }
}

fn state_surface(browser: &BrowserUiState, width: f32, mode: Surface) -> Element {
    let card_width = (width - 48.0).clamp(96.0, 360.0);
    let (title, description, icon) = match mode {
        Surface::Error => {
            let (title, description) = error_copy(browser);
            (title, description, Icon::WarningTriangle)
        }
        Surface::Loading => (
            if browser.started {
                "Abriendo página…"
            } else {
                "Iniciando navegador…"
            }
            .into(),
            if browser.started {
                "Esperando la respuesta del sitio."
            } else {
                "Preparando una sesión aislada para esta conversación."
            }
            .into(),
            Icon::Globe,
        ),
        _ => (
            "Una página para tu proyecto".into(),
            "Escribí una dirección web o local en la barra para comenzar.".into(),
            Icon::Globe,
        ),
    };
    let visual = if mode == Surface::Loading {
        Spinner::new("browser_start_progress", theme::FOREGROUND_SUBTLE)
            .size(20.0)
            .render()
    } else {
        Element::panel(Element::icon(icon).foreground(theme::FOREGROUND_SUBTLE))
            .background(theme::CARD)
            .radius(8.0)
            .size(Some(36.0), Some(36.0))
    };
    let mut content = vec![
        visual.centered_x(),
        label(title, false).font_size(15.0).center_content(),
        label(description, true).center_content(),
    ];
    if mode == Surface::Error {
        if let Some(error) = &browser.error {
            content.push(
                Tooltip::new(
                    label("Ver detalle del error", true)
                        .font_size(11.0)
                        .center_content(),
                    "browser_error_detail",
                    "Detalle del navegador",
                    theme::component_palette(),
                )
                .description(error.chars().take(640).collect::<String>())
                .max_width(360.0)
                .render(),
            );
        }
        let (action_id, action_label) = if browser.dependency_missing {
            (
                "browser_install",
                if browser.installing {
                    "Instalando…"
                } else {
                    "Instalar navegador"
                },
            )
        } else if browser.started {
            ("browser_navigate", "Reintentar")
        } else {
            ("browser_restart", "Reintentar")
        };
        let retry = Button::new(action_id, action_label, theme::component_palette())
            .variant(ButtonVariant::Secondary)
            .enabled(!browser.installing)
            .loading(browser.installing)
            .size(ControlSize::Compact)
            .render();
        let edit = Button::new(
            "browser_focus_url",
            "Editar dirección",
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Compact)
        .render();
        let actions = if card_width < 260.0 {
            Element::column(vec![retry, edit]).gap(4.0)
        } else {
            Element::row(vec![retry, edit]).gap(4.0).centered_x()
        };
        content.push(actions);
    } else if mode == Surface::Empty {
        content.push(
            Button::new(
                "browser_focus_url",
                "Escribir dirección",
                theme::component_palette(),
            )
            .variant(ButtonVariant::Secondary)
            .size(ControlSize::Compact)
            .render()
            .centered_x(),
        );
    }
    let card = Element::column(content)
        .gap(10.0)
        .size(Some(card_width), None);
    ScrollArea::new(
        "browser_state_scroll",
        Element::column(vec![
            Element::spacer().size(None, Some(32.0)),
            card.centered_x(),
            Element::spacer().size(None, Some(32.0)),
        ])
        .size(Some(width), None),
    )
    .render()
    .flex(1.0)
}

fn status(browser: &BrowserUiState, width: f32, mode: Surface) -> Element {
    let error = browser.input_rejection.as_deref().or_else(|| {
        (mode == Surface::Page)
            .then_some(browser.error.as_deref())
            .flatten()
    });
    let text = if let Some(error) = error {
        error
            .lines()
            .next()
            .unwrap_or(error)
            .chars()
            .take(160)
            .collect()
    } else if browser.queued_inputs > 0 {
        format!("{} entradas pendientes", browser.queued_inputs)
    } else if browser.navigating {
        "Cargando página…".into()
    } else if browser.loading {
        "Actualizando vista…".into()
    } else {
        "Sesión aislada · Compartida con el agente".into()
    };
    let row = Element::truncated_text(text)
        .font_family(FontFamily::Sans)
        .font_size(10.5)
        .foreground(theme::FOREGROUND_SUBTLE)
        .padding(3.0)
        .size(Some(width), Some(STATUS_HEIGHT));
    Tooltip::new(
        row,
        "browser_status",
        if error.is_some() {
            "Entrada del navegador"
        } else {
            "Navegador de esta conversación"
        },
        theme::component_palette(),
    )
    .description(
        error
            .unwrap_or(
                "Esta página pertenece a la conversación. No usa tu perfil personal del navegador.",
            )
            .chars()
            .take(640)
            .collect::<String>(),
    )
    .max_width(360.0)
    .render()
}

pub(crate) fn build(state: &ViewState, width: f32) -> Element {
    let browser = &state.browser;
    let width = width.max(160.0);
    let mode = surface(browser);
    let content = if mode == Surface::Page {
        // The host resizes Chromium to this retained region. Painting and
        // coordinate conversion share these bounds, with no second scrollbar.
        Element::clickable(
            "browser_viewport",
            Element::raster_image(browser.image.as_ref().unwrap().clone()),
        )
        .focus_ring(false)
        .clip_children()
        .flex(1.0)
    } else {
        state_surface(browser, width, mode)
    };
    Element::column(vec![
        navigation(browser, width),
        Separator::horizontal(theme::component_palette()).render(),
        content,
        status(browser, width, mode),
    ])
    .size(Some(width), None)
    .clip_children()
}

#[cfg(test)]
mod tests {
    use super::*;
    use openide_ui::{DrawCommand, HitKind, Point, Size, UiTree};

    #[test]
    fn navigation_icon_has_a_centered_glyph_and_disabled_controls_stay_passive() {
        for enabled in [false, true] {
            let mut tree = UiTree::new(action("browser_back", Icon::ArrowLeft, "Atrás", enabled));
            let frame = tree.layout(Size::new(NAVIGATION_SIZE, NAVIGATION_SIZE));
            assert_eq!(tree.focus("browser_back"), enabled);
            assert_eq!(
                frame
                    .regions
                    .iter()
                    .any(|region| region.kind == HitKind::Button),
                enabled,
            );
            if enabled {
                let region = frame
                    .regions
                    .iter()
                    .find(|region| region.id == "browser_back")
                    .unwrap();
                assert_eq!(region.rect.width, NAVIGATION_SIZE);
                assert_eq!(region.rect.height, NAVIGATION_SIZE);
                let icon = frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::Icon {
                            rect,
                            icon: Icon::ArrowLeft,
                            ..
                        } => Some(rect),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(icon.width, 16.0);
                assert_eq!(icon.height, 16.0);
                let center = NAVIGATION_SIZE / 2.0;
                assert!((icon.x + icon.width / 2.0 - center).abs() < 0.01);
                assert!((icon.y + icon.height / 2.0 - center).abs() < 0.01);
                assert_eq!(
                    frame.hit_test(Point::new(center, center)),
                    Some("browser_back")
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "browser_ui_tests.rs"]
mod surface_tests;

#[cfg(test)]
#[path = "browser_visual_tests.rs"]
mod visual_tests;
