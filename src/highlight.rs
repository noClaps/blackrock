//! Syntax highlighting of code blocks with giallo, using Shiki's grammars and Xcode's default
//! color themes.
//!
//! Each highlighted token is wrapped in a `<span>` with an inline `color: light-dark(…)` style, so
//! the colors follow the page's `color-scheme`.

use std::fmt::Write;
use std::fs;
use std::sync::LazyLock;

use giallo::{Color, FontStyle, HighlightOptions, Registry, Style, ThemeVariant};
use pulldown_cmark_escape::escape_html_body_text;

/// Xcode's Default (Light) and Default (Dark) colors, as VS Code themes. Generated from Xcode's
/// `.xccolortheme` files, mapping grammar scopes to Xcode's syntax colors.
const XCODE_THEMES: [(&str, &str); 2] = [
    ("xcode-light", include_str!("themes/xcode-light.json")),
    ("xcode-dark", include_str!("themes/xcode-dark.json")),
];

const THEMES: ThemeVariant<&str> = ThemeVariant::Dual {
    light: "xcode-light",
    dark: "xcode-dark",
};

/// Shiki's grammars and our themes, loaded when first used.
static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let mut registry = Registry::builtin().expect("giallo's built-in registry is valid");
    // giallo can only load themes from files.
    for (name, json) in XCODE_THEMES {
        let path =
            std::env::temp_dir().join(format!("blackrock-{}-{name}.json", std::process::id()));
        fs::write(&path, json).expect("can write to the temporary directory");
        let added = registry.add_theme_from_path(&path);
        let _ = fs::remove_file(&path);
        added.expect("the Xcode themes are valid");
    }
    registry.link_grammars();
    registry
});

/// Highlights `code` as `lang`, a Shiki language id or alias, returning HTML for the contents of
/// a `<code>` element.
///
/// Returns `None` if the language is not supported.
pub fn highlight(lang: &str, code: &str) -> Option<String> {
    let highlighted = REGISTRY
        .highlight(
            code,
            &HighlightOptions::new(lang, THEMES).merge_whitespace(false),
        )
        .ok()?;
    let ThemeVariant::Dual { light, dark } = highlighted.theme else {
        unreachable!("highlighted with two themes")
    };
    let defaults = (light.default_style, dark.default_style);

    let mut html = String::new();
    // The style of the open `<span>`, if any. Adjacent tokens with the same style share a span.
    let mut open = String::new();
    for (i, line) in highlighted.tokens.iter().enumerate() {
        if i > 0 {
            html.push('\n');
        }
        for token in line {
            let ThemeVariant::Dual { light, dark } = token.style else {
                unreachable!("highlighted with two themes")
            };
            let style = token_style((light, dark), defaults);
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
            escape_html_body_text(&mut html, &token.text).ok()?;
        }
    }
    if !open.is_empty() {
        html.push_str("</span>");
    }
    Some(html)
}

/// The inline style for a token, or an empty string if it looks like plain text.
fn token_style((light, dark): (Style, Style), defaults: (Style, Style)) -> String {
    let mut style = String::new();
    if light.foreground != defaults.0.foreground || dark.foreground != defaults.1.foreground {
        let _ = write!(
            style,
            "color:light-dark({},{})",
            hex(light.foreground),
            hex(dark.foreground)
        );
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

fn hex(color: Color) -> String {
    color.as_hex().to_ascii_lowercase()
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
