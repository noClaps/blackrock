//! Syntax highlighting of code blocks with Irosashi, using Shiki's grammars and Xcode's default
//! color themes.
//!
//! Each highlighted token is wrapped in a `<span>` with an inline `color: light-dark(…)` style, so
//! the colors follow the page's `color-scheme`.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::sync::LazyLock;

use irosashi::{
    CodeToTokensOptions, DiagnosticKind, FontStyle, Highlighter, ThemedToken, TokensResult,
};
use pulldown_cmark_escape::escape_html_body_text;

/// Shiki's grammars, each loaded when first used, and Xcode's Default (Light) and Default (Dark)
/// colors as VS Code themes. The themes are generated from Xcode's `.xccolortheme` files, mapping
/// grammar scopes to Xcode's syntax colors.
static HIGHLIGHTER: LazyLock<Highlighter> = LazyLock::new(|| {
    Highlighter::builder()
        .theme(
            "xcode-light",
            include_bytes!("themes/xcode-light.json").to_vec(),
        )
        .and_then(|builder| {
            builder.theme(
                "xcode-dark",
                include_bytes!("themes/xcode-dark.json").to_vec(),
            )
        })
        .and_then(irosashi::HighlighterBuilder::build)
        .expect("the Xcode themes are valid")
});

/// Highlights `code` as `lang`, a Shiki language id or alias, returning HTML for the contents of
/// a `<code>` element.
///
/// Returns `None` if the language is not supported.
pub fn highlight(lang: &str, code: &str) -> Option<String> {
    // Themes are ordered by key, so the light theme is slot 0 and the dark theme slot 1.
    let themes = BTreeMap::from([
        ("0".to_owned(), "xcode-light".to_owned()),
        ("1".to_owned(), "xcode-dark".to_owned()),
    ]);
    let result = HIGHLIGHTER
        .code_to_tokens_multi(code, &themes, &CodeToTokensOptions::new(lang, ""))
        .ok()?;
    if (result.diagnostics.iter()).any(|d| matches!(d.kind, DiagnosticKind::UnknownLanguage)) {
        return None;
    }

    let mut html = String::new();
    // The style of the open `<span>`, if any. Adjacent tokens with the same style share a span.
    let mut open = String::new();
    // How much of the source has been written. Text between tokens, like line breaks, is copied
    // as-is.
    let mut written = 0;
    for line in &result.lines {
        for token in &line.tokens {
            let start = line.range.start + token.start;
            escape_html_body_text(&mut html, &result.source[written..start]).ok()?;
            let style = token_style(&result, token);
            if style != open {
                if !open.is_empty() {
                    html.push_str("</span>");
                }
                if !style.is_empty() {
                    // Writing to a `String` can't fail.
                    let _ = write!(html, "<span style=\"{style}\">");
                }
                open = style;
            }
            written = line.range.start + token.end;
            escape_html_body_text(&mut html, &result.source[start..written]).ok()?;
        }
    }
    if !open.is_empty() {
        html.push_str("</span>");
    }
    escape_html_body_text(&mut html, &result.source[written..]).ok()?;
    Some(html)
}

/// The inline style for a token, or an empty string if it looks like plain text.
fn token_style(result: &TokensResult, token: &ThemedToken) -> String {
    let (light, dark) = (result.style_in(token, 0), result.style_in(token, 1));
    // A token without its own color has the theme's default color.
    let color = |slot, color| match color {
        Some(id) => result.color_in(slot, id),
        None => result.fg_of(slot),
    };
    let (light_color, dark_color) = (color(0, light.color), color(1, dark.color));

    let mut style = String::new();
    if light_color != result.fg_of(0) || dark_color != result.fg_of(1) {
        let _ = write!(style, "color:light-dark({light_color},{dark_color})");
    }
    for (flag, css) in [
        (FontStyle::BOLD, "font-weight:bold"),
        (FontStyle::ITALIC, "font-style:italic"),
    ] {
        if light.font_style.contains(flag) {
            if !style.is_empty() {
                style.push(';');
            }
            style.push_str(css);
        }
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust() {
        let html = highlight("rs", "fn main() {}\n").unwrap();
        assert_eq!(
            html,
            "<span style=\"color:light-dark(#9b2393,#fc5fa3)\">fn</span> \
             <span style=\"color:light-dark(#0f68a0,#41a1c0)\">main</span>() {}\n"
        );
    }

    #[test]
    fn uses_shiki_ids_and_aliases() {
        for lang in [
            "shellscript",
            "bash",
            "ZSH",
            "c++",
            "mjs",
            "jsx",
            "mts",
            "yml",
            "toml",
        ] {
            assert!(highlight(lang, "x\n").is_some(), "{lang}");
        }
    }

    #[test]
    fn escapes_source() {
        let html = highlight("python", "x = '<a>'\n").unwrap();
        assert!(html.contains("&lt;a&gt;"), "{html}");
    }

    #[test]
    fn unknown_language() {
        assert_eq!(highlight("brainfuck", "+++"), None);
    }

    #[test]
    fn html_embeds_javascript() {
        let html = highlight("html", "<script>let x = 1;</script>\n").unwrap();
        // `let` is a JavaScript keyword.
        assert!(
            html.contains("<span style=\"color:light-dark(#9b2393,#fc5fa3)\">let</span>"),
            "{html}"
        );
    }
}
