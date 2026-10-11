//! Where a link of the conversation opens, and the menu of one link.
//!
//! limn's `Markdown` finds the addresses, paints them and gives each an
//! action; `rich_text` scopes those actions. This module only decides the
//! destination: nothing here fetches, and every address was already reduced
//! to plain HTTP(S) by `Markdown::web_link`.

use std::net::Ipv4Addr;

use openide_ui::{
    Component, ContextMenu, ContextMenuEntry, ContextMenuItem, DropdownOption, Element, Frame,
    Icon, Markdown,
};
use serde::{Deserialize, Serialize};

use crate::{rich_text::web_link_url_from_action, theme};

pub(crate) const MENU_ACTION_PREFIX: &str = "chat_link_menu:";
pub(crate) const SETTING_ACTION_PREFIX: &str = "chat_links:";

/// The two places a link can open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinkTarget {
    System,
    Integrated,
}

/// The persisted choice of Settings › Navegador.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatLinkOpening {
    /// This machine's addresses in the dock, the rest in the system browser.
    #[default]
    Auto,
    System,
    Integrated,
}

impl ChatLinkOpening {
    const ALL: [Self; 3] = [Self::Auto, Self::System, Self::Integrated];

    fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::System => "system",
            Self::Integrated => "integrated",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automático",
            Self::System => "Navegador del sistema",
            Self::Integrated => "Navegador integrado",
        }
    }

    pub(crate) fn from_action_id(id: &str) -> Option<Self> {
        let id = id.strip_prefix(SETTING_ACTION_PREFIX)?;
        Self::ALL.into_iter().find(|choice| choice.id() == id)
    }

    pub(crate) fn options(self) -> Vec<DropdownOption> {
        Self::ALL
            .into_iter()
            .map(|choice| {
                DropdownOption::new(
                    format!("{SETTING_ACTION_PREFIX}{}", choice.id()),
                    choice.label(),
                )
                .selected(choice == self)
            })
            .collect()
    }

    /// Where a plain click opens `url`; `other` (Ctrl held) picks the
    /// opposite place.
    pub(crate) fn target(self, url: &str, other: bool) -> LinkTarget {
        let integrated = match self {
            Self::Auto => is_local(url),
            Self::System => false,
            Self::Integrated => true,
        };
        if integrated != other {
            LinkTarget::Integrated
        } else {
            LinkTarget::System
        }
    }
}

/// The host of an address as the network sees it: lower case, and in
/// punycode when it is not ASCII, so a look-alike name cannot pass for
/// another site.
pub(crate) fn host(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// An address served by this machine: localhost and its subdomains, the
/// IPv4 loopback block, 0.0.0.0 (what a dev server prints when it listens on
/// every interface) and `[::1]`.
pub(crate) fn is_local(url: &str) -> bool {
    let host = host(url);
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "[::1]"
        || host
            .parse::<Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// The link under a secondary click, kept while its menu is open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ChatLinkMenu {
    /// The destination, normalized.
    pub url: String,
    /// The words of the link as painted.
    pub text: String,
}

impl ChatLinkMenu {
    /// The link whose action is `id` in a laid out frame.
    pub(crate) fn of(frame: &Frame, id: &str) -> Option<Self> {
        let url = web_link_url_from_action(id)?;
        let fragments: Vec<_> = frame
            .regions
            .iter()
            .filter(|region| region.id == id)
            .map(|region| region.rect)
            .collect();
        // A wrapped link is several fragments; each painted run of text
        // lies inside the fragment that carries the action.
        let text = frame
            .text_layouts()
            .iter()
            .flat_map(|block| &block.runs)
            .filter(|run| {
                let center = openide_ui::Point::new(
                    run.rect.x + run.rect.width / 2.0,
                    run.rect.y + run.rect.height / 2.0,
                );
                fragments.iter().any(|rect| rect.contains(center))
            })
            .map(|run| run.text.as_str())
            .collect();
        Some(Self { url, text })
    }

    /// The text says something other than the address it opens.
    fn text_differs(&self) -> bool {
        !self.text.is_empty()
            && self.text != self.url
            && Markdown::web_link(&self.text).as_deref() != Some(self.url.as_str())
    }

    fn entries(&self, opening: ChatLinkOpening, dock_available: bool) -> Vec<ContextMenuEntry> {
        let default = opening.target(&self.url, false);
        let item = |operation: &str, label: &str, icon| {
            ContextMenuItem::new(format!("{MENU_ACTION_PREFIX}{operation}"), label).icon(icon)
        };
        let open = |operation, label, icon, target| {
            let item = item(operation, label, icon);
            if target == default {
                item
            } else {
                item.shortcut("Ctrl+Clic")
            }
        };
        let mut entries = vec![
            // The real host comes first: the words of a link can name
            // another site.
            ContextMenuEntry::Item(
                item("host", &host(&self.url), Markdown::link_icon(&self.url)).enabled(false),
            ),
            ContextMenuEntry::Separator,
            ContextMenuEntry::Item(
                open(
                    "integrated",
                    "Abrir en el navegador integrado",
                    Icon::PanelRight,
                    LinkTarget::Integrated,
                )
                .enabled(dock_available),
            ),
            ContextMenuEntry::Item(open(
                "system",
                "Abrir en el navegador del sistema",
                Icon::Globe,
                LinkTarget::System,
            )),
        ];
        entries.push(ContextMenuEntry::Separator);
        entries.push(ContextMenuEntry::Item(item("copy", "Copiar enlace", Icon::Copy)));
        if self.text_differs() {
            entries.push(ContextMenuEntry::Item(item(
                "copy_text",
                "Copiar texto del enlace",
                Icon::Text,
            )));
        }
        entries
    }

    /// `root` with this link's menu. The host wraps the window only while a
    /// secondary click landed on a link, so text keeps its own copy menu.
    pub(crate) fn attach(
        &self,
        root: Element,
        opening: ChatLinkOpening,
        dock_available: bool,
    ) -> Element {
        ContextMenu::new(
            "chat_link_menu",
            root,
            self.entries(opening, dock_available),
            theme::menu_palette(),
        )
        .render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(entries: &[ContextMenuEntry]) -> Vec<String> {
        // Entries are opaque outside limn; their debug form names the label.
        entries
            .iter()
            .map(|entry| match entry {
                ContextMenuEntry::Separator => "—".to_owned(),
                ContextMenuEntry::Item(item) => {
                    let debug = format!("{item:?}");
                    let label = debug.split("label: \"").nth(1).unwrap();
                    let enabled = debug.contains("enabled: true");
                    format!(
                        "{}{}",
                        label.split('"').next().unwrap(),
                        if enabled { "" } else { " (off)" }
                    )
                }
            })
            .collect()
    }

    #[test]
    fn this_machine_is_local_and_look_alikes_are_not() {
        for url in [
            "http://localhost:5173/",
            "http://LOCALHOST:5173",
            "http://app.localhost:3000/a",
            "http://127.0.0.1:8080/",
            "http://127.1/",
            "http://0.0.0.0:3000/",
            "http://[::1]:4000/x",
        ] {
            assert!(is_local(url), "{url}");
        }
        for url in [
            "https://localhost.evil.test/",
            "https://127.evil.test/",
            "https://example.test/?next=localhost",
            "https://notlocalhost/",
            "http://192.168.1.4/",
            "no es una dirección",
        ] {
            assert!(!is_local(url), "{url}");
        }
    }

    #[test]
    fn the_setting_and_ctrl_decide_where_a_click_opens() {
        use ChatLinkOpening::*;
        use LinkTarget::{Integrated as Dock, System as Os};
        let local = "http://localhost:5173/";
        let web = "https://github.com/a/b";
        for (opening, url, plain, ctrl) in [
            (Auto, local, Dock, Os),
            (Auto, web, Os, Dock),
            (System, local, Os, Dock),
            (System, web, Os, Dock),
            (Integrated, local, Dock, Os),
            (Integrated, web, Dock, Os),
        ] {
            assert_eq!(opening.target(url, false), plain, "{opening:?} {url}");
            assert_eq!(opening.target(url, true), ctrl, "{opening:?} {url} ctrl");
        }
        assert_eq!(ChatLinkOpening::default(), Auto);
        for choice in ChatLinkOpening::ALL {
            let id = format!("{SETTING_ACTION_PREFIX}{}", choice.id());
            assert_eq!(ChatLinkOpening::from_action_id(&id), Some(choice));
        }
        assert_eq!(ChatLinkOpening::from_action_id("chat_links:otro"), None);
    }

    #[test]
    fn the_host_is_shown_in_punycode_and_without_port_or_path() {
        assert_eq!(host("https://GitHub.com:443/a/b?x=1"), "github.com");
        assert_eq!(host("http://localhost:5173/"), "localhost");
        // Cyrillic "а" in "аpple.com".
        assert_eq!(host("https://аpple.com/"), "xn--pple-43d.com");
        assert_eq!(host("javascript:alert(1)"), "");
    }

    #[test]
    fn the_menu_names_the_host_and_offers_the_text_only_when_it_differs() {
        let named = ChatLinkMenu {
            url: "https://evil.test/login".into(),
            text: "github.com/openai".into(),
        };
        assert_eq!(
            labels(&named.entries(ChatLinkOpening::Auto, true)),
            [
                "evil.test (off)",
                "—",
                "Abrir en el navegador integrado",
                "Abrir en el navegador del sistema",
                "—",
                "Copiar enlace",
                "Copiar texto del enlace",
            ]
        );
        let bare = ChatLinkMenu {
            url: "http://localhost:5173/".into(),
            text: "http://localhost:5173".into(),
        };
        assert_eq!(
            labels(&bare.entries(ChatLinkOpening::Auto, false)),
            [
                "localhost (off)",
                "—",
                "Abrir en el navegador integrado (off)",
                "Abrir en el navegador del sistema",
                "—",
                "Copiar enlace",
            ]
        );
    }
}
