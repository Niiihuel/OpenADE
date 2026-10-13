use super::*;
use crate::browser_runtime::{BrowserCommand, BrowserScope, BrowserSnapshot};
use crate::chat_links::{ChatLinkMenu, ChatLinkOpening};
use openide_ui::{HitKind, Key, Point, Rect};

const SIZE: Size = Size::new(1100.0, 700.0);
const PROSE: &str = "Puedes abrirlo en http://localhost:5173. El código está en \
[el repositorio](https://github.com/openai/codex) y la guía en https://example.test/docs.\n\n\
Nada de esto abre: [uno](javascript:alert(1)), [dos](file:///etc/passwd), \
[tres](vscode://file/x), mailto:a@b.test y `https://en.codigo.test`.\n\n\
```\nhttps://en.bloque.test\n```";

struct Fixture {
    _root: tempfile::TempDir,
    app: AgentsWindow,
    ctx: egui::Context,
    browser: crate::browser_runtime::tests::BrowserCommands,
}

/// A conversation with links and a browser tab whose context is a fixture:
/// nothing here starts a browser or reaches the network.
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let mut app = AgentsWindow::with_data_root(root.path().join("data"));
    app.view.workspace = project.canonicalize().unwrap().display().to_string();
    app.view.selected_session = Some("links-a".into());
    app.view.main_page = MainPage::Workspace;
    app.view.show_settings = false;
    app.view.show_side_pane = false;
    app.viewport_width = SIZE.width;
    app.viewport_height = SIZE.height;
    app.reduced_motion = true;
    app.tree.set_reduced_motion(true);
    app.view.transcript = vec![
        TranscriptItem::User { content: "¿Dónde lo veo? https://example.test/pedido".into(), images: vec![] },
        TranscriptItem::Assistant { content: PROSE.into(), streaming: false },
    ];
    let id = app.dock.open(&app.dock_scope(), DockTabKind::Browser);
    let (runtime, browser) = crate::browser_runtime::tests::fixture(
        BrowserScope {
            workspace: PathBuf::from(&app.view.workspace),
            session_id: "links-a".into(),
        },
        BrowserSnapshot {
            started: true,
            generation: 1,
            viewport_width: 640,
            viewport_height: 480,
            ..Default::default()
        },
    );
    app.browsers.insert(id.get(), runtime);
    app.layout_workspace_frame(SIZE);
    Fixture { _root: root, app, ctx: egui::Context::default(), browser }
}

impl Fixture {
    /// The first painted fragment of each link, with its destination.
    fn links(&self) -> Vec<(String, Rect, String)> {
        let mut links: Vec<(String, Rect, String)> = Vec::new();
        for region in &self.app.tree.frame().regions {
            if let Some(url) = crate::rich_text::web_link_url_from_action(&region.id) {
                if region.kind == HitKind::Button && !links.iter().any(|(id, ..)| *id == region.id) {
                    links.push((region.id.clone(), region.rect, url));
                }
            }
        }
        links
    }

    fn link(&self, url: &str) -> (String, Point) {
        let (id, rect, _) = self
            .links()
            .into_iter()
            .find(|(.., found)| found == url)
            .unwrap_or_else(|| panic!("no link to {url}: {:?}", self.links()));
        (id, Point::new(rect.x + rect.width - 3.0, rect.y + rect.height / 2.0))
    }

    /// A point in the words just before a link, and one just after it.
    fn around(&self, url: &str) -> (Point, Point) {
        let (id, first, _) = self.links().into_iter().find(|(.., found)| found == url).unwrap();
        // The glyph is a fragment of its own; the words end at the last one.
        let regions = &self.app.tree.frame().regions;
        let last = regions.iter().rev().find(|region| region.id == id).unwrap().rect;
        (
            Point::new(first.x - 40.0, first.y + first.height / 2.0),
            Point::new(last.x + last.width + 24.0, last.y + last.height / 2.0),
        )
    }

    fn click(&mut self, url: &str) {
        let (_, point) = self.link(url);
        self.app.tree.handle_event(UiEvent::PointerDown(point));
        for action in self.app.tree.handle_event(UiEvent::PointerUp(point)) {
            self.app.apply_action(action, &self.ctx);
        }
    }

    fn choose(&mut self, operation: &str) {
        let id = format!("chat_link_menu:{operation}");
        self.app.apply_action(UiAction::Clicked { id }, &self.ctx);
    }

    /// What left the app through egui since the last call.
    fn output(&self) -> Vec<String> {
        self.ctx
            .output_mut(|output| std::mem::take(&mut output.commands))
            .into_iter()
            .map(|command| match command {
                egui::OutputCommand::OpenUrl(open) => format!("open {}", open.url),
                egui::OutputCommand::CopyText(text) => format!("copy {text}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// The address the integrated browser was sent to, if any.
    fn navigated(&mut self) -> Option<String> {
        self.app.update_browser(&self.ctx);
        match self.browser.finish_next() {
            Some(BrowserCommand::Navigate { url }) => Some(url),
            _ => None,
        }
    }

    fn menu_rows(&self) -> Vec<String> {
        self.app
            .tree
            .frame()
            .regions
            .iter()
            .filter(|region| region.id.starts_with("__context_menu:item:"))
            .map(|region| region.id.clone())
            .collect()
    }
}

#[test]
fn only_web_addresses_outside_code_are_links() {
    let fixture = fixture();
    let urls: Vec<_> = fixture.links().into_iter().map(|(.., url)| url).collect();
    assert_eq!(
        urls,
        [
            "https://example.test/pedido",
            "http://localhost:5173/",
            "https://github.com/openai/codex",
            "https://example.test/docs",
        ]
    );
}

#[test]
fn a_click_opens_local_addresses_in_the_dock_and_the_rest_in_the_system_browser() {
    let mut fixture = fixture();
    fixture.click("https://github.com/openai/codex");
    assert_eq!(fixture.output(), ["open https://github.com/openai/codex"]);
    assert_eq!(fixture.navigated(), None);

    fixture.click("http://localhost:5173/");
    assert!(fixture.output().is_empty());
    assert_eq!(fixture.navigated().as_deref(), Some("http://localhost:5173/"));
    assert_eq!(fixture.app.active_browser_id().is_some(), true);
}

#[test]
fn the_setting_moves_every_click_and_is_saved() {
    let mut fixture = fixture();
    fixture.app.view.show_settings = true;
    fixture.app.apply_action(clicked(SettingsPage::Browser.action_id()), &fixture.ctx);
    assert_eq!(fixture.app.view.settings_page, SettingsPage::Browser);
    fixture.app.apply_action(clicked("settings_choice_toggle:chat_links"), &fixture.ctx);
    assert_eq!(fixture.app.view.settings_choice_menu_open.as_deref(), Some("chat_links"));
    fixture.app.apply_action(clicked("chat_links:system"), &fixture.ctx);
    assert_eq!(fixture.app.view.settings_choice_menu_open, None);
    assert_eq!(fixture.app.view.chat_link_opening, ChatLinkOpening::System);
    let saved = crate::settings_preferences::SettingsPreferences::load(&fixture.app.data_root);
    assert_eq!(saved.chat_links, ChatLinkOpening::System);
    fixture.app.view.show_settings = false;
    fixture.app.layout_workspace_frame(SIZE);

    fixture.click("http://localhost:5173/");
    assert_eq!(fixture.output(), ["open http://localhost:5173/"]);
    assert_eq!(fixture.navigated(), None);

    fixture.app.apply_action(clicked("chat_links:integrated"), &fixture.ctx);
    fixture.click("https://example.test/docs");
    assert!(fixture.output().is_empty());
    assert_eq!(fixture.navigated().as_deref(), Some("https://example.test/docs"));

    fixture.app.apply_action(clicked("chat_links:nada"), &fixture.ctx);
    assert_eq!(fixture.app.view.chat_link_opening, ChatLinkOpening::Integrated);
}

#[test]
fn a_secondary_click_on_a_link_opens_its_menu_and_each_row_acts_on_that_link() {
    let mut fixture = fixture();
    let (_, point) = fixture.link("https://github.com/openai/codex");
    let event = UiEvent::ContextMenu { position: point };
    assert!(fixture.app.handle_chat_link_menu_event(&event, &fixture.ctx));
    assert!(fixture.app.tree.context_menu_open());
    assert_eq!(
        fixture.app.view.chat_link_menu,
        Some(ChatLinkMenu {
            url: "https://github.com/openai/codex".into(),
            text: "el repositorio".into(),
        })
    );
    fixture.app.layout_workspace_frame(SIZE);
    // The host row is not a target: two places to open, the link, its text.
    assert_eq!(fixture.menu_rows().len(), 4, "{:?}", fixture.menu_rows());
    assert!(fixture.output().is_empty(), "opening a menu opens no address");

    fixture.choose("copy_text");
    assert_eq!(fixture.output(), ["copy el repositorio"]);
    assert_eq!(fixture.app.view.chat_link_menu, None);

    for (operation, expected) in [
        ("copy", vec!["copy https://github.com/openai/codex".to_owned()]),
        ("system", vec!["open https://github.com/openai/codex".to_owned()]),
        ("integrated", vec![]),
    ] {
        fixture.app.tree.dismiss_context_menu();
        fixture.app.layout_workspace_frame(SIZE);
        assert!(fixture.app.handle_chat_link_menu_event(&event, &fixture.ctx));
        fixture.choose(operation);
        assert_eq!(fixture.output(), expected, "{operation}");
    }
    assert_eq!(fixture.navigated().as_deref(), Some("https://github.com/openai/codex"));
    // The copied link, not its text, is announced.
    assert_eq!(
        fixture.app.view.link_copied.as_ref().map(|copied| copied.url.as_str()),
        Some("https://github.com/openai/codex")
    );

    // A row chosen with no link behind it does nothing.
    fixture.choose("system");
    assert!(fixture.output().is_empty());
}

#[test]
fn a_bare_address_offers_no_separate_text_and_prose_keeps_its_own_menu() {
    let mut fixture = fixture();
    let (_, point) = fixture.link("http://localhost:5173/");
    let event = UiEvent::ContextMenu { position: point };
    assert!(fixture.app.handle_chat_link_menu_event(&event, &fixture.ctx));
    assert_eq!(
        fixture.app.view.chat_link_menu.as_ref().map(|menu| menu.text.as_str()),
        Some("http://localhost:5173")
    );
    fixture.app.layout_workspace_frame(SIZE);
    assert_eq!(fixture.menu_rows().len(), 3, "{:?}", fixture.menu_rows());

    // The words before the link are prose: the link's menu is dropped and
    // the event goes on to the tree, which offers its text menu.
    let prose = UiEvent::ContextMenu { position: fixture.around("http://localhost:5173/").0 };
    assert!(!fixture.app.handle_chat_link_menu_event(&prose, &fixture.ctx));
    assert_eq!(fixture.app.view.chat_link_menu, None);
    assert!(!fixture.app.tree.context_menu_open());
    fixture.app.tree.handle_event(prose.clone());
    assert!(fixture.app.tree.context_menu_open());
    fixture.app.tree.dismiss_context_menu();
    assert!(!fixture.app.handle_chat_link_menu_event(&prose, &fixture.ctx));
}

#[test]
fn links_take_focus_in_reading_order_and_answer_enter_and_the_menu_key() {
    let mut fixture = fixture();
    let order: Vec<_> = fixture.links().into_iter().map(|(id, ..)| id).collect();
    let (first, _) = fixture.link("http://localhost:5173/");
    assert!(fixture.app.tree.focus(&first));
    let key = |key| UiEvent::Key { key, shift: false, ctrl: false };
    fixture.app.tree.handle_event(key(Key::Tab));
    let focused = fixture.app.tree.focused_id().unwrap().to_owned();
    let next = order.iter().position(|id| *id == first).unwrap() + 1;
    assert_eq!(focused, order[next]);

    for action in fixture.app.tree.handle_event(key(Key::Enter)) {
        fixture.app.apply_action(action, &fixture.ctx);
    }
    assert_eq!(fixture.output(), ["open https://github.com/openai/codex"]);

    assert!(fixture
        .app
        .handle_chat_link_menu_event(&UiEvent::ContextMenuKeyboard, &fixture.ctx));
    assert!(fixture.app.tree.context_menu_open());
    assert_eq!(
        fixture.app.view.chat_link_menu.as_ref().map(|menu| menu.url.as_str()),
        Some("https://github.com/openai/codex")
    );
}

#[test]
fn nothing_but_plain_http_leaves_the_app() {
    let mut fixture = fixture();
    for address in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "vscode://file/x",
        "https://user:clave@example.test/",
        "https://example.test/\u{7}",
        "data:text/html,<script>1</script>",
    ] {
        for target in [crate::chat_links::LinkTarget::System, crate::chat_links::LinkTarget::Integrated] {
            fixture.app.open_chat_link(address, target, &fixture.ctx);
        }
        let action = format!("chat_link_open:0:0:{address}");
        fixture.app.apply_action(clicked(action), &fixture.ctx);
        fixture.app.view.chat_link_menu =
            Some(ChatLinkMenu { url: address.into(), text: "GitHub".into() });
        fixture.choose("system");
        assert!(fixture.output().is_empty(), "{address}");
        assert_eq!(fixture.navigated(), None, "{address}");
    }
}

#[test]
fn a_selection_across_a_link_copies_the_paragraph_as_written() {
    let mut fixture = fixture();
    let (start, end) = fixture.around("http://localhost:5173/");
    fixture.app.tree.handle_event(UiEvent::PointerDown(start));
    fixture.app.tree.handle_event(UiEvent::PointerMove(end));
    fixture.app.tree.handle_event(UiEvent::PointerUp(end));
    let selected = fixture.app.tree.selected_text().unwrap_or_default().to_owned();
    assert!(selected.contains("http://localhost:5173."), "{selected:?}");
    assert!(fixture.output().is_empty(), "dragging over a link opens nothing");
}

fn clicked(id: impl Into<String>) -> UiAction {
    UiAction::Clicked { id: id.into() }
}
