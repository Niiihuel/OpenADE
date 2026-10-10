//! A pending approval and its three answers. Arriving never takes focus.

use crate::{
    Button, ButtonGroup, ButtonVariant, Component, ComponentPalette, ControlSize, Element,
    FontFamily, Icon, Text, TextRole,
};

/// A controlled request card for a transcript: what is asked, on what, and
/// the decision. The host owns each action id and validates it on dispatch.
pub struct ApprovalRequest {
    deny_id: String,
    allow_session_id: String,
    allow_once_id: String,
    title: String,
    target: Option<String>,
    status: String,
    icon: Icon,
    palette: ComponentPalette,
    stacked: bool,
}

impl ApprovalRequest {
    pub fn new(
        title: impl Into<String>,
        deny_id: impl Into<String>,
        allow_session_id: impl Into<String>,
        allow_once_id: impl Into<String>,
        palette: ComponentPalette,
    ) -> Self {
        Self {
            deny_id: deny_id.into(),
            allow_session_id: allow_session_id.into(),
            allow_once_id: allow_once_id.into(),
            title: title.into(),
            target: None,
            status: "Aprobación pendiente".into(),
            icon: Icon::InfoCircle,
            palette,
            stacked: false,
        }
    }

    /// Put the answers in a column when the card is too narrow for a row.
    pub fn stacked(mut self, stacked: bool) -> Self {
        self.stacked = stacked;
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
        let mut summary = vec![
            // The status moves like streaming text: the turn is alive and waiting.
            Element::shimmer_truncated_text(
                format!("{}:status", self.allow_once_id),
                self.status,
                self.palette.text,
            )
            .font_family(FontFamily::Sans)
            .font_size(11.0)
            .foreground(self.palette.muted),
            Text::new(self.title, self.palette).truncate().render(),
        ];
        if let Some(target) = self.target.filter(|target| !target.is_empty()) {
            // Nothing else shows the whole target, so it wraps.
            summary.push(
                Text::new(target, self.palette)
                    .role(TextRole::Small)
                    .mono()
                    .muted()
                    .render(),
            );
        }
        // The lasting choice sits apart from the two answers to this request.
        let decisions = ButtonGroup::new(vec![
            Button::new(self.deny_id, "Denegar", self.palette)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Compact)
                .render(),
            Button::new(self.allow_once_id, "Permitir una vez", self.palette)
                .size(ControlSize::Compact)
                .render(),
        ])
        .leading(
            Button::new(self.allow_session_id, "Permitir en esta sesión", self.palette)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Compact)
                .tooltip("Autoriza esta misma operación, con los mismos argumentos y contexto, durante esta sesión")
                .render(),
        )
        .stacked(self.stacked)
        .align_end()
        .render();
        Element::panel(
            Element::column(vec![
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
                ])
                .gap(8.0),
                decisions,
            ])
            .gap(10.0),
        )
        .padding(10.0)
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
    fn request_offers_its_three_answers_inside_the_card_without_taking_focus() {
        for width in [240.0, 350.0, 900.0] {
            let mut tree = UiTree::new(
                ApprovalRequest::new(
                    "Eliminar archivo",
                    "deny",
                    "session",
                    "once",
                    ComponentPalette::neutral_dark(),
                )
                .stacked(width < 470.0)
                .target("src/una/carpeta/con/un/nombre/bastante/largo/documentación.md")
                .icon(Icon::Trash)
                .render(),
            );
            let frame = tree.layout(Size::new(width, 400.0));
            assert!(
                tree.focused_id().is_none(),
                "arriving approval must not steal focus"
            );
            let buttons: Vec<_> = frame
                .regions
                .iter()
                .filter(|region| region.kind == HitKind::Button)
                .collect();
            assert_eq!(
                buttons.iter().map(|button| button.id.as_str()).collect::<Vec<_>>(),
                ["session", "deny", "once"]
            );
            for button in buttons {
                assert!(button.rect.x >= 0.0);
                assert!(button.rect.x + button.rect.width <= width);
                assert_eq!(button.rect.height, 28.0);
                assert!(tree.focus(&button.id));
                assert_eq!(
                    tree.handle_event(UiEvent::Key {
                        key: Key::Enter,
                        shift: false,
                        ctrl: false
                    }),
                    [UiAction::Clicked {
                        id: button.id.clone()
                    }]
                );
            }
        }
    }
}
