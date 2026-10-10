//! A passive approval notice. Reviewing a request never grants it.

use crate::{
    Button, ButtonVariant, Component, ComponentPalette, ControlSize, Element, FontFamily, Icon,
    Text, TextRole,
};

/// A compact, controlled request summary for a transcript or composer notice.
/// Only the review action is available here; the host owns the decision dialog.
pub struct ApprovalRequest {
    review_id: String,
    identity: Option<String>,
    title: String,
    target: Option<String>,
    status: String,
    icon: Icon,
    palette: ComponentPalette,
    compact: bool,
}

impl ApprovalRequest {
    pub fn new(
        review_id: impl Into<String>,
        title: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            review_id: review_id.into(),
            identity: None,
            title: title.into(),
            target: None,
            status: "Aprobación pendiente".into(),
            icon: Icon::InfoCircle,
            palette,
            compact: false,
        }
    }

    /// Distinct focus identity when a pending request also appears in a banner.
    /// Activation still emits the original review action.
    pub fn identity(mut self, identity: impl Into<String>) -> Self {
        self.identity = Some(identity.into());
        self
    }

    /// A one-line reminder next to the composer; the transcript keeps details.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn status(mut self, status: impl Into<String>) -> Self {
        self.status = status.into();
        self
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = icon;
        self
    }
}

impl Component for ApprovalRequest {
    fn render(self) -> Element {
        // The status moves like streaming text: the turn is alive and waiting.
        let waiting = |status: &str, size: f32| {
            Element::shimmer_truncated_text(
                format!(
                    "{}:status",
                    self.identity.as_deref().unwrap_or(&self.review_id)
                ),
                status,
                self.palette.text,
            )
            .font_family(FontFamily::Sans)
            .font_size(size)
            .foreground(self.palette.muted)
        };
        let mut summary = if self.compact {
            vec![waiting(&self.status, 12.0)]
        } else {
            vec![
                waiting(&self.status, 11.0),
                Text::new(self.title, self.palette).truncate().render(),
            ]
        };
        if let Some(target) = self
            .target
            .filter(|target| !self.compact && !target.is_empty())
        {
            summary.push(
                Text::new(target, self.palette)
                    .role(TextRole::Small)
                    .mono()
                    .muted()
                    .truncate()
                    .render(),
            );
        }
        let review = Button::new(
            self.identity.unwrap_or_else(|| self.review_id.clone()),
            "Revisar",
            self.palette,
        )
        .variant(ButtonVariant::Outline)
        .size(if self.compact {
            ControlSize::Dense
        } else {
            ControlSize::Compact
        })
        .render()
        .dispatch_as(self.review_id);
        Element::panel(
            Element::row(vec![
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    Element::icon(self.icon)
                        .foreground(self.palette.muted)
                        .size(Some(16.0), Some(16.0)),
                    Element::spacer().flex(1.0),
                ])
                .size(Some(16.0), None),
                Element::column(summary).gap(4.0).flex(1.0),
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    review,
                    Element::spacer().flex(1.0),
                ]),
            ])
            .gap(8.0),
        )
        .padding(if self.compact { 8.0 } else { 10.0 })
        .background(self.palette.surface)
        .border(self.palette.border, 1.0)
        .radius(8.0)
    }
}

// The body before its text moved to limn's Text, kept for the parity test.
#[cfg(test)]
impl ApprovalRequest {
    fn legacy_render(self) -> Element {
        // The status moves like streaming text: the turn is alive and waiting.
        let waiting = |status: &str, size: f32| {
            Element::shimmer_truncated_text(
                format!(
                    "{}:status",
                    self.identity.as_deref().unwrap_or(&self.review_id)
                ),
                status,
                self.palette.text,
            )
            .font_family(FontFamily::Sans)
            .font_size(size)
            .foreground(self.palette.muted)
        };
        let mut summary = if self.compact {
            vec![waiting(&self.status, 12.0)]
        } else {
            vec![
                waiting(&self.status, 11.0),
                Element::truncated_text(self.title)
                    .font_family(FontFamily::Sans)
                    .font_size(12.0)
                    .foreground(self.palette.text),
            ]
        };
        if let Some(target) = self
            .target
            .filter(|target| !self.compact && !target.is_empty())
        {
            summary.push(
                Element::truncated_text(target)
                    .font_family(FontFamily::Monospace)
                    .font_size(11.0)
                    .foreground(self.palette.muted),
            );
        }
        let review = Button::new(
            self.identity.unwrap_or_else(|| self.review_id.clone()),
            "Revisar",
            self.palette,
        )
        .variant(ButtonVariant::Outline)
        .size(if self.compact {
            ControlSize::Dense
        } else {
            ControlSize::Compact
        })
        .render()
        .dispatch_as(self.review_id);
        Element::panel(
            Element::row(vec![
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    Element::icon(self.icon)
                        .foreground(self.palette.muted)
                        .size(Some(16.0), Some(16.0)),
                    Element::spacer().flex(1.0),
                ])
                .size(Some(16.0), None),
                Element::column(summary).gap(4.0).flex(1.0),
                Element::column(vec![
                    Element::spacer().flex(1.0),
                    review,
                    Element::spacer().flex(1.0),
                ]),
            ])
            .gap(8.0),
        )
        .padding(if self.compact { 8.0 } else { 10.0 })
        .background(self.palette.surface)
        .border(self.palette.border, 1.0)
        .radius(8.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HitKind, Key, Size, UiAction, UiEvent, UiTree};

    #[test]
    fn the_request_on_limn_text_matches_the_body_it_replaced() {
        for width in [240.0, 720.0] {
            for compact in [false, true] {
                let request = || {
                    ApprovalRequest::new(
                        "review",
                        "Eliminar archivo",
                        ComponentPalette::neutral_dark(),
                    )
                    .compact(compact)
                    .target("src/una/carpeta/con/un/nombre/bastante/largo/documentación.md")
                };
                let frame = |element| {
                    let mut tree = UiTree::new(Element::column(vec![element]));
                    tree.set_reduced_motion(true);
                    tree.layout(Size::new(width, 120.0))
                };
                let (old, new) = (frame(request().legacy_render()), frame(request().render()));
                assert_eq!(old.commands, new.commands, "compact {compact} at {width}");
                assert_eq!(format!("{:?}", old.regions), format!("{:?}", new.regions));
            }
        }
    }

    #[test]
    fn composer_reminder_is_compact_and_keeps_review_reachable() {
        for width in [240.0, 350.0, 900.0] {
            let mut tree = UiTree::new(
                ApprovalRequest::new(
                    "review",
                    "Eliminar archivo",
                    ComponentPalette::neutral_dark(),
                )
                .compact(true)
                .target("src/archivo.md")
                .render(),
            );
            let frame = tree.layout(Size::new(width, 48.0));
            let review = frame
                .regions
                .iter()
                .find(|region| region.id == "review")
                .unwrap();
            assert!(review.rect.x >= 0.0 && review.rect.x + review.rect.width <= width);
            assert!(review.rect.y >= 0.0 && review.rect.y + review.rect.height <= 48.0);
            assert_eq!(review.rect.height, 24.0);
            assert!(frame.commands.iter().any(|command| matches!(command,
                crate::DrawCommand::Text { text, .. } if text.starts_with("Aprobación"))));
        }
    }

    #[test]
    fn request_is_passive_responsive_and_only_opens_review() {
        for width in [350.0, 900.0] {
            let mut tree = UiTree::new(
                ApprovalRequest::new(
                    "review:request-1",
                    "Eliminar archivo",
                    ComponentPalette::neutral_dark(),
                )
                .identity("review-inline")
                .target("src/una/carpeta/con/un/nombre/bastante/largo/documentación.md")
                .icon(Icon::Trash)
                .render(),
            );
            let frame = tree.layout(Size::new(width, 200.0));
            assert!(
                tree.focused_id().is_none(),
                "arriving approval must not steal focus"
            );
            let buttons: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button)
                .collect();
            assert_eq!(buttons.len(), 1);
            assert_eq!(buttons[0].id, "review-inline");
            assert!(buttons[0].rect.x >= 0.0);
            assert!(buttons[0].rect.x + buttons[0].rect.width <= width);
            assert_eq!(buttons[0].rect.height, 28.0);
            assert!(tree.focus("review-inline"));
            assert_eq!(
                tree.handle_event(UiEvent::Key {
                    key: Key::Enter,
                    shift: false,
                    ctrl: false
                }),
                [UiAction::Clicked {
                    id: "review:request-1".into()
                }]
            );
        }
    }
}
