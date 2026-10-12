//! Deterministic native-font chrome review. Never launches a server or browser.
use super::*;
use base64::Engine;
use eframe::egui;
use openide_ui::{DrawCommand, Frame, RasterImage, RasterImageFormat, Size, UiTree};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};

fn font_id(size: f32, family: FontFamily) -> egui::FontId {
    egui::FontId::new(
        size,
        match family {
            FontFamily::Sans | FontFamily::SansItalic => egui::FontFamily::Proportional,
            FontFamily::Monospace => egui::FontFamily::Monospace,
        },
    )
}

fn native_tree(root: Element) -> (egui::Context, UiTree) {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    ctx.begin_pass(egui::RawInput::default());
    let mut tree = UiTree::new(root);
    tree.set_reduced_motion(true);
    let fonts = ctx.clone();
    tree.set_text_measurer(move |text, size, family| {
        fonts.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.into(), font_id(size, family), egui::Color32::WHITE)
                .size()
                .x
        })
    });
    let fonts = ctx.clone();
    tree.set_text_height_measurer(move |size, family| {
        fonts.fonts_mut(|fonts| fonts.row_height(&font_id(size, family)))
    });
    (ctx, tree)
}

fn fixture_image() -> RasterImage {
    // A local static visual fixture, not a screenshot claimed to be a website.
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="640" height="600">
      <rect width="640" height="600" fill="#f7f8fb"/>
      <rect width="640" height="58" fill="#fff"/>
      <path d="M0 58H640" stroke="#e6e9ee"/>
      <text x="28" y="36" font-family="sans-serif" font-size="18" fill="#182233">Proyecto local</text>
      <text x="28" y="122" font-family="sans-serif" font-size="27" fill="#182233">Una vista de tu aplicación</text>
      <text x="28" y="154" font-family="sans-serif" font-size="15" fill="#607087">Fixture visual del navegador de OpenIDE.</text>
      <rect x="28" y="188" width="584" height="184" rx="12" fill="#fff" stroke="#e6e9ee"/>
      <text x="48" y="228" font-family="sans-serif" font-size="16" fill="#182233">Nombre del proyecto</text>
      <rect x="48" y="244" width="544" height="40" rx="7" fill="#f7f8fb" stroke="#d4dae4"/>
      <text x="60" y="270" font-family="sans-serif" font-size="15" fill="#182233">Mi proyecto</text>
      <rect x="48" y="304" width="108" height="38" rx="7" fill="#365ff4"/>
      <text x="67" y="329" font-family="sans-serif" font-size="14" fill="#fff">Guardar</text>
      <text x="28" y="422" font-family="sans-serif" font-size="15" fill="#607087">La página ocupa el área disponible del panel.</text>
    </svg>"##;
    let document =
        resvg::usvg::Tree::from_str(svg, &crate::settings_ui::visual_qa::native_svg_options())
            .unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(640, 600).unwrap();
    resvg::render(
        &document,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let bytes = pixmap.encode_png().unwrap();
    RasterImage::new(
        format!("{:x}", Sha256::digest(&bytes)),
        RasterImageFormat::Png,
        bytes.len() as u32,
        Arc::from(base64::engine::general_purpose::STANDARD.encode(bytes)),
        "Fixture visual del navegador",
    )
    .unwrap()
    .full_resolution()
}

fn export(frame: &Frame, size: Size, name: &str) {
    let directory = Path::new("/tmp/openide-browser-ui-review");
    std::fs::create_dir_all(directory).unwrap();
    let path = directory.join(format!("{name}.svg"));
    // A frame only names the live surface. For the review its place is taken
    // by the fixture, from the same corner and at its own size.
    let mut frame = frame.clone();
    for command in &mut frame.commands {
        if let DrawCommand::Surface { rect, opacity, .. } = command {
            *command = DrawCommand::RasterImage {
                rect: openide_ui::Rect::new(rect.x, rect.y, 640.0, 600.0),
                image: fixture_image(),
                opacity: *opacity,
                radius: 0.0,
            };
        }
    }
    crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, size.width, size.height)
        .unwrap();
    let document = resvg::usvg::Tree::from_str(
        &std::fs::read_to_string(&path).unwrap(),
        &crate::settings_ui::visual_qa::native_svg_options(),
    )
    .unwrap();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width as u32, size.height as u32).unwrap();
    resvg::render(
        &document,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap.save_png(path.with_extension("png")).unwrap();
}

fn browser_fixture() -> BrowserUiState {
    BrowserUiState {
        started: true,
        url: "http://localhost:3000/".into(),
        url_draft: "http://localhost:3000/".into(),
        can_go_back: true,
        title: "localhost".into(),
        tab_id: Some(71),
        live: true,
        ..Default::default()
    }
}

#[test]
#[ignore = "opt-in native-font browser toolbar, error and viewport review"]
fn export_browser_ui_visual_review() {
    for width in [240.0, 320.0, 640.0] {
        for mode in [
            "empty",
            "loading",
            "error-local",
            "page",
            "more",
            "error-startup",
            "error-missing",
            "notice",
        ] {
            let mut state = crate::view::tests::empty_state();
            state.browser = browser_fixture();
            match mode {
                "empty" => {
                    state.browser.url = "about:blank".into();
                    state.browser.url_draft.clear();
                }
                "loading" => {
                    state.browser.navigating = true;
                    state.browser.loading = true;
                }
                "error-local" => {
                    state.browser.url = "chrome-error://chromewebdata/".into();
                    state.browser.error = Some(
                        "net::ERR_CONNECTION_REFUSED at http://localhost:3000/".into(),
                    );
                }
                "more" => state.browser.more_open = true,
                "notice" => {
                    state.browser.engine_notice = Some("Falta icudtl.dat junto a libcef.so.".into());
                }
                "error-startup" => {
                    state.browser.started = false;
                    state.browser.error = Some("El navegador no pudo iniciarse: error while loading shared libraries: libglib-2.0.so.0".into());
                }
                "error-missing" => {
                    state.browser.started = false;
                    state.browser.browser_missing = true;
                    state.browser.error = Some("No se encontró Google Chrome, Chromium ni Microsoft Edge en este equipo. Instalá uno de ellos o indicá la ruta de su ejecutable en la variable OPENIDE_BROWSER_EXECUTABLE.".into());
                }
                _ => {}
            }
            let size = Size::new(width, 680.0);
            let root = build(&state, width).background(theme::BACKGROUND);
            let (ctx, mut tree) = native_tree(root);
            let frame = tree.layout(size);
            export(&frame, size, &format!("browser-{mode}-{width:.0}"));
            if mode == "page" && width == 320.0 {
                assert!(tree.focus("browser_url"));
                export(&tree.layout(size), size, "browser-address-focused-320");
            }
            ctx.end_pass().textures_delta.clear();
        }
    }

    for (name, mode) in [
        ("ide", crate::ide_workspace::WindowMode::Ide),
        ("agents", crate::ide_workspace::WindowMode::Agents),
    ] {
        for size in [Size::new(1440.0, 900.0), Size::new(1040.0, 720.0)] {
            let mut state = crate::view::ide_visual_tests::fixture();
            state.ide.mode = mode;
            state.show_terminal_panel = false;
            state.ide.bottom_open = false;
            state.show_side_pane = true;
            state.side_tab = crate::view::SideTab::Browser;
            state.right_dock_width = Some(if size.width < 1200.0 { 320.0 } else { 420.0 });
            state.dock_tabs = vec![crate::view::DockTabView {
                id: 71,
                title: "Navegador".into(),
                full_path: None,
                active: true,
                kind: crate::view::SideTab::Browser,
                closable: true,
            }];
            state.browser = browser_fixture();
            let (ctx, mut tree) = native_tree(crate::view::build_window_view(
                &state,
                size.width,
                size.height,
            ));
            let frame = tree.layout(size);
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "browser_url"));
            assert!(frame
                .regions
                .iter()
                .any(|region| region.id == "browser_viewport"));
            export(
                &frame,
                size,
                &format!("browser-{name}-{:.0}x{:.0}", size.width, size.height),
            );
            ctx.end_pass().textures_delta.clear();
        }
    }
}
