//! Native chrome for the conversation-owned browser; the actor owns its page.

use openide_ui::{
    Button, ButtonVariant, Component, ControlSize, DropdownMenu, DropdownMenuEntry,
    DropdownMenuItem, Element,
    FontFamily, Icon, IconButton, PopoverPlacement, ScrollArea, Separator, Spinner,
    Text, TextRole, Tooltip, TooltipPlacement, TooltipSpec,
};

use crate::{theme, view::ViewState};

pub(crate) const TOOLBAR_HEIGHT: f32 = crate::dock_tab_ui::HEADER_HEIGHT;
pub(crate) const STATUS_HEIGHT: f32 = 20.0;
const NAVIGATION_SIZE: f32 = crate::dock_tab_ui::TOOL_SIZE;
// The inset of `ui_sizing::PANE_HEADER`, which the toolbar is built with.
#[cfg(test)]
const NAVIGATION_INSET: f32 = (TOOLBAR_HEIGHT - NAVIGATION_SIZE) / 2.0;

#[derive(Clone, Default, PartialEq)]
pub(crate) struct BrowserUiState {
    pub tab_id: Option<u64>,
    pub generation: u64,
    pub url_draft: String,
    pub url: String,
    pub title: String,
    /// The tab's live surface holds a frame of this page.
    pub live: bool,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub loading: bool,
    pub navigating: bool,
    pub started: bool,
    pub error: Option<String>,
    /// The error is a page that did not load, whatever its text says.
    pub load_failed: bool,
    /// The embedded Chromium cannot start in this run of the application:
    /// opening the context again would fail the same way.
    pub engine_failed: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub queued_inputs: usize,
    pub input_rejection: Option<String>,
    pub more_open: bool,
}

/// The live surface a browser tab's frames are presented on.
pub(crate) fn surface_id(tab: u64) -> String {
    format!("browser:{tab}")
}

fn menu_action(
    id: &str,
    icon: Icon,
    title: &str,
    description: &str,
    enabled: bool,
) -> DropdownMenuEntry {
    DropdownMenuItem::new(id, title)
        .icon(icon)
        .enabled(enabled)
        .truncate()
        .tooltip_with(
            TooltipSpec::new(title)
                .description(description)
                .placement(TooltipPlacement::Left)
                .max_width(320.0)
                .palette(theme::component_palette()),
        )
        .into()
}

fn more_menu(browser: &BrowserUiState, width: f32) -> Element {
    let ready = browser.started && !browser.navigating;
    let address = !browser.url_draft.is_empty() || !browser.url.is_empty();
    DropdownMenu::new(
        IconButton::new("browser_more_toggle", Icon::More, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .enabled(true)
            .tooltip_with(TooltipSpec::new("Más opciones del navegador").placement(TooltipPlacement::Below))
            .render(),
        vec![
            menu_action("browser_copy_url", Icon::File, "Copiar dirección", "Copia la dirección visible en la barra al portapapeles.", address),
            menu_action("browser_snapshot", Icon::Refresh, "Actualizar vista", "Vuelve a pedir la imagen en vivo de la página, sin recargarla.", ready),
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
    // Back, forward and reload stand before the address, which takes the
    // width they and the menu leave.
    let leading = Element::row(vec![
        IconButton::new("browser_back", Icon::ArrowLeft, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .enabled(ready && browser.can_go_back)
            .tooltip_with(TooltipSpec::new("Atrás").placement(TooltipPlacement::Below))
            .render(),
        IconButton::new(
            "browser_forward",
            Icon::ArrowRight,
            theme::component_palette(),
        )
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Dense)
        .enabled(ready && browser.can_go_forward)
        .tooltip_with(TooltipSpec::new("Adelante").placement(TooltipPlacement::Below))
        .render(),
        IconButton::new("browser_reload", Icon::Refresh, theme::component_palette())
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Dense)
            .enabled(ready)
            .tooltip_with(TooltipSpec::new("Recargar página").placement(TooltipPlacement::Below))
            .render(),
        address,
    ])
    .gap(4.0);
    openide_ui::PanelHeader::new("", theme::component_palette())
        .leading(leading)
        .trailing(vec![more_menu(browser, width)])
        .width(width)
        .sizing(crate::ui_sizing::PANE_HEADER)
        .render()
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
            || browser.load_failed
            || error.contains("net::ERR_")
    });
    if chrome_error || load_error || (browser.error.is_some() && !browser.live) {
        Surface::Error
    } else if browser.navigating || (browser.loading && !browser.live) {
        Surface::Loading
    } else if browser.url.is_empty() || browser.url == "about:blank" {
        Surface::Empty
    } else if browser.live {
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
    crate::chat_links::is_local(address)
}

fn error_copy(browser: &BrowserUiState) -> (String, String) {
    let error = browser.error.as_deref().unwrap_or("");
    if browser.engine_failed {
        // Said plainly, with the reason: there is no other browser to use.
        ("El navegador embebido no pudo iniciarse".into(), error.chars().take(640).collect())
    } else if error.contains("ERR_CONNECTION_REFUSED") && local_address(browser) {
        // Nobody listens there: the project's server stopped, or never started.
        (format!("No se pudo conectar con {} — ¿sigue corriendo el servidor?", address_host(browser).unwrap_or_else(|| "localhost".into())),
        "Nada responde en ese puerto de este equipo. Iniciá el servidor del proyecto, o revisá el puerto, y reintentá.".into())
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
        theme::text_px(title, 15.0, theme::FOREGROUND).center_content(),
        Text::new(description, theme::component_palette())
            .muted()
            .render()
            .center_content(),
    ];
    // The reason is already on the card, and Chromium starts once per run:
    // there is no detail to add and nothing a retry could change.
    if mode == Surface::Error && !browser.engine_failed {
        if let Some(error) = &browser.error {
            content.push(
                Tooltip::new(
                    Text::new("Ver detalle del error", theme::component_palette())
                        .role(TextRole::Small)
                        .muted()
                        .render()
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
        // A context that never started is opened again; one that did only
        // reopens the address.
        let action_id = if browser.started {
            "browser_navigate"
        } else {
            "browser_restart"
        };
        let retry = Button::new(action_id, "Reintentar", theme::component_palette())
            .variant(ButtonVariant::Secondary)
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
    let row = Text::new(text, theme::component_palette())
        .size(10.5)
        .muted()
        .truncate()
        .render()
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
        // The host resizes Chromium to this retained region. Its frames are
        // presented on the surface as they arrive, one pixel per physical
        // pixel from this corner, with no second scrollbar.
        Element::clickable(
            "browser_viewport",
            Element::surface(
                surface_id(browser.tab_id.unwrap_or_default()),
                "Vista del navegador del proyecto",
            ),
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
            let mut tree = UiTree::new(
                IconButton::new("browser_back", Icon::ArrowLeft, theme::component_palette())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Dense)
                    .enabled(enabled)
                    .tooltip_with(TooltipSpec::new("Atrás").placement(TooltipPlacement::Below))
                    .render(),
            );
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
