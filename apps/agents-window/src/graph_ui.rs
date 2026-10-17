//! The graph tab's elements: the canvas the graph is drawn over and its
//! floating controls (filters, groups, display and forces), and what each
//! control does to the options.

use crate::graph_view::{ColorGroup, GraphOptions, GraphStatus, CANVAS_ID, GROUP_COLORS, SURFACE_ID};
use openide_ui::{
    Button, ButtonVariant, Color, Component, ComponentPalette, ControlSize, Element, FontFamily, Icon,
    IconButton, Point, ScrollArea, SearchField, Slider, Switch, UiAction,
};

const PANEL_WIDTH: f32 = 240.0;
const INSET: f32 = 12.0;
const LABEL_SIZE: f32 = 13.0;

/// Each slider: its key, label, range and whether it moves in whole steps.
const SLIDERS: [(&str, &str, f64, f64, bool); 8] = [
    ("depth", "Profundidad", 1.0, 5.0, true),
    ("text_fade", "Umbral de fundido de texto", -3.0, 3.0, false),
    ("node_size", "Tamaño de nodo", 0.1, 5.0, false),
    ("line_size", "Grosor de línea", 0.1, 5.0, false),
    ("center", "Fuerza de centro", 0.0, 1.0, false),
    ("repel", "Repulsión", 0.0, 20.0, false),
    ("link", "Fuerza de enlace", 0.0, 1.0, false),
    ("distance", "Distancia de enlace", 30.0, 500.0, true),
];

/// The controls float like the window's other cards: the menu palette.
fn palette() -> ComponentPalette {
    crate::theme::menu_palette()
}

fn slider_value(options: &GraphOptions, key: &str) -> f64 {
    match key {
        "depth" => f64::from(options.depth),
        "text_fade" => f64::from(options.text_fade),
        "node_size" => f64::from(options.node_size),
        "line_size" => f64::from(options.line_size),
        "center" => options.center,
        "repel" => options.repel,
        "link" => options.link,
        _ => options.distance,
    }
}

fn set_slider(options: &mut GraphOptions, key: &str, fraction: f32) -> bool {
    let Some(&(_, _, low, high, whole)) = SLIDERS.iter().find(|slider| slider.0 == key) else {
        return false;
    };
    let fraction = if fraction.is_finite() { f64::from(fraction.clamp(0.0, 1.0)) } else { 0.0 };
    let value = low + (high - low) * fraction;
    let value = if whole { value.round() } else { value };
    match key {
        "depth" => options.depth = value as u8,
        "text_fade" => options.text_fade = value as f32,
        "node_size" => options.node_size = value as f32,
        "line_size" => options.line_size = value as f32,
        "center" => options.center = value,
        "repel" => options.repel = value,
        "link" => options.link = value,
        _ => options.distance = value,
    }
    true
}

fn switch_value<'a>(options: &'a mut GraphOptions, key: &str) -> Option<&'a mut bool> {
    Some(match key {
        "imports" => &mut options.imports,
        "calls" => &mut options.calls,
        "uses" => &mut options.uses,
        "orphans" => &mut options.orphans,
        "local" => &mut options.local,
        "incoming" => &mut options.incoming,
        "outgoing" => &mut options.outgoing,
        "neighbours" => &mut options.neighbours,
        "arrows" => &mut options.arrows,
        "folder_colors" => &mut options.folder_colors,
        "filters" => &mut options.filters_open,
        "groups" => &mut options.groups_open,
        "display" => &mut options.display_open,
        "forces" => &mut options.forces_open,
        _ => return None,
    })
}

/// What a control asked for beyond changing the options.
#[derive(Debug, Default, PartialEq)]
pub struct Applied {
    pub timelapse: bool,
}

/// Applies one of the graph controls' actions; `None` when it is not theirs.
pub fn apply(options: &mut GraphOptions, action: &UiAction) -> Option<Applied> {
    match action {
        UiAction::SliderChanged { id, fraction, .. } => {
            let key = id.strip_prefix("graph:slider:")?;
            set_slider(options, key, *fraction).then(Applied::default)
        }
        UiAction::InputChanged { id, value } | UiAction::Submitted { id, value } => {
            if id == "graph:search" {
                options.search = value.clone();
            } else {
                let index: usize = id.strip_prefix("graph:group:")?.parse().ok()?;
                options.groups.get_mut(index)?.query = value.clone();
            }
            Some(Applied::default())
        }
        UiAction::Clicked { id } => {
            let id = id.strip_prefix("graph:")?;
            match id {
                "open" => options.open = true,
                "close" => options.open = false,
                "reset" => options.reset(),
                "search:clear" => options.search.clear(),
                "timelapse" => return Some(Applied { timelapse: true }),
                "group_add" => {
                    let color = GROUP_COLORS[options.groups.len() % GROUP_COLORS.len()];
                    options.groups.push(ColorGroup { query: String::new(), color });
                }
                _ => {
                    if let Some(key) = id.strip_prefix("toggle:").or_else(|| id.strip_prefix("section:")) {
                        let value = switch_value(options, key)?;
                        *value = !*value;
                    } else if let Some(index) = id.strip_prefix("group_remove:") {
                        let index: usize = index.parse().ok()?;
                        (index < options.groups.len()).then(|| options.groups.remove(index))?;
                    } else if let Some(index) = id.strip_prefix("group_color:") {
                        // Each click takes the group to the next color offered.
                        let group = options.groups.get_mut(index.parse::<usize>().ok()?)?;
                        let at = GROUP_COLORS.iter().position(|color| *color == group.color).map_or(0, |at| at + 1);
                        group.color = GROUP_COLORS[at % GROUP_COLORS.len()];
                    } else {
                        return None;
                    }
                }
            }
            Some(Applied::default())
        }
        _ => None,
    }
}

fn label(text: impl Into<String>) -> Element {
    Element::text(text).font_family(FontFamily::Sans).font_size(LABEL_SIZE).foreground(palette().text)
}

fn switch_row(key: &str, text: &str, on: bool) -> Element {
    Element::row(vec![
        label(text).flex(1.0).centered_y(),
        Switch::new(format!("graph:toggle:{key}"), on, palette())
            .accessibility_label(text)
            .render(),
    ])
    .gap(8.0)
    .size(None, Some(30.0))
}

fn slider_row(options: &GraphOptions, key: &str) -> Element {
    let &(_, text, low, high, whole) = SLIDERS.iter().find(|slider| slider.0 == key).expect("a known slider");
    let value = slider_value(options, key);
    let shown = if whole { format!("{value:.0}") } else { format!("{value:.2}") };
    let mut slider = Slider::new(format!("graph:slider:{key}"), ((value - low) / (high - low)) as f32, palette())
        .accessibility_label(text);
    if whole {
        slider = slider.steps((high - low) as usize + 1);
    }
    Element::column(vec![
        label(text),
        Element::row(vec![
            label(shown).foreground(palette().muted).size(Some(36.0), None).centered_y(),
            slider.render().flex(1.0),
        ])
        .gap(6.0),
    ])
    .gap(6.0)
}

fn section(key: &str, title: &str, open: bool, body: Vec<Element>, first: bool) -> Element {
    let mut header = vec![
        Element::clickable(
            format!("graph:section:{key}"),
            Element::row(vec![
                Element::icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }).foreground(palette().muted),
                label(title).foreground(palette().muted),
            ])
            .gap(6.0)
            .centered_y(),
        )
        .flex(1.0),
    ];
    if first {
        header.push(
            IconButton::new("graph:reset", Icon::Refresh, palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .tooltip("Restaurar valores por defecto")
                .render(),
        );
        header.push(
            IconButton::new("graph:close", Icon::Close, palette())
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .tooltip("Cerrar")
                .render(),
        );
    }
    let mut rows = vec![Element::row(header).gap(2.0).size(None, Some(28.0))];
    if open {
        rows.push(Element::column(body).gap(4.0).padding(4.0));
    }
    Element::column(rows).gap(4.0).padding(6.0)
}

fn separator() -> Element {
    Element::spacer().size(None, Some(1.0)).background(palette().border)
}

fn panel(options: &GraphOptions, height: f32) -> Element {
    let mut filters = vec![SearchField::new("graph:search", options.search.clone(), "Buscar archivos…", palette()).render()];
    filters.push(switch_row("local", "Grafo local", options.local));
    if options.local {
        filters.push(slider_row(options, "depth"));
        filters.push(switch_row("incoming", "Enlaces entrantes", options.incoming));
        filters.push(switch_row("outgoing", "Enlaces salientes", options.outgoing));
        filters.push(switch_row("neighbours", "Enlaces entre vecinos", options.neighbours));
    }
    filters.extend([
        switch_row("imports", "Importaciones", options.imports),
        switch_row("calls", "Llamadas", options.calls),
        switch_row("uses", "Usos de tipos", options.uses),
        switch_row("orphans", "Huérfanos", options.orphans),
    ]);
    let mut groups: Vec<Element> = options
        .groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let [r, g, b] = group.color;
            Element::row(vec![
                Element::text_input(format!("graph:group:{index}"), group.query.clone(), "lang:rs, path:src…")
                    .font_size(LABEL_SIZE)
                    .flex(1.0),
                Element::clickable(
                    format!("graph:group_color:{index}"),
                    Element::spacer().size(Some(18.0), Some(18.0)).background(Color::rgb(r, g, b)).radius(9.0),
                )
                .accessibility_label("Cambiar el color del grupo")
                .centered_y(),
                IconButton::new(format!("graph:group_remove:{index}"), Icon::Close, palette())
                    .variant(ButtonVariant::Ghost)
                    .size(ControlSize::Compact)
                    .tooltip("Borrar grupo")
                    .render(),
            ])
            .gap(6.0)
        })
        .collect();
    groups.push(Button::new("graph:group_add", "Nuevo grupo", palette()).size(ControlSize::Compact).variant(ButtonVariant::Secondary).render());
    let display = vec![
        switch_row("folder_colors", "Colores por carpeta", options.folder_colors),
        switch_row("arrows", "Flechas", options.arrows),
        slider_row(options, "text_fade"),
        slider_row(options, "node_size"),
        slider_row(options, "line_size"),
        Element::row(vec![
            Element::spacer().flex(1.0),
            Button::new("graph:timelapse", "Animar", palette()).size(ControlSize::Compact).variant(ButtonVariant::Secondary).render(),
        ]),
    ];
    let forces = ["center", "repel", "link", "distance"].map(|key| slider_row(options, key)).to_vec();
    let body = Element::column(vec![
        section("filters", "Filtros", options.filters_open, filters, true),
        separator(),
        section("groups", "Grupos", options.groups_open, groups, false),
        separator(),
        section("display", "Visualización", options.display_open, display, false),
        separator(),
        section("forces", "Fuerzas", options.forces_open, forces, false),
    ]);
    ScrollArea::new("graph_controls", body)
        .render()
        .size(Some(PANEL_WIDTH), None)
        .max_height((height - 2.0 * INSET).max(80.0))
        .background(palette().raised)
        .border(palette().border, 1.0)
        .radius(openide_ui::tokens().radius.surface)
        .shadow(palette().shadow, Point::new(0.0, 3.0), 10.0, 0.0)
        .clip_children()
}

/// With the panel closed, two loose icons stay in its corner.
fn closed() -> Element {
    Element::column(vec![
        IconButton::new("graph:open", Icon::Settings, palette())
            .variant(ButtonVariant::Ghost)
            .tooltip("Abrir ajustes del grafo")
            .render(),
        IconButton::new("graph:timelapse", Icon::Play, palette())
            .variant(ButtonVariant::Ghost)
            .tooltip("Ver cómo creció el proyecto")
            .render(),
    ])
    .gap(4.0)
}

fn status_text(status: &GraphStatus) -> Option<String> {
    match status {
        GraphStatus::Idle => None,
        GraphStatus::Loading => Some("Indexando el proyecto…".into()),
        GraphStatus::Ready { files, links } => Some(format!("{files} archivos · {links} enlaces")),
        GraphStatus::Failed(error) => Some(format!("No se pudo leer el grafo: {error}")),
    }
}

pub fn build(options: &GraphOptions, status: &GraphStatus, width: f32, height: f32) -> Element {
    let canvas = Element::clickable(CANVAS_ID, Element::surface(SURFACE_ID, "Grafo del proyecto"))
        .focus_ring(false)
        .size(Some(width), Some(height));
    let controls = if options.open { panel(options, height) } else { closed() };
    let mut layers = vec![
        Element::row(vec![Element::spacer().flex(1.0), controls]),
        Element::spacer().flex(1.0),
    ];
    if let Some(text) = status_text(status) {
        layers.push(label(text).font_size(12.0).foreground(palette().muted));
    }
    Element::stack(vec![
        canvas,
        Element::column(layers).padding(INSET).size(Some(width), Some(height)),
    ])
    .size(Some(width), Some(height))
    .clip_children()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(options: &mut GraphOptions, id: &str) -> Option<Applied> {
        apply(options, &UiAction::Clicked { id: id.into() })
    }

    #[test]
    fn the_controls_change_their_options_and_nothing_else() {
        let mut options = GraphOptions::default();
        assert_eq!(click(&mut options, "graph:toggle:orphans"), Some(Applied::default()));
        assert!(!options.orphans);
        click(&mut options, "graph:section:forces");
        assert!(options.forces_open);
        click(&mut options, "graph:close");
        assert!(!options.open);
        assert_eq!(click(&mut options, "graph:timelapse"), Some(Applied { timelapse: true }));
        assert_eq!(click(&mut options, "settings_page:providers"), None);
        assert_eq!(click(&mut options, "graph:toggle:nothing"), None);
    }

    #[test]
    fn sliders_map_their_track_onto_each_range() {
        let mut options = GraphOptions::default();
        let slide = |options: &mut GraphOptions, key: &str, fraction: f32| {
            apply(options, &UiAction::SliderChanged { id: format!("graph:slider:{key}"), fraction, committed: false })
        };
        slide(&mut options, "repel", 0.5);
        assert_eq!(options.repel, 10.0);
        slide(&mut options, "distance", 1.0);
        assert_eq!(options.distance, 500.0);
        slide(&mut options, "depth", 0.6);
        assert_eq!(options.depth, 3);
        slide(&mut options, "text_fade", 0.0);
        assert_eq!(options.text_fade, -3.0);
        assert_eq!(slider_value(&options, "repel"), 10.0);
    }

    #[test]
    fn groups_are_added_edited_recolored_and_removed() {
        let mut options = GraphOptions::default();
        click(&mut options, "graph:group_add");
        click(&mut options, "graph:group_add");
        assert_eq!(options.groups[1].color, GROUP_COLORS[1]);
        apply(&mut options, &UiAction::InputChanged { id: "graph:group:0".into(), value: "lang:rs".into() });
        assert_eq!(options.groups[0].query, "lang:rs");
        click(&mut options, "graph:group_color:0");
        assert_eq!(options.groups[0].color, GROUP_COLORS[1]);
        click(&mut options, "graph:group_remove:0");
        assert_eq!(options.groups.len(), 1);
        assert_eq!(click(&mut options, "graph:group_remove:7"), None);
    }

    #[test]
    fn reset_restores_values_but_keeps_the_panel_as_it_is() {
        let mut options = GraphOptions { arrows: true, repel: 3.0, forces_open: true, ..GraphOptions::default() };
        click(&mut options, "graph:reset");
        assert!(!options.arrows);
        assert_eq!(options.repel, 10.0);
        assert!(options.forces_open);
    }
}
