//! Embedded provider identity artwork shared by native rendering and exports.
//!
//! SVGL is the primary source; MiniMax and Z.ai use the MIT-licensed Lobe
//! fallback. Provenance and exact upstream hashes
//! live beside the assets; providers never download icons while painting.

use openide_ui::Icon;

pub(crate) fn icon_for_provider(provider_id: &str) -> Icon {
    match provider_id {
        "codex" => CODEX,
        "openai" => OPENAI,
        "openrouter" => OPENROUTER,
        "opencode" => OPENCODE,
        "nvidia" => NVIDIA,
        "deepseek" => DEEPSEEK,
        "minimax" => MINIMAX,
        "anthropic" => ANTHROPIC,
        // The Claude Code subscription, as the usage panel lists it.
        "claude-code" => CLAUDE,
        "groq" => GROQ,
        "mistral" => MISTRAL,
        "xai" => XAI,
        "zai" | "z-ai" | "z.ai" | "glm" => ZAI,
        _ => Icon::Grid,
    }
}

/// The mark of an agent CLI; one without its own artwork is a terminal.
pub(crate) fn icon_for_agent_cli(agent_id: &str) -> Icon {
    match agent_id {
        "claude" => CLAUDE,
        "codex" => CODEX,
        "opencode" => OPENCODE,
        _ => Icon::Terminal,
    }
}

/// An ACP agent's mark from the brand catalog, found by its name.
pub(crate) fn icon_for_acp_agent(name: &str) -> Icon {
    crate::brand_icons::search(name, 1)
        .first()
        .and_then(|brand| brand.icon(!crate::theme_mode::light()))
        .unwrap_or(Icon::Terminal)
}

/// Artwork limn does not ship: provider marks and the application's own
/// glyphs. Each is an SVG drawn with `currentColor`, so it follows the theme.
pub(crate) const CODEX: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/codex.svg"));
pub(crate) const OPENAI: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/openai.svg"));
pub(crate) const OPENROUTER: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/openrouter.svg"));
pub(crate) const OPENCODE: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/opencode.svg"));
pub(crate) const NVIDIA: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/nvidia.svg"));
pub(crate) const DEEPSEEK: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/deepseek.svg"));
pub(crate) const MINIMAX: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/minimax.svg"));
pub(crate) const ZAI: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/zai.svg"));
pub(crate) const ANTHROPIC: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/anthropic.svg"));
pub(crate) const CLAUDE: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/claude.svg"));
pub(crate) const GROQ: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/groq.svg"));
pub(crate) const MISTRAL: Icon =
    Icon::Custom(include_str!("../assets/providers/icons/mistral.svg"));
pub(crate) const XAI: Icon = Icon::Custom(include_str!("../assets/providers/icons/xai.svg"));
pub(crate) const CONTEXT_RING: Icon =
    Icon::Custom(include_str!("../assets/openide/context-ring.svg"));

#[cfg(test)]
pub(crate) const PROVIDER_ICONS: [Icon; 13] = [
    CLAUDE,
    CODEX,
    OPENAI,
    OPENROUTER,
    OPENCODE,
    NVIDIA,
    DEEPSEEK,
    MINIMAX,
    ZAI,
    ANTHROPIC,
    GROQ,
    MISTRAL,
    XAI,
];

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{fs, path::Path};

    fn svg_for_icon(icon: Icon) -> Option<&'static str> {
        match icon {
            Icon::Custom(source) => Some(source),
            _ => None,
        }
    }

    #[test]
    fn known_provider_logos_and_unknown_fallback() {
        for (provider, expected) in [
            ("codex", CODEX),
            ("openai", OPENAI),
            ("openrouter", OPENROUTER),
            ("opencode", OPENCODE),
            ("nvidia", NVIDIA),
            ("deepseek", DEEPSEEK),
            ("minimax", MINIMAX),
            ("zai", ZAI),
            ("z-ai", ZAI),
            ("z.ai", ZAI),
            ("glm", ZAI),
            ("anthropic", ANTHROPIC),
            ("groq", GROQ),
            ("mistral", MISTRAL),
            ("xai", XAI),
        ] {
            assert_eq!(icon_for_provider(provider), expected, "{provider}");
            assert!(svg_for_icon(expected).is_some());
        }
        // A custom endpoint or a similarly named brand must not inherit a logo.
        for provider in ["custom", "future-provider", "openai-compatible", "zaia"] {
            assert_eq!(icon_for_provider(provider), Icon::Grid);
        }
        assert!(svg_for_icon(Icon::Grid).is_none());
        assert!(svg_for_icon(Icon::Code).is_none());
    }

    #[test]
    fn every_named_provider_in_the_registry_has_distinct_brand_artwork() {
        let registry: serde_json::Value =
            serde_json::from_str(include_str!("provider-definitions.json")).unwrap();
        let mut seen = std::collections::HashSet::new();
        for provider in registry["providers"].as_array().unwrap() {
            let id = provider["id"].as_str().unwrap();
            let icon = icon_for_provider(id);
            if id == "custom" {
                assert_eq!(icon, Icon::Grid);
                continue;
            }
            assert_ne!(
                icon,
                Icon::Grid,
                "provider {id} is missing its brand artwork"
            );
            assert!(
                svg_for_icon(icon).is_some(),
                "provider {id} is not embedded"
            );
            assert!(
                seen.insert(icon),
                "provider {id} reused another provider's mark"
            );
        }
        // One embedded mark belongs to an agent CLI rather than a provider.
        assert!(!seen.contains(&CLAUDE));
        assert_eq!(icon_for_agent_cli("claude"), CLAUDE);
        assert_eq!(icon_for_agent_cli("codex"), CODEX);
        assert_eq!(icon_for_agent_cli("opencode"), OPENCODE);
        assert_eq!(icon_for_agent_cli("other"), Icon::Terminal);
        assert_eq!(seen.len() + 1, PROVIDER_ICONS.len());
    }

    #[test]
    fn openrouter_removes_the_upstream_dangling_clip_without_changing_paths() {
        let original = include_str!("../assets/providers/svgl/openrouter_dark.svg");
        let embedded = svg_for_icon(OPENROUTER).unwrap();
        assert!(original.contains("clip-path=\"url(#clip0_205_3)\""));
        assert!(!original.contains("id=\"clip0_205_3\""));
        assert!(!embedded.contains("clip-path="));
        let paths = |source: &str| {
            source
                .split(" d=\"")
                .skip(1)
                .map(|tail| tail.split('"').next().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        let expected = paths(original);
        assert_eq!(expected.len(), 4);
        assert_eq!(
            paths(embedded),
            expected,
            "native paths must retain upstream geometry"
        );
    }

    #[test]
    fn provider_svg_sources_are_offline_bounded_and_provenance_hashes_match() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/providers");
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../assets/providers/SOURCE.json")).unwrap();
        let assets = manifest["assets"].as_array().unwrap();
        assert_eq!(assets.len(), PROVIDER_ICONS.len());
        for asset in assets {
            for field in ["original", "embedded"] {
                let path = asset[field].as_str().unwrap();
                let data = fs::read(root.join(path)).unwrap();
                assert!(data.len() < 21 * 1024, "unbounded provider SVG: {path}");
                let hash = format!("{:x}", Sha256::digest(&data));
                let expected = asset[format!("{field}_sha256")].as_str().unwrap();
                assert_eq!(hash, expected, "{path}");
            }
        }
        for icon in PROVIDER_ICONS {
            let source = svg_for_icon(icon).unwrap();
            for forbidden in [
                "<script",
                "<image",
                "href=",
                "<foreignObject",
                "@import",
                "url(",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "external or active SVG: {icon:?}"
                );
            }
            assert!(source.contains("currentColor"));
            let parsed = resvg::usvg::Tree::from_str(
                &source.replace("currentColor", "#FFFFFF"),
                &Default::default(),
            )
            .unwrap();
            assert!(parsed.size().width().is_finite() && parsed.size().width() > 0.0);
            assert!(parsed.size().height().is_finite() && parsed.size().height() > 0.0);
        }
        assert_eq!(assets.iter().filter(|a| a["source"] == "SVGL").count(), 11);
        assert!(assets
            .iter()
            .any(|a| a["provider"] == "minimax" && a["source"] != "SVGL"));
        assert!(assets
            .iter()
            .any(|a| a["provider"] == "zai" && a["source"] != "SVGL"));
    }

    #[test]
    #[ignore = "writes provider icon visual review artifacts"]
    fn export_provider_icon_visual_review() {
        use openide_ui::{Color, DrawCommand, FontFamily, Frame, Point, Rect};
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/provider-icons-visual-qa");
        fs::create_dir_all(&root).unwrap();
        for (theme, background, foreground) in [
            ("dark", Color::rgb(24, 24, 24), Color::rgb(222, 222, 222)),
            ("light", Color::rgb(246, 246, 246), Color::rgb(38, 38, 38)),
        ] {
            let mut frame = Frame::default();
            frame.commands.push(DrawCommand::FillRect {
                rect: Rect::new(0.0, 0.0, 416.0, 476.0),
                color: background,
                radius: 0.0,
            });
            for (index, (label, icon)) in [
                ("Codex", CODEX),
                ("OpenAI", OPENAI),
                ("OpenRouter", OPENROUTER),
                ("OpenCode", OPENCODE),
                ("NVIDIA", NVIDIA),
                ("DeepSeek", DEEPSEEK),
                ("MiniMax", MINIMAX),
                ("GLM · Z.ai", ZAI),
                ("Anthropic", ANTHROPIC),
                ("Groq", GROQ),
                ("Mistral", MISTRAL),
                ("xAI", XAI),
            ]
            .into_iter()
            .enumerate()
            {
                let y = 16.0 + index as f32 * 38.0;
                frame.commands.push(DrawCommand::Icon {
                    rect: Rect::new(16.0, y, 32.0, 32.0),
                    icon,
                    tint: foreground,
                });
                frame.commands.push(DrawCommand::Text {
                    position: Point::new(62.0, y + 7.0),
                    text: label.into(),
                    color: foreground,
                    font_size: 13.0,
                    font_family: FontFamily::Sans,
                });
                frame.commands.push(DrawCommand::Icon {
                    rect: Rect::new(328.0, y, 32.0, 32.0),
                    icon,
                    tint: Color::rgba(foreground.r, foreground.g, foreground.b, 100),
                });
            }
            let path = root.join(format!("providers-{theme}.svg"));
            crate::settings_ui::visual_qa::write_native_frame_svg(&frame, &path, 416.0, 476.0)
                .unwrap();
            let svg = fs::read_to_string(&path).unwrap();
            let tree = resvg::usvg::Tree::from_str(
                &svg,
                &crate::settings_ui::visual_qa::native_svg_options(),
            )
            .unwrap();
            for scale in [1, 2] {
                let mut image = resvg::tiny_skia::Pixmap::new(416 * scale, 476 * scale).unwrap();
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::from_scale(scale as f32, scale as f32),
                    &mut image.as_mut(),
                );
                image
                    .save_png(root.join(format!("providers-{theme}-{scale}x.png")))
                    .unwrap();
            }
        }
    }
}
