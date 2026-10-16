use super::*;
use openide_ui::{DrawCommand, Size, UiTree};
use std::collections::HashSet;

#[test]
fn only_vendored_svg_artwork_is_exempt_from_theme_ink() {
    assert!(!is_brand_icon(&Icon::Custom(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"><path d=\"M0 0h1v1z\"/></svg>"
    )));
    assert!(!is_brand_icon(&Icon::Search));
    for entry in catalog() {
        for dark in [false, true] {
            if let Some(icon) = entry.icon(dark) {
                assert!(is_brand_icon(&icon), "{} (dark={dark})", entry.title);
            }
        }
    }
}

#[test]
fn complete_catalog_retains_aliases_unique_stable_ids_and_bounded_search() {
    assert_eq!(catalog().len(), 669);
    let mut ids = HashSet::new();
    for entry in catalog() {
        assert!(ids.insert(entry.id));
        assert_eq!(entry.id.len(), 29);
        assert!(
            entry.id.starts_with("svgl-")
                && entry.id[5..].bytes().all(|byte| byte.is_ascii_hexdigit())
        );
        assert_eq!(get(entry.id).unwrap().title, entry.title);
        assert!(!entry.aliases.is_empty() && !entry.categories.is_empty());
    }
    let pycharm = search("PyCharm", 20);
    assert_eq!(pycharm.len(), 1);
    assert_eq!(pycharm[0].aliases.len(), 2);
    assert_eq!(search("", usize::MAX).len(), 128);
    assert!(search("this-brand-does-not-exist-3918", 20).is_empty());
    assert!(
        search("slack", 20)
            .iter()
            .any(|entry| entry.title == "Slack")
    );
    assert!(get("svgl-../../private").is_none());
}

#[test]
fn search_prioritizes_exact_names_then_prefixes_and_keeps_ties_in_source_order() {
    let matches = search("git", 128);
    assert_eq!(matches.first().unwrap().title, "Git");
    let github = matches
        .iter()
        .position(|entry| entry.title == "GitHub")
        .unwrap();
    for (index, entry) in matches.iter().enumerate() {
        if entry.title.to_lowercase().contains("git")
            && !entry.title.to_lowercase().starts_with("git")
        {
            assert!(github < index, "{} must follow name prefixes", entry.title);
        }
    }
    let prefixes = |entry: &&BrandEntry| {
        entry.aliases.iter().any(|alias| {
            let name = alias.to_lowercase();
            name.starts_with("git") && name != "git"
        })
    };
    assert_eq!(
        matches
            .iter()
            .copied()
            .filter(prefixes)
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        catalog()
            .iter()
            .filter(prefixes)
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
    );
    let alias = catalog()
        .iter()
        .find_map(|entry| {
            entry
                .aliases
                .iter()
                .find(|alias| !alias.eq_ignore_ascii_case(entry.title))
        })
        .unwrap();
    assert!(
        search(alias, 128)
            .first()
            .unwrap()
            .aliases
            .iter()
            .any(|name| name.eq_ignore_ascii_case(alias))
    );
    assert_eq!(
        search("", 128)
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        catalog()
            .iter()
            .take(128)
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn detection_requires_unique_exact_endpoint_host_and_refuses_ambiguous_or_misleading_names() {
    let slack = catalog()
        .iter()
        .find(|entry| entry.title == "Slack")
        .unwrap();
    let host = website(slack.website)
        .unwrap()
        .host_str()
        .unwrap()
        .to_owned();
    let endpoint = format!("https://{host}/mcp");
    let different_website = catalog()
        .iter()
        .find(|entry| entry.title == "Figma")
        .unwrap()
        .website;
    assert_eq!(
        detect(&BrandHints {
            endpoint_url: Some(&endpoint),
            ..Default::default()
        })
        .unwrap()
        .id,
        slack.id
    );
    for hints in [
        BrandHints {
            display_name: "Slack-compatible",
            ..Default::default()
        },
        BrandHints {
            package_names: &["@other/mcp-server-slack"],
            ..Default::default()
        },
        BrandHints {
            executable: Some("mcp-server-slack"),
            ..Default::default()
        },
        BrandHints {
            endpoint_url: Some("https://slack.com.evil.test/mcp"),
            ..Default::default()
        },
        BrandHints {
            endpoint_url: Some("https://evil.test/slack.com/mcp"),
            ..Default::default()
        },
        BrandHints {
            endpoint_url: Some("https://private@slack.com/mcp"),
            ..Default::default()
        },
        BrandHints {
            endpoint_url: Some("https://github.com/mcp"),
            ..Default::default()
        },
        BrandHints {
            endpoint_url: Some(&endpoint),
            website_urls: &[different_website],
            ..Default::default()
        },
    ] {
        assert!(detect(&hints).is_none());
    }
    let hints = BrandHints {
        package_names: &["@other/mcp-server-slack"],
        ..Default::default()
    };
    assert!(candidates(&hints).contains(&BrandCandidate {
        id: slack.id,
        reason: BrandEvidence::PackageName
    }));
    assert!(
        candidates(&BrandHints {
            executable: Some("/usr/bin/python3"),
            ..Default::default()
        })
        .is_empty()
    );
    assert!(!same_website(
        "https://github.com/org/one",
        "https://github.com/org/two"
    ));
}

#[test]
fn local_auto_requires_an_exact_unique_website_and_corroborating_package_or_executable() {
    let entry = catalog()
        .iter()
        .find(|entry| entry.title == "Figma")
        .unwrap();
    let websites = [entry.website];
    for hints in [
        BrandHints {
            website_urls: &websites,
            package_names: &["@example/mcp-server-figma"],
            ..Default::default()
        },
        BrandHints {
            website_urls: &websites,
            executable: Some("/usr/bin/figma-mcp.exe"),
            ..Default::default()
        },
    ] {
        assert_eq!(detect(&hints).unwrap().id, entry.id);
    }
    for hints in [
        BrandHints {
            website_urls: &websites,
            ..Default::default()
        },
        BrandHints {
            website_urls: &websites,
            display_name: "figma",
            ..Default::default()
        },
        BrandHints {
            package_names: &["@example/mcp-server-figma"],
            ..Default::default()
        },
        BrandHints {
            website_urls: &websites,
            package_names: &["mcp-server-slack"],
            ..Default::default()
        },
        BrandHints {
            website_urls: &websites,
            package_names: &["mcp-server-figma-slack"],
            ..Default::default()
        },
        BrandHints {
            website_urls: &websites,
            executable: Some("npx"),
            ..Default::default()
        },
        BrandHints {
            website_urls: &websites,
            package_names: &["figma-mcp"],
            endpoint_url: Some("https://unrelated.example/mcp"),
            ..Default::default()
        },
    ] {
        assert!(detect(&hints).is_none());
    }
    let ambiguous = catalog()
        .iter()
        .find(|entry| entry.title == "Acrobat Reader")
        .unwrap()
        .website;
    assert!(
        detect(&BrandHints {
            website_urls: &[ambiguous],
            package_names: &["acrobat-reader"],
            ..Default::default()
        })
        .is_none()
    );
}

#[test]
fn every_vendored_svg_parses_without_external_resources_and_all_art_fits_the_shared_box() {
    let mut source_count = HashSet::new();
    let mut raster_count = HashSet::new();
    for entry in catalog() {
        for dark in [false, true] {
            match entry.artwork(dark) {
                BrandArtwork::Svg(source) => {
                    for forbidden in [
                        "<script",
                        "<image",
                        "<text",
                        "<foreignObject",
                        "<!DOCTYPE",
                        "<!ENTITY",
                        "@import",
                        "javascript:",
                        "data:",
                    ] {
                        assert!(
                            !source.contains(forbidden),
                            "{} contains {forbidden}",
                            entry.title
                        );
                    }
                    if source_count.insert(source) {
                        let tree = resvg::usvg::Tree::from_str(source, &Default::default())
                            .unwrap_or_else(|error| panic!("{}: {error}", entry.title));
                        assert!(
                            tree.size().width().is_finite() && tree.size().height().is_finite()
                        );
                        assert!(
                            !tree.root().children().is_empty(),
                            "{} is blank",
                            entry.title
                        );
                    }
                }
                BrandArtwork::Png(bytes) => {
                    raster_count.insert(bytes);
                    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
                        .unwrap();
                    assert_eq!((image.width(), image.height()), (64, 64));
                    assert!(image.to_rgba8().pixels().any(|pixel| pixel[3] > 0));
                }
            }
            let frame = UiTree::new(entry.element(dark)).layout(Size::new(64.0, 64.0));
            assert!(
                frame.commands.iter().any(|command| match command {
                    DrawCommand::Icon { rect, tint, .. } =>
                        rect.width == ICON_SIZE
                            && rect.height == ICON_SIZE
                            && *tint == Color::rgb(255, 255, 255),
                    DrawCommand::RasterImage { rect, .. } =>
                        rect.width == ICON_SIZE && rect.height == ICON_SIZE,
                    _ => false,
                }),
                "{}",
                entry.title
            );
        }
    }
    assert!(source_count.len() >= 826);
    assert_eq!(raster_count.len(), 5);
}

#[test]
fn native_renderer_keeps_brand_pixels_colored_and_uses_white_vertex_tint() {
    use eframe::egui;
    let entry = catalog()
        .iter()
        .find(|entry| entry.title == "Figma")
        .unwrap();
    for dark in [false, true] {
        let frame = UiTree::new(entry.element(dark)).layout(Size::new(32.0, 32.0));
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            limn_egui::paint_at(ui, &frame, egui::Pos2::ZERO);
        });
        let icon = output
            .textures_delta
            .set
            .iter()
            .find_map(|(id, writes)| {
                writes
                    .iter()
                    .find(|delta| delta.image.size() == [16, 16])
                    .map(|delta| (*id, delta))
            })
            .expect("native icon texture");
        let egui::ImageData::Color(image) = &icon.1.image;
        assert!(
            image
                .pixels
                .iter()
                .any(|pixel| pixel.a() > 100 && pixel.r().abs_diff(pixel.g()) > 50),
            "the source colors must survive native rasterization"
        );
        assert!(
            output.shapes.iter().any(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) if mesh.texture_id == icon.0 => mesh
                    .vertices
                    .iter()
                    .all(|vertex| vertex.color == egui::Color32::WHITE),
                _ => false,
            }),
            "the native painter must not multiply brand colors by text ink"
        );
        output.textures_delta.clear();
    }
}

#[test]
fn provenance_hashes_match_every_embedded_artifact() {
    let source: serde_json::Value =
        serde_json::from_str(include_str!("../assets/brands/svgl/SOURCE.json")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/brands/svgl");
    assert_eq!(source["upstream_entries"], 670);
    assert_eq!(source["selectable_artworks"], 669);
    assert_eq!(source["unique_upstream_svg"], 831);
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(include_bytes!("../assets/brands/svgl/catalog.json"))
        ),
        source["catalog_sha256"].as_str().unwrap()
    );
    for asset in source["assets"].as_array().unwrap() {
        for theme in ["light", "dark"] {
            let data = std::fs::read(
                root.join("artwork")
                    .join(asset[theme]["file"].as_str().unwrap()),
            )
            .unwrap();
            assert_eq!(
                format!("{:x}", Sha256::digest(&data)),
                asset[theme]["sha256"].as_str().unwrap()
            );
        }
    }
}
