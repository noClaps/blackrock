//! Syntax highlighting of code blocks with Tree-sitter, using GitHub's color themes.
//!
//! Each highlighted token is wrapped in a `<span>` with an inline `color: light-dark(…)` style, so
//! the colors follow the page's `color-scheme`.

use std::collections::HashMap;
use std::sync::LazyLock;

use tree_sitter_highlight::{HighlightConfiguration, Highlighter, HtmlRenderer};
use {
    tree_sitter_bash as bash, tree_sitter_c as c, tree_sitter_cpp as cpp, tree_sitter_css as css,
    tree_sitter_go as go, tree_sitter_html as html, tree_sitter_java as java,
    tree_sitter_javascript as js, tree_sitter_json as json, tree_sitter_python as python,
    tree_sitter_ruby as ruby, tree_sitter_rust as rust, tree_sitter_toml_ng as toml,
    tree_sitter_typescript as ts, tree_sitter_yaml as yaml,
};

macro_rules! color {
    ($light:literal, $dark:literal) => {
        concat!("color:light-dark(", $light, ",", $dark, ")")
    };
}

// GitHub's "prettylights" syntax colors, from Primer's `light` and `dark` themes.
const COMMENT: &str = color!("#59636e", "#9198a1");
const CONSTANT: &str = color!("#0550ae", "#79c0ff");
const ENTITY: &str = color!("#6639ba", "#d2a8ff");
const ENTITY_TAG: &str = color!("#0550ae", "#7ee787");
const KEYWORD: &str = color!("#cf222e", "#ff7b72");
const STRING: &str = color!("#0a3069", "#a5d6ff");
const STRING_REGEXP: &str = color!("#116329", "#7ee787");
const VARIABLE: &str = color!("#953800", "#ffa657");

/// The Tree-sitter captures we recognize, and the style of each.
///
/// Tree-sitter picks the most specific recognized name for each capture, so e.g.
/// `@function.method.builtin` falls back to `function`. Captures that match nothing here (like
/// `@variable` or `@punctuation.bracket`) are left uncolored, as on GitHub.
const CAPTURES: &[(&str, &str)] = &[
    ("attribute", CONSTANT),
    ("boolean", CONSTANT),
    ("comment", COMMENT),
    ("constant", CONSTANT),
    ("constructor", ENTITY),
    ("escape", STRING_REGEXP),
    ("function", ENTITY),
    ("keyword", KEYWORD),
    ("label", CONSTANT),
    ("module", VARIABLE),
    ("namespace", VARIABLE),
    ("number", CONSTANT),
    ("operator", KEYWORD),
    ("property", CONSTANT),
    ("punctuation.special", KEYWORD),
    ("string", STRING),
    ("string.escape", STRING_REGEXP),
    ("string.regexp", STRING_REGEXP),
    ("string.special.key", CONSTANT),
    ("string.special.regex", STRING_REGEXP),
    ("string.special.symbol", CONSTANT),
    ("tag", ENTITY_TAG),
    ("type", VARIABLE),
    ("type.builtin", KEYWORD),
    ("variable.builtin", CONSTANT),
];

/// All supported languages, keyed by their Shiki language id.
static LANGUAGES: LazyLock<HashMap<&'static str, HighlightConfiguration>> = LazyLock::new(|| {
    // TypeScript's and C++'s queries extend JavaScript's and C's.
    let ts_highlights = [ts::HIGHLIGHTS_QUERY, js::HIGHLIGHT_QUERY].concat();
    let ts_locals = [ts::LOCALS_QUERY, js::LOCALS_QUERY].concat();
    let languages = [
        ("shellscript", bash::LANGUAGE, bash::HIGHLIGHT_QUERY, "", ""),
        ("c", c::LANGUAGE, c::HIGHLIGHT_QUERY, "", ""),
        (
            "cpp",
            cpp::LANGUAGE,
            &[cpp::HIGHLIGHT_QUERY, c::HIGHLIGHT_QUERY].concat(),
            "",
            "",
        ),
        ("css", css::LANGUAGE, css::HIGHLIGHTS_QUERY, "", ""),
        ("go", go::LANGUAGE, go::HIGHLIGHTS_QUERY, "", ""),
        (
            "html",
            html::LANGUAGE,
            html::HIGHLIGHTS_QUERY,
            html::INJECTIONS_QUERY,
            "",
        ),
        ("java", java::LANGUAGE, java::HIGHLIGHTS_QUERY, "", ""),
        (
            "javascript",
            js::LANGUAGE,
            &[js::HIGHLIGHT_QUERY, js::JSX_HIGHLIGHT_QUERY].concat(),
            js::INJECTIONS_QUERY,
            js::LOCALS_QUERY,
        ),
        ("json", json::LANGUAGE, json::HIGHLIGHTS_QUERY, "", ""),
        ("python", python::LANGUAGE, python::HIGHLIGHTS_QUERY, "", ""),
        (
            "ruby",
            ruby::LANGUAGE,
            ruby::HIGHLIGHTS_QUERY,
            "",
            ruby::LOCALS_QUERY,
        ),
        (
            "rust",
            rust::LANGUAGE,
            rust::HIGHLIGHTS_QUERY,
            rust::INJECTIONS_QUERY,
            "",
        ),
        ("toml", toml::LANGUAGE, toml::HIGHLIGHTS_QUERY, "", ""),
        (
            "tsx",
            ts::LANGUAGE_TSX,
            &[&ts_highlights, js::JSX_HIGHLIGHT_QUERY].concat(),
            js::INJECTIONS_QUERY,
            &ts_locals,
        ),
        (
            "typescript",
            ts::LANGUAGE_TYPESCRIPT,
            &ts_highlights,
            js::INJECTIONS_QUERY,
            &ts_locals,
        ),
        ("yaml", yaml::LANGUAGE, yaml::HIGHLIGHTS_QUERY, "", ""),
    ];

    let names: Vec<&str> = CAPTURES.iter().map(|(name, _)| *name).collect();
    languages
        .into_iter()
        .map(|(name, language, highlights, injections, locals)| {
            let mut config =
                HighlightConfiguration::new(language.into(), name, highlights, injections, locals)
                    .unwrap_or_else(|e| panic!("invalid {name} highlight query: {e}"));
            config.configure(&names);
            (name, config)
        })
        .collect()
});

/// Maps a code block's info string (or an injected language name) to a supported language's
/// Shiki id, accepting the same ids and aliases as Shiki.
fn shiki_id(lang: &str) -> Option<&'static str> {
    Some(match lang.to_ascii_lowercase().as_str() {
        "shellscript" | "bash" | "sh" | "shell" | "zsh" => "shellscript",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        "css" => "css",
        "go" => "go",
        "html" => "html",
        "java" => "java",
        // The JavaScript queries include JSX, so Shiki's separate `jsx` language uses them too.
        "javascript" | "js" | "cjs" | "mjs" | "jsx" => "javascript",
        "json" => "json",
        "python" | "py" => "python",
        "ruby" | "rb" => "ruby",
        "rust" | "rs" => "rust",
        "toml" => "toml",
        "tsx" => "tsx",
        "typescript" | "ts" | "cts" | "mts" => "typescript",
        "yaml" | "yml" => "yaml",
        _ => return None,
    })
}

/// Highlights `code` as `lang`, returning HTML for the contents of a `<code>` element.
///
/// Returns `None` if the language is not supported or the code could not be highlighted.
pub fn highlight(lang: &str, code: &str) -> Option<String> {
    let config = LANGUAGES.get(shiki_id(lang)?)?;
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, code.as_bytes(), None, None, |injected| {
            LANGUAGES.get(shiki_id(injected)?)
        })
        .ok()?;

    let mut renderer = HtmlRenderer::new();
    renderer
        .render(events, code.as_bytes(), &|h, html| {
            html.extend(b"style=\"");
            html.extend(CAPTURES[h.0].1.as_bytes());
            html.extend(b"\"");
        })
        .ok()?;
    String::from_utf8(renderer.html).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_languages_load() {
        assert_eq!(LANGUAGES.len(), 16);
    }

    #[test]
    fn uses_shiki_ids_and_aliases() {
        for id in LANGUAGES.keys() {
            assert_eq!(shiki_id(id), Some(*id));
        }
        for (alias, id) in [
            ("bash", "shellscript"),
            ("ZSH", "shellscript"),
            ("c++", "cpp"),
            ("mjs", "javascript"),
            ("jsx", "javascript"),
            ("mts", "typescript"),
            ("yml", "yaml"),
        ] {
            assert_eq!(shiki_id(alias), Some(id), "{alias}");
        }
    }

    #[test]
    fn highlights_rust() {
        let html = highlight("rs", "fn main() {}\n").unwrap();
        assert_eq!(
            html,
            "<span style=\"color:light-dark(#cf222e,#ff7b72)\">fn</span> \
             <span style=\"color:light-dark(#6639ba,#d2a8ff)\">main</span>() {}\n"
        );
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
    fn html_injects_javascript() {
        let html = highlight("html", "<script>let x = 1;</script>\n").unwrap();
        // `let` is a JavaScript keyword.
        assert!(
            html.contains("<span style=\"color:light-dark(#cf222e,#ff7b72)\">let</span>"),
            "{html}"
        );
    }
}
