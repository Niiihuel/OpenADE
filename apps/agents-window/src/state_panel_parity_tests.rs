//! The comparison behind every state panel that limn's `Notice` and `Text`
//! replaced. Each file keeps its old body beside the new one under
//! `#[cfg(test)]`. Delete with the final audit (S18).

use crate::theme;
use openide_ui::{Component, Element, FontFamily, Size, Text, TextRole, UiTree};

/// Same draw commands filling a narrow and a wide pane, and the same frames
/// and actions under the pointer at its own size.
pub(crate) fn assert_same(name: &str, legacy: Element, current: Element) {
    for width in [240.0, 900.0] {
        let frame = |element: Element| {
            let mut tree = UiTree::new(Element::column(vec![
                element,
                Element::spacer().flex(1.0),
            ]));
            tree.set_reduced_motion(true);
            tree.layout(Size::new(width, 400.0))
        };
        assert_eq!(
            frame(current.clone()),
            frame(legacy.clone()),
            "{name} width={width}"
        );
    }
    crate::icon_button_parity_tests::assert_same(name, legacy, current);
}

/// The one-line hints of view.rs, git_panel, terminal_dock_ui, ide_search_ui
/// and ide_activity_ui were this chain at one of four sizes.
#[test]
fn a_muted_hint_is_the_text_of_its_role() {
    for (size, role) in [
        (10.0, TextRole::Micro),
        (11.0, TextRole::Small),
        (12.0, TextRole::Caption),
        (14.0, TextRole::Body),
    ] {
        let value = "Elige un archivo en Control de versiones para ver sus cambios.";
        assert_same(
            "muted hint",
            Element::text(value)
                .font_family(FontFamily::Sans)
                .font_size(size)
                .foreground(theme::FOREGROUND_SUBTLE),
            Text::new(value, theme::component_palette())
                .role(role)
                .muted()
                .render(),
        );
    }
}
