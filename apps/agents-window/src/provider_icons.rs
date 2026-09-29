//! Brand artwork for model providers with an implemented connection path.
//!
//! The SVGs are from Lobe Icons and are rasterized through the same mask and
//! tint pipeline as the rest of the native UI. This keeps the logo legible in
//! the dark theme without making the selector depend on a web icon package.

use openide_ui::Icon;

pub(crate) fn icon_for_provider(provider_id: &str) -> Icon {
    match provider_id {
        "codex" => Icon::Codex,
        "openai" => Icon::OpenAi,
        "openrouter" => Icon::OpenRouter,
        "deepseek" => Icon::DeepSeek,
        _ => Icon::Grid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_provider_logos_and_unknown_fallback() {
        assert_eq!(icon_for_provider("codex"), Icon::Codex);
        assert_eq!(icon_for_provider("openai"), Icon::OpenAi);
        assert_eq!(icon_for_provider("openrouter"), Icon::OpenRouter);
        assert_eq!(icon_for_provider("deepseek"), Icon::DeepSeek);
        assert_eq!(icon_for_provider("custom"), Icon::Grid);
        assert_eq!(icon_for_provider("future-provider"), Icon::Grid);
    }
}
