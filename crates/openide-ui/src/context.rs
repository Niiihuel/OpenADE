//! Composable context usage presentation; all measurements belong to the host.

use crate::{Color, Component, ComponentPalette, Element, FontFamily};

fn context_text(value: impl Into<String>, color: Color, size: f32) -> Element {
    Element::text(value)
        .font_family(FontFamily::Sans)
        .foreground(color)
        .font_size(size)
}

fn measured_fraction(value: Option<f32>) -> Option<f32> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

fn percent_label(fraction: f32) -> String {
    if fraction > 0.0 && fraction < 0.01 {
        "<1%".into()
    } else {
        format!("{:.0}%", fraction * 100.0)
    }
}

fn usage_color(fraction: Option<f32>, palette: ComponentPalette) -> Color {
    match fraction {
        Some(value) if value >= 1.0 => Color::rgb(235, 115, 119),
        Some(value) if value >= 0.8 => Color::rgb(225, 169, 65),
        Some(_) => palette.text,
        None => palette.muted,
    }
}

fn section_rule(palette: ComponentPalette) -> Element {
    Element::spacer()
        .size(None, Some(1.0))
        .background(palette.border)
}

fn right_value(value: &str, palette: ComponentPalette) -> Element {
    // The outer flex slot bounds the column. A short intrinsic value follows
    // the spacer to the right; a long value keeps its origin at the slot's
    // left edge and fades at that same clipped right edge.
    Element::row(vec![
        Element::spacer().flex(1.0),
        Element::faded_text(value, 14.0)
            .font_family(FontFamily::Sans)
            .foreground(palette.muted)
            .font_size(12.0),
    ])
    .clip_children()
    .flex(1.0)
}

fn compact_label(value: &str, palette: ComponentPalette) -> Element {
    Element::faded_text(value, 12.0)
        .font_family(FontFamily::Sans)
        .foreground(palette.text)
        .font_size(12.0)
        .flex(1.0)
}

/// A measured portion of the context window. Fractions and labels come from
/// the host; metadata such as document bytes must never create token segments.
pub struct ContextSegment {
    pub fraction: f32,
    pub color: Color,
}

/// The original colored breakdown row remains source-compatible.
pub struct ContextUsageRow {
    pub label: String,
    pub value: String,
    pub color: Color,
}

/// A naturally sized context card. The host controls popover width and its
/// maximum scrolling height; the card never reserves a fixed-height viewport.
pub struct ContextUsage {
    title: String,
    subtitle: String,
    model: String,
    summary: String,
    total: String,
    fraction: Option<f32>,
    caption: String,
    note: String,
    segments: Vec<ContextSegment>,
    rows: Vec<ContextUsageRow>,
    sections: Vec<(String, Vec<ContextRow>)>,
    close_action: String,
    details_action: String,
    scroll_id: Option<String>,
    palette: ComponentPalette,
}

impl ContextUsage {
    pub fn new(title: impl Into<String>, palette: ComponentPalette) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            model: String::new(),
            summary: "Sin medición".into(),
            total: String::new(),
            fraction: None,
            caption: String::new(),
            note: String::new(),
            segments: Vec::new(),
            rows: Vec::new(),
            sections: Vec::new(),
            close_action: String::new(),
            details_action: String::new(),
            scroll_id: None,
            palette,
        }
    }
    /// Preserve caller-authored wording and units without interpreting them.
    pub fn summary(mut self, summary: impl Into<String>, total: impl Into<String>) -> Self {
        self.summary = summary.into();
        self.total = total.into();
        self
    }
    /// Set an exact measured fraction. Unknown is never rendered as zero.
    /// Explicit segments, when supplied, still own their measured breakdown.
    pub fn usage(mut self, fraction: Option<f32>, total: impl Into<String>) -> Self {
        self.fraction = measured_fraction(fraction);
        self.summary = self.fraction.map_or_else(
            || "Sin medición".into(),
            |fraction| format!("{} usado", percent_label(fraction)),
        );
        self.total = total.into();
        self
    }
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = caption.into();
        self
    }
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }
    pub fn segments(mut self, segments: Vec<ContextSegment>) -> Self {
        self.segments = segments;
        self
    }
    pub fn rows(mut self, rows: Vec<ContextUsageRow>) -> Self {
        self.rows = rows;
        self
    }
    /// Categories retain their own units. Descriptions are available on hover
    /// without turning the compact summary into a series of long paragraphs.
    pub fn section(mut self, title: impl Into<String>, rows: Vec<ContextRow>) -> Self {
        self.sections.push((title.into(), rows));
        self
    }
    pub fn actions(mut self, close: impl Into<String>, details: impl Into<String>) -> Self {
        self.close_action = close.into();
        self.details_action = details.into();
        self
    }
    /// Keep the header and details action fixed when the containing popover
    /// clamps this naturally sized card to a shorter viewport.
    pub fn scroll_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.scroll_id = (!id.is_empty()).then_some(id);
        self
    }
}

impl Component for ContextUsage {
    fn render(self) -> Element {
        let palette = self.palette;
        let scope = if self.close_action.is_empty() {
            "context_usage".to_owned()
        } else {
            self.close_action.clone()
        };
        let heading = if self.subtitle.is_empty() {
            // A column places its only child at the top. A single text line
            // uses the full header row and shares the close button's center.
            Element::faded_text(self.title, 12.0)
                .font_family(FontFamily::Sans)
                .foreground(palette.text)
                .font_size(12.0)
                .flex(1.0)
        } else {
            Element::column(vec![
                context_text(self.title, palette.text, 12.0),
                context_text(self.subtitle, palette.muted, 11.0),
            ])
            .gap(3.0)
            .flex(1.0)
        };
        let mut header = vec![heading];
        if !self.close_action.is_empty() {
            header.push(
                crate::IconButton::new(self.close_action, crate::Icon::Close, palette)
                    .variant(crate::ButtonVariant::Ghost)
                    .button_size(crate::ButtonSize::IconExtraSmall)
                    .tooltip("Cerrar contexto")
                    .tooltip_shortcut("Esc")
                    .render(),
            );
        }
        let mut content = vec![Element::row(header).gap(6.0).min_size(0.0, 24.0)];
        if !self.model.is_empty() {
            content.push(compact_note(
                self.model,
                format!("{scope}:model"),
                "Modelo",
                palette,
            ));
        }
        let ink = usage_color(self.fraction, palette);
        let mut overview = vec![context_text(self.summary, ink, 20.0)];
        if !self.total.is_empty() {
            overview.push(context_text(self.total, palette.muted, 12.0));
        }
        let mut remaining = 1.0_f32;
        let mut bar = Vec::new();
        let segments = if self.segments.is_empty() {
            self.fraction
                .map(|fraction| ContextSegment {
                    fraction,
                    color: ink,
                })
                .into_iter()
                .collect()
        } else {
            self.segments
        };
        for segment in segments {
            if !segment.fraction.is_finite() {
                continue;
            }
            let fraction = segment.fraction.clamp(0.0, remaining);
            if fraction > 0.0 {
                bar.push(
                    Element::spacer()
                        .flex(fraction)
                        .size(None, Some(6.0))
                        .background(segment.color),
                );
                remaining -= fraction;
            }
        }
        if remaining > 0.0 {
            bar.push(
                Element::spacer()
                    .flex(remaining)
                    .size(None, Some(6.0))
                    .background(palette.border),
            );
        }
        overview.push(Element::row(bar).size(None, Some(6.0)));
        if !self.caption.is_empty() {
            overview.push(compact_note(
                self.caption,
                format!("{scope}:caption"),
                "Medición",
                palette,
            ));
        }
        content.push(Element::column(overview).gap(4.0));
        if !self.rows.is_empty() {
            content.push(section_rule(palette));
            let rows = self
                .rows
                .into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let item = Element::row(vec![
                        Element::spacer()
                            .size(Some(8.0), Some(8.0))
                            .background(row.color)
                            .radius(2.0),
                        compact_label(&row.label, palette),
                        right_value(&row.value, palette),
                    ])
                    .gap(8.0)
                    .min_size(0.0, 20.0);
                    crate::Tooltip::new(item, format!("{scope}:usage:{index}"), row.label, palette)
                        .description(row.value)
                        .render()
                })
                .collect();
            content.push(Element::column(rows).gap(2.0));
        }
        for (section_index, (title, rows)) in self.sections.into_iter().enumerate() {
            if rows.is_empty() {
                continue;
            }
            content.push(section_rule(palette));
            let mut section = Vec::new();
            if !title.is_empty() {
                section.push(context_text(title, palette.muted, 11.0));
            }
            for (row_index, row) in rows.into_iter().enumerate() {
                let item = Element::row(vec![
                    compact_label(&row.label, palette),
                    right_value(&row.value, palette),
                ])
                .gap(12.0)
                .min_size(0.0, 18.0);
                let mut hint = crate::Tooltip::new(
                    item,
                    format!("{scope}:section:{section_index}:row:{row_index}"),
                    row.label,
                    palette,
                )
                .description(row.value);
                if let Some(description) = row.description {
                    hint = hint.description(description);
                }
                section.push(hint.render());
            }
            content.push(Element::column(section).gap(4.0));
        }
        if !self.note.is_empty() {
            content.push(compact_note(
                self.note,
                format!("{scope}:note"),
                "Estado del contexto",
                palette,
            ));
        }
        let has_footer = !self.details_action.is_empty();
        if has_footer {
            content.push(section_rule(palette));
            content.push(
                crate::Button::new(self.details_action, "Ver detalles", palette)
                    .variant(crate::ButtonVariant::Ghost)
                    .size(crate::ControlSize::Compact)
                    .trailing_icon(crate::Icon::ChevronRight)
                    .render(),
            );
        }
        if let Some(id) = self.scroll_id {
            let header = content.remove(0);
            let footer = has_footer.then(|| content.split_off(content.len() - 2));
            let mut parts = vec![
                header,
                crate::ScrollArea::new(id, Element::column(content).gap(6.0))
                    .buttons(false)
                    .tint(palette.muted)
                    .render()
                    .flex(1.0),
            ];
            if let Some(footer) = footer {
                parts.push(Element::column(footer).gap(6.0));
            }
            Element::column(parts).gap(6.0).padding(10.0)
        } else {
            Element::column(content).gap(6.0).padding(10.0)
        }
    }
}

fn compact_note(text: String, id: String, title: &str, palette: ComponentPalette) -> Element {
    crate::Tooltip::new(
        Element::faded_text(&text, 14.0)
            .font_family(FontFamily::Sans)
            .font_size(11.0)
            .foreground(palette.muted),
        id,
        title,
        palette,
    )
    .description(text)
    .render()
}

/// An interactive context trigger. None remains unknown rather than zero.
pub struct ContextMeter {
    id: String,
    compact: bool,
    percent: Option<u8>,
    fraction: Option<Option<f32>>,
    palette: ComponentPalette,
}

impl ContextMeter {
    pub fn new(id: impl Into<String>, percent: Option<u8>, palette: ComponentPalette) -> Self {
        Self {
            id: id.into(),
            compact: false,
            percent: percent.map(|value| value.min(100)),
            fraction: None,
            palette,
        }
    }
    /// Prefer an exact measurement over the legacy integer percentage. An
    /// explicit None clears the measurement instead of falling back to zero.
    pub fn fraction(mut self, fraction: Option<f32>) -> Self {
        self.fraction = Some(measured_fraction(fraction));
        self
    }
    /// Keep the measured ring and hit target when a toolbar has no room for a label.
    /// The host can retain the textual measurement in its shared tooltip.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }
}

impl Component for ContextMeter {
    fn render(self) -> Element {
        let fraction = self
            .fraction
            .unwrap_or_else(|| self.percent.map(|value| value as f32 / 100.0));
        let label = fraction.map_or_else(|| "Contexto".to_owned(), percent_label);
        let ink = usage_color(fraction, self.palette);
        let mut content =
            vec![Element::progress_ring(fraction, self.palette.border, ink)
                .size(Some(16.0), Some(16.0))];
        if !self.compact {
            content.push(context_text(
                label,
                if fraction.is_some() {
                    ink
                } else {
                    self.palette.muted
                },
                12.0,
            ));
        }
        self.palette
            .interactive_surface(Element::clickable(
                self.id,
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    Element::row(content).gap(6.0).size(None, Some(16.0)),
                    Element::spacer().flex(1.0),
                ]),
            ))
            .padding(6.0)
            .min_size(0.0, 28.0)
    }
}

pub struct ContextRow {
    pub label: String,
    pub value: String,
    pub description: Option<String>,
}
impl ContextRow {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: None,
        }
    }
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }
}

/// Caller-composed sections retain their units and provenance. No tokenizer,
/// pricing database, or model-name inference lives in this UI component.
pub struct ContextContent {
    title: String,
    description: String,
    sections: Vec<(String, Vec<ContextRow>)>,
    footer: Option<String>,
    palette: ComponentPalette,
}
impl ContextContent {
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            sections: Vec::new(),
            footer: None,
            palette,
        }
    }
    pub fn section(mut self, title: impl Into<String>, rows: Vec<ContextRow>) -> Self {
        self.sections.push((title.into(), rows));
        self
    }
    pub fn footer(mut self, text: impl Into<String>) -> Self {
        self.footer = Some(text.into());
        self
    }
}
impl Component for ContextContent {
    fn render(self) -> Element {
        let text = |value: String, color: Color, size: f32| {
            Element::text(value)
                .font_family(FontFamily::Sans)
                .foreground(color)
                .font_size(size)
        };
        let mut content = vec![
            text(self.title, self.palette.text, 12.0),
            text(self.description, self.palette.muted, 11.0),
        ];
        for (title, rows) in self.sections {
            let mut group = vec![
                Element::spacer()
                    .size(None, Some(1.0))
                    .background(self.palette.border),
                text(title, self.palette.text, 12.0),
            ];
            for row in rows {
                let mut parts = vec![Element::row(vec![
                    text(row.label, self.palette.muted, 12.0).flex(1.0),
                    text(row.value, self.palette.text, 12.0).flex(1.0),
                ])
                .gap(12.0)];
                if let Some(description) = row.description {
                    parts.push(text(description, self.palette.muted, 11.0));
                }
                group.push(Element::column(parts).gap(4.0));
            }
            content.push(Element::column(group).gap(6.0));
        }
        if let Some(footer) = self.footer {
            content.push(text(footer, self.palette.muted, 11.0));
        }
        Element::column(content).gap(10.0).padding(10.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DrawCommand, Key, Size, UiAction, UiEvent, UiTree};
    #[test]
    fn compact_usage_bar_uses_measured_fractions_and_explicit_actions() {
        let palette = ComponentPalette::neutral_dark();
        let input = Color::rgb(153, 129, 245);
        let output = Color::rgb(219, 129, 119);
        for width in [300.0, 660.0] {
            let mut tree = UiTree::new(
                ContextUsage::new("Uso de contexto", palette)
                    .summary("30% usado", "300 / 1000 tokens")
                    .caption("Última respuesta reportada")
                    .segments(vec![
                        ContextSegment {
                            fraction: 0.25,
                            color: input,
                        },
                        ContextSegment {
                            fraction: 0.05,
                            color: output,
                        },
                    ])
                    .rows(vec![ContextUsageRow {
                        label: "Skills".into(),
                        value: "2 · 400 bytes".into(),
                        color: input,
                    }])
                    .actions("close", "details")
                    .render(),
            );
            let frame = tree.layout(Size::new(width, 420.0));
            let portion = |color| {
                frame
                    .commands
                    .iter()
                    .find_map(|command| match command {
                        DrawCommand::FillRect {
                            rect,
                            color: actual,
                            ..
                        } if *actual == color && rect.height == 6.0 => Some(*rect),
                        _ => None,
                    })
                    .unwrap()
            };
            let first = portion(input);
            let second = portion(output);
            let free = portion(palette.border);
            let total_width = first.width + second.width + free.width;
            assert!((first.width / total_width - 0.25).abs() < 0.001);
            assert!((second.width / total_width - 0.05).abs() < 0.001);
            assert!((free.width / total_width - 0.70).abs() < 0.001);
            assert!(first.x + first.width <= second.x + 0.1);
            for id in ["close", "details"] {
                assert!(tree.focus(id));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
            assert!(!tree.is_animating());
        }
    }
    #[test]
    fn context_trigger_keeps_unknown_separate_from_zero_and_full_and_activates_by_keyboard() {
        for percent in [None, Some(0), Some(42), Some(100)] {
            let mut tree = UiTree::new(
                ContextMeter::new("context", percent, ComponentPalette::neutral_dark()).render(),
            );
            let frame = tree.layout(Size::new(120.0, 32.0));
            assert!(frame.commands.iter().any(|command| matches!(command, DrawCommand::ProgressRing { fraction, .. } if *fraction == percent.map(|value| value as f32 / 100.0))));
            assert!(!tree.is_animating());
            assert!(tree.focus("context"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "context".into()
                }]
            );
        }
    }
    fn natural_card(card: ContextUsage) -> Element {
        Element::column(vec![card.render(), Element::spacer().flex(1.0)])
    }

    fn tooltip_text(frame: &crate::Frame) -> String {
        let start = frame
            .commands
            .iter()
            .rposition(|command| matches!(command, DrawCommand::Shadow { .. }))
            .unwrap_or(frame.commands.len());
        frame.commands[start..]
            .iter()
            .filter_map(|command| match command {
                DrawCommand::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            // Wrapped painter fragments can retain their trailing space.
            // Compare the complete word sequence, independent of line breaks.
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn compact_card_keeps_values_aligned_and_nine_categories_below_480_pixels() {
        let palette = ComponentPalette::neutral_dark();
        for width in [300.0, 344.0] {
            let mut tree = UiTree::new(natural_card(
                ContextUsage::new("Uso de contexto", palette)
                    .subtitle("Última respuesta")
                    .model("Proveedor · modelo exacto")
                    .usage(Some(0.3), "30 000 / 100 000 tokens")
                    .caption("Medición reportada por el proveedor")
                    .rows(
                        [
                            ("Entrada", "24 000"),
                            ("Salida", "6 000"),
                            ("Caché", "12 000"),
                            ("Razonamiento", "3 000"),
                        ]
                        .into_iter()
                        .map(|(label, value)| ContextUsageRow {
                            label: label.into(),
                            value: value.into(),
                            color: palette.text,
                        })
                        .collect(),
                    )
                    .section(
                        "Fuentes del proyecto",
                        vec![
                            ContextRow::new("Instrucciones", "2 documentos"),
                            ContextRow::new("Skills", "4 documentos"),
                            ContextRow::new("Herramientas", "18 disponibles"),
                            ContextRow::new("Archivos", "8 400 bytes"),
                            ContextRow::new("Entorno", "Local"),
                        ],
                    )
                    .note("La caché forma parte de la entrada")
                    .actions("close", "details"),
            ));
            let measure = |text: &str, size: f32| text.chars().count() as f32 * size * 0.52;
            tree.set_text_measurer(move |text, size, _| measure(text, size));
            let frame = tree.layout(Size::new(width, 700.0));
            let close = frame
                .regions
                .iter()
                .find(|region| region.id == "close")
                .unwrap()
                .rect;
            let details = frame
                .regions
                .iter()
                .find(|region| region.id == "details")
                .unwrap()
                .rect;
            assert_eq!((close.width, close.height), (24.0, 24.0));
            assert_eq!(close.x + close.width, width - 10.0);
            assert_eq!(details.height, 28.0);
            assert!(details.y + details.height + 10.0 <= 480.0, "{details:?}");
            let blocks = frame.text_layouts();
            for value in [
                "24 000",
                "6 000",
                "12 000",
                "3 000",
                "2 documentos",
                "4 documentos",
                "18 disponibles",
                "8 400 bytes",
                "Local",
            ] {
                let runs: Vec<_> = frame
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        DrawCommand::Text {
                            position,
                            text,
                            font_size,
                            ..
                        } if text == value => Some((position.x, measure(text, *font_size))),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    runs.len(),
                    1,
                    "short value must remain fully visible: {value}"
                );
                assert!(
                    (runs[0].0 + runs[0].1 - (width - 10.0)).abs() < 0.01,
                    "{value}: {runs:?}"
                );
            }
            assert!(blocks
                .iter()
                .flat_map(|block| &block.runs)
                .all(|run| run.rect.x >= 0.0 && run.rect.x + run.rect.width <= width + 0.01));
            for id in ["close", "details"] {
                assert!(tree.focus(id));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    vec![UiAction::Clicked { id: id.into() }]
                );
            }
        }
    }

    #[test]
    fn long_category_values_keep_a_label_column_fade_and_full_tooltip_at_narrow_widths() {
        use crate::{Point, Rect};
        use std::time::{Duration, Instant};
        let palette = ComponentPalette::neutral_dark();
        let skills = "12 seleccionadas · 98 765 bytes de instrucciones disponibles";
        let mcp = "12 servidores habilitados · 384 herramientas disponibles";
        let model = "proveedor/modelo-con-nombre-muy-largo-y-variante-de-contexto-ampliado";
        for width in [280.0, 344.0] {
            let mut tree = UiTree::new(natural_card(
                ContextUsage::new("Uso de contexto", palette)
                    .model(model)
                    .usage(Some(0.25), "25 000 / 100 000 tokens")
                    .section(
                        "Fuentes",
                        vec![
                            ContextRow::new("Skills", skills),
                            ContextRow::new("MCP", mcp)
                                .description("Las herramientas no son un conteo de tokens."),
                        ],
                    )
                    .actions("close", "details"),
            ));
            tree.set_reduced_motion(true);
            tree.set_text_measurer(|text, size, _| text.chars().count() as f32 * size * 0.55);
            // UI labels use calibrated Sans metrics; the per-size height
            // callback belongs to fixed-row code views, not ordinary text.
            tree.set_sans_metrics(crate::FontMetrics {
                char_width: 7.7,
                line_height: crate::DEFAULT_FONT_SIZE * 1.3,
            });
            let now = Instant::now();
            let size = Size::new(width, 640.0);
            let frame = tree.layout_at(size, now);
            let close = frame
                .regions
                .iter()
                .find(|region| region.id == "close")
                .unwrap()
                .rect;
            let (title_y, title_size) = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::Text {
                        position,
                        text,
                        font_size,
                        ..
                    } if text == "Uso de contexto" => Some((position.y, font_size)),
                    _ => None,
                })
                .unwrap();
            assert!(
                (title_y + title_size * 1.3 / 2.0 - (close.y + close.height / 2.0)).abs() < 0.01
            );
            let mut clips = vec![Rect::new(0.0, 0.0, width, size.height)];
            let mut faded = [false; 2];
            let mut model_faded = false;
            for command in &frame.commands {
                match command {
                    DrawCommand::PushClip(rect) => {
                        clips.push(clips.last().unwrap().intersection(*rect).unwrap())
                    }
                    DrawCommand::PopClip => {
                        clips.pop();
                    }
                    DrawCommand::Text { text, color, .. } => {
                        if let Some(index) = [skills, mcp].iter().position(|value| text == *value) {
                            let clip = clips.last().unwrap();
                            assert!(
                                clip.x >= width / 2.0,
                                "value must not consume the label column: {clip:?}"
                            );
                            assert!(clip.x + clip.width <= width - 10.0 + 0.01, "{clip:?}");
                            faded[index] |= color.a < 255;
                        }
                        if text == model {
                            let clip = clips.last().unwrap();
                            assert!(clip.x + clip.width <= width - 10.0 + 0.01);
                            model_faded |= color.a < 255;
                        }
                    }
                    _ => {}
                }
            }
            assert!(faded.into_iter().all(|faded| faded));
            assert!(model_faded);
            for label in ["Skills", "MCP"] {
                assert!(frame.commands.iter().any(|command| matches!(command,
                    DrawCommand::Text { text, color, position, .. }
                        if text == label && color.a == 255 && position.x == 10.0)));
            }
            for (index, value) in [skills, mcp].into_iter().enumerate() {
                let id = format!("close:section:0:row:{index}");
                let row = frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect;
                assert_eq!(row.height, 18.0);
                let moment = now + Duration::from_secs(index as u64 * 2);
                tree.handle_event_at(
                    UiEvent::PointerMove(Point::new(row.x + 8.0, row.y + 9.0)),
                    moment,
                );
                tree.layout_at(size, moment);
                let hovered = tree.layout_at(size, moment + Duration::from_millis(700));
                assert!(
                    tooltip_text(&hovered).contains(value),
                    "tooltip must retain {value}; actual={:?}",
                    tooltip_text(&hovered)
                );
            }
        }
    }

    #[test]
    fn unknown_card_is_naturally_short_and_category_descriptions_are_hover_only() {
        use crate::Point;
        use std::time::{Duration, Instant};
        let palette = ComponentPalette::neutral_dark();
        let description = "Incluye instrucciones locales en bytes UTF-8; no representa tokens medidos por el proveedor.";
        let mut tree = UiTree::new(natural_card(
            ContextUsage::new("Uso de contexto", palette)
                .usage(None, "Límite todavía no informado")
                .section(
                    "Fuentes",
                    vec![ContextRow::new("Skills", "4 documentos").description(description)],
                )
                .actions("close", "details"),
        ));
        tree.set_reduced_motion(true);
        let now = Instant::now();
        let size = Size::new(344.0, 600.0);
        let frame = tree.layout_at(size, now);
        let blocks = frame.text_layouts();
        assert!(blocks.iter().any(|block| block.source == "Sin medición"));
        assert!(!blocks
            .iter()
            .any(|block| block.source == "0% usado" || block.source == description));
        let details = frame
            .regions
            .iter()
            .find(|region| region.id == "details")
            .unwrap()
            .rect;
        assert!(details.y + details.height + 12.0 < 280.0, "{details:?}");
        let category = frame
            .regions
            .iter()
            .find(|region| region.id == "close:section:0:row:0")
            .unwrap()
            .rect;
        let point = Point::new(category.x + 4.0, category.y + category.height / 2.0);
        tree.handle_event_at(UiEvent::PointerMove(point), now);
        tree.layout_at(size, now);
        let hovered = tree.layout_at(size, now + Duration::from_millis(700));
        assert!(
            tooltip_text(&hovered).contains(description),
            "tooltip={:?}, point={point:?}, target={:?}",
            tooltip_text(&hovered),
            frame.hit_test(point)
        );
        assert!(tree
            .handle_event_at(
                UiEvent::PointerDown(point),
                now + Duration::from_millis(800)
            )
            .is_empty());
        assert!(tree
            .handle_event_at(UiEvent::PointerUp(point), now + Duration::from_millis(810))
            .is_empty());
    }

    #[test]
    fn exact_meter_preserves_sub_percent_usage_unknown_and_threshold_colors_without_motion() {
        let palette = ComponentPalette::neutral_dark();
        for (fraction, expected, label, color) in [
            (None, None, "Contexto", palette.muted),
            (Some(0.0), Some(0.0), "0%", palette.text),
            (Some(0.004), Some(0.004), "<1%", palette.text),
            (Some(0.42), Some(0.42), "42%", palette.text),
            (Some(0.8), Some(0.8), "80%", Color::rgb(225, 169, 65)),
            (Some(1.0), Some(1.0), "100%", Color::rgb(235, 115, 119)),
            (Some(2.0), Some(1.0), "100%", Color::rgb(235, 115, 119)),
            (Some(-1.0), Some(0.0), "0%", palette.text),
            (Some(f32::NAN), None, "Contexto", palette.muted),
            (Some(f32::INFINITY), None, "Contexto", palette.muted),
        ] {
            let mut tree = UiTree::new(
                ContextMeter::new("context", Some(0), palette)
                    .fraction(fraction)
                    .render(),
            );
            let frame = tree.layout(Size::new(120.0, 32.0));
            assert!(frame.commands.iter().any(|command| matches!(command,
                DrawCommand::ProgressRing { fraction, fill, .. } if *fraction == expected && *fill == color)));
            assert!(frame
                .text_layouts()
                .iter()
                .any(|block| block.source == label));
            assert!(!tree.is_animating());
            assert!(tree.animation_repaint_after().is_none());
        }
    }

    #[test]
    fn compact_meter_preserves_measurement_center_and_keyboard_action_without_a_label() {
        let palette = ComponentPalette::neutral_dark();
        for fraction in [None, Some(0.42), Some(0.9)] {
            let mut tree = UiTree::new(
                ContextMeter::new("context", None, palette)
                    .fraction(fraction)
                    .compact(true)
                    .render()
                    .size(Some(28.0), Some(28.0)),
            );
            let frame = tree.layout(Size::new(28.0, 28.0));
            let target = frame
                .regions
                .iter()
                .find(|region| region.id == "context")
                .unwrap()
                .rect;
            let ring = frame
                .commands
                .iter()
                .find_map(|command| match command {
                    DrawCommand::ProgressRing {
                        rect,
                        fraction: actual,
                        ..
                    } if *actual == fraction => Some(*rect),
                    _ => None,
                })
                .expect("same measured ring in compact mode");
            assert_eq!((target.width, target.height), (28.0, 28.0));
            assert_eq!(ring.x + ring.width / 2.0, target.x + target.width / 2.0);
            assert_eq!(ring.y + ring.height / 2.0, target.y + target.height / 2.0);
            assert!(frame.text_layouts().is_empty());
            assert!(tree.focus("context"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                vec![UiAction::Clicked {
                    id: "context".into()
                }]
            );
        }
    }

    #[test]
    fn meter_hover_and_pressed_surfaces_keep_the_ring_center_and_target_geometry() {
        use crate::Point;
        let palette = ComponentPalette::neutral_dark();
        let size = Size::new(120.0, 32.0);
        let mut tree = UiTree::new(ContextMeter::new("context", Some(30), palette).render());
        tree.set_reduced_motion(true);
        let initial = tree.layout(size);
        let target = initial
            .regions
            .iter()
            .find(|region| region.id == "context")
            .unwrap()
            .rect;
        let ring = initial
            .commands
            .iter()
            .find_map(|command| match command {
                DrawCommand::ProgressRing { rect, .. } => Some(*rect),
                _ => None,
            })
            .unwrap();
        assert_eq!(target.height, 32.0);
        assert_eq!((ring.width, ring.height), (16.0, 16.0));
        assert_eq!(ring.y + ring.height / 2.0, target.y + target.height / 2.0);
        let point = Point::new(target.x + 10.0, target.y + 16.0);
        tree.handle_event(UiEvent::PointerMove(point));
        let hovered = tree.layout(size);
        assert!(hovered.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. } if *rect == target && *color == palette.hover)));
        tree.handle_event(UiEvent::PointerDown(point));
        let pressed = tree.layout(size);
        assert!(pressed.commands.iter().any(|command| matches!(command,
            DrawCommand::FillRect { rect, color, .. } if *rect == target && *color == palette.selected)));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked {
                id: "context".into()
            }]
        );
        assert_eq!(initial.regions, pressed.regions);
    }

    #[test]
    fn usage_bar_rejects_nonfinite_segments_and_caps_the_last_real_portion() {
        let palette = ComponentPalette::neutral_dark();
        let first_color = Color::rgb(130, 90, 220);
        let second_color = Color::rgb(210, 130, 90);
        let frame = UiTree::new(natural_card(
            ContextUsage::new("Contexto", palette)
                .usage(Some(1.0), "100 / 100 tokens")
                .segments(vec![
                    ContextSegment {
                        fraction: f32::NAN,
                        color: Color::WHITE,
                    },
                    ContextSegment {
                        fraction: f32::INFINITY,
                        color: Color::WHITE,
                    },
                    ContextSegment {
                        fraction: -1.0,
                        color: Color::WHITE,
                    },
                    ContextSegment {
                        fraction: 0.8,
                        color: first_color,
                    },
                    ContextSegment {
                        fraction: 0.7,
                        color: second_color,
                    },
                ]),
        ))
        .layout(Size::new(344.0, 300.0));
        let bars: Vec<_> = frame
            .commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::FillRect { rect, color, .. } if rect.height == 6.0 => {
                    Some((*rect, *color))
                }
                _ => None,
            })
            .collect();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].1, first_color);
        assert_eq!(bars[1].1, second_color);
        let width = bars[0].0.width + bars[1].0.width;
        assert!((bars[0].0.width / width - 0.8).abs() < 0.001);
        assert!((bars[1].0.width / width - 0.2).abs() < 0.001);
        assert!((bars[1].0.x + bars[1].0.width - 334.0).abs() < 0.01);
    }

    #[test]
    fn short_scroll_card_keeps_header_footer_visible_while_reaching_the_last_category() {
        use crate::Point;
        let palette = ComponentPalette::neutral_dark();
        let mut tree = UiTree::new(
            ContextUsage::new("Uso de contexto", palette)
                .usage(Some(0.4), "40 000 / 100 000 tokens")
                .section(
                    "Fuentes",
                    (0..20)
                        .map(|index| {
                            ContextRow::new(
                                format!("Fuente {index}"),
                                format!("{} documentos", index + 1),
                            )
                        })
                        .collect(),
                )
                .actions("close", "details")
                .scroll_id("summary")
                .render(),
        );
        tree.set_reduced_motion(true);
        let size = Size::new(344.0, 300.0);
        let initial = tree.layout(size);
        let region = |frame: &crate::Frame, id: &str| {
            frame
                .regions
                .iter()
                .find(|region| region.id == id)
                .unwrap()
                .rect
        };
        let header = region(&initial, "close");
        let footer = region(&initial, "details");
        let body = region(&initial, "summary");
        assert!(header.y + header.height <= body.y);
        assert!(body.y + body.height < footer.y);
        assert!(footer.y + footer.height <= size.height - 10.0);
        assert!(initial
            .regions
            .iter()
            .all(|region| region.id != "close:section:0:row:19"));
        tree.handle_event(UiEvent::ScrollDirect {
            position: Point::new(body.x + 20.0, body.y + 20.0),
            delta_y: -100_000.0,
        });
        let last = tree.layout(size);
        assert!(tree.scroll_offset("summary") > 0.0);
        assert!(tree.is_scroll_at_end("summary", 0.01));
        assert_eq!(region(&last, "close"), header);
        assert_eq!(region(&last, "details"), footer);
        let last_row = region(&last, "close:section:0:row:19");
        assert!(
            last_row.y >= body.y && last_row.y + last_row.height <= body.y + body.height + 0.01
        );
        assert!(tree.focus("close"));
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: false,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("details"));
        assert_eq!(
            tree.handle_event(UiEvent::Key {
                key: Key::Enter,
                shift: false,
                ctrl: false
            }),
            vec![UiAction::Clicked {
                id: "details".into()
            }]
        );
        tree.handle_event(UiEvent::Key {
            key: Key::Tab,
            shift: true,
            ctrl: false,
        });
        assert_eq!(tree.focused_id(), Some("close"));
        let point = Point::new(header.x + 16.0, header.y + 16.0);
        tree.handle_event(UiEvent::PointerDown(point));
        assert_eq!(
            tree.handle_event(UiEvent::PointerUp(point)),
            vec![UiAction::Clicked { id: "close".into() }]
        );
    }

    #[test]
    fn optional_body_scroll_preserves_natural_height_when_the_card_fits() {
        let palette = ComponentPalette::neutral_dark();
        let card = || {
            ContextUsage::new("Uso de contexto", palette)
                .usage(None, "Límite no informado")
                .actions("close", "details")
        };
        let mut plain = UiTree::new(natural_card(card()));
        let mut scroll = UiTree::new(natural_card(card().scroll_id("summary")));
        let size = Size::new(344.0, 700.0);
        let plain = plain.layout(size);
        let scroll_frame = scroll.layout(size);
        for id in ["close", "details"] {
            let rect = |frame: &crate::Frame| {
                frame
                    .regions
                    .iter()
                    .find(|region| region.id == id)
                    .unwrap()
                    .rect
            };
            let expected = rect(&plain);
            let actual = rect(&scroll_frame);
            assert!((actual.x - expected.x).abs() < 0.01);
            assert!((actual.y - expected.y).abs() < 0.01);
            assert_eq!(
                (actual.width, actual.height),
                (expected.width, expected.height)
            );
        }
        assert_eq!(scroll.scroll_offset("summary"), 0.0);
        let footer = scroll_frame
            .regions
            .iter()
            .find(|region| region.id == "details")
            .unwrap()
            .rect;
        assert!(footer.y + footer.height + 12.0 < 240.0);
    }

    #[test]
    fn context_sections_keep_exact_counts_unknown_units_and_long_sources_at_narrow_widths() {
        let description =
            "Proveedor exacto · modelo-exacto · fuente verificada al guardar el evento";
        let long =
            "Instrucciones adicionales locales: bytes UTF-8, no tokens del proveedor. ".repeat(8);
        for width in [240.0, 360.0, 640.0] {
            let frame = UiTree::new(
                ContextContent::new(
                    "Contexto nativo",
                    description,
                    ComponentPalette::neutral_dark(),
                )
                .section(
                    "Última respuesta",
                    vec![
                        ContextRow::new("Entrada", "12 345 tokens"),
                        ContextRow::new("Caché", "Sin dato"),
                    ],
                )
                .section(
                    "Instrucciones",
                    vec![ContextRow::new("Skills", "4 documentos").description(&long)],
                )
                .footer("La caché es parte de la entrada; no se suma de nuevo.")
                .render(),
            )
            .layout(Size::new(width, 1600.0));
            let blocks = frame.text_layouts();
            assert!(blocks.iter().any(|block| block.source == description));
            assert!(blocks.iter().any(|block| block.source == long));
            assert!(blocks.iter().any(|block| block.source == "Sin dato"));
            assert!(frame.regions.is_empty());
            for run in blocks.iter().flat_map(|block| &block.runs) {
                assert!(
                    run.rect.x >= 0.0 && run.rect.x + run.rect.width <= width + 0.1,
                    "{run:?}"
                );
            }
        }
    }
}
