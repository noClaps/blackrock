//! Syntax highlighting of code blocks with Tree-sitter, using GitHub's color themes.
//!
//! Each highlighted token is wrapped in a `<span>` with an inline `color: light-dark(…)` style, so
//! the colors follow the page's `color-scheme`.

use std::collections::HashMap;
use std::sync::LazyLock;

use pulldown_cmark_escape::escape_html_body_text;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// GitHub's "prettylights" syntax colors (light, dark), from Primer's `light` and `dark` themes.
mod github {
    pub const COMMENT: (&str, &str) = ("#59636e", "#9198a1");
    pub const CONSTANT: (&str, &str) = ("#0550ae", "#79c0ff");
    pub const ENTITY: (&str, &str) = ("#6639ba", "#d2a8ff");
    pub const ENTITY_TAG: (&str, &str) = ("#0550ae", "#7ee787");
    pub const KEYWORD: (&str, &str) = ("#cf222e", "#ff7b72");
    pub const STRING: (&str, &str) = ("#0a3069", "#a5d6ff");
    pub const STRING_REGEXP: (&str, &str) = ("#116329", "#7ee787");
    pub const VARIABLE: (&str, &str) = ("#953800", "#ffa657");
}

/// The Tree-sitter captures we recognize, and the GitHub color each maps to.
///
/// Tree-sitter picks the most specific recognized name for each capture, so e.g.
/// `@function.method.builtin` falls back to `function`. Captures that match nothing here (like
/// `@variable` or `@punctuation.bracket`) are left uncolored, as on GitHub.
const CAPTURES: &[(&str, (&str, &str))] = &[
    ("attribute", github::CONSTANT),
    ("boolean", github::CONSTANT),
    ("comment", github::COMMENT),
    ("constant", github::CONSTANT),
    ("constant.builtin", github::CONSTANT),
    ("constructor", github::ENTITY),
    ("escape", github::STRING_REGEXP),
    ("function", github::ENTITY),
    ("function.builtin", github::ENTITY),
    ("function.macro", github::ENTITY),
    ("function.method", github::ENTITY),
    ("keyword", github::KEYWORD),
    ("label", github::CONSTANT),
    ("module", github::VARIABLE),
    ("namespace", github::VARIABLE),
    ("number", github::CONSTANT),
    ("operator", github::KEYWORD),
    ("property", github::CONSTANT),
    ("punctuation.special", github::KEYWORD),
    ("string", github::STRING),
    ("string.escape", github::STRING_REGEXP),
    ("string.regexp", github::STRING_REGEXP),
    ("string.special", github::STRING),
    ("string.special.key", github::CONSTANT),
    ("string.special.regex", github::STRING_REGEXP),
    ("string.special.symbol", github::CONSTANT),
    ("tag", github::ENTITY_TAG),
    ("type", github::VARIABLE),
    ("type.builtin", github::KEYWORD),
    ("variable.builtin", github::CONSTANT),
];

/// The `style` attribute value for each entry of [`CAPTURES`], in the same order.
static STYLES: LazyLock<Vec<String>> = LazyLock::new(|| {
    CAPTURES
        .iter()
        .map(|(_, (light, dark))| format!("color:light-dark({light},{dark})"))
        .collect()
});

/// All supported languages, keyed by their canonical name.
static LANGUAGES: LazyLock<HashMap<&'static str, HighlightConfiguration>> = LazyLock::new(|| {
    let javascript_highlights = [
        tree_sitter_javascript::HIGHLIGHT_QUERY,
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
    ]
    .concat();
    let typescript_highlights = [
        tree_sitter_typescript::HIGHLIGHTS_QUERY,
        tree_sitter_javascript::HIGHLIGHT_QUERY,
    ]
    .concat();
    let tsx_highlights = [
        typescript_highlights.as_str(),
        tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
    ]
    .concat();
    let cpp_highlights = [
        tree_sitter_cpp::HIGHLIGHT_QUERY,
        tree_sitter_c::HIGHLIGHT_QUERY,
    ]
    .concat();
    let javascript_locals = tree_sitter_javascript::LOCALS_QUERY;
    let typescript_locals = [
        tree_sitter_typescript::LOCALS_QUERY,
        tree_sitter_javascript::LOCALS_QUERY,
    ]
    .concat();

    let definitions = [
        (
            "bash",
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        (
            "c",
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        (
            "cpp",
            tree_sitter_cpp::LANGUAGE.into(),
            &cpp_highlights,
            "",
            "",
        ),
        (
            "css",
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        (
            "go",
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        (
            "html",
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        ),
        (
            "java",
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        (
            "javascript",
            tree_sitter_javascript::LANGUAGE.into(),
            &javascript_highlights,
            tree_sitter_javascript::INJECTIONS_QUERY,
            javascript_locals,
        ),
        (
            "json",
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        (
            "python",
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        (
            "ruby",
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_ruby::LOCALS_QUERY,
        ),
        (
            "rust",
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        ),
        (
            "toml",
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        (
            "tsx",
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            &tsx_highlights,
            tree_sitter_javascript::INJECTIONS_QUERY,
            &typescript_locals,
        ),
        (
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            &typescript_highlights,
            tree_sitter_javascript::INJECTIONS_QUERY,
            &typescript_locals,
        ),
        (
            "yaml",
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
    ];

    let names: Vec<&str> = CAPTURES.iter().map(|(name, _)| *name).collect();
    definitions
        .into_iter()
        .map(|(name, language, highlights, injections, locals)| {
            let mut config =
                HighlightConfiguration::new(language, name, highlights, injections, locals)
                    .unwrap_or_else(|e| panic!("invalid {name} highlight query: {e}"));
            config.configure(&names);
            (name, config)
        })
        .collect()
});

/// Maps a code block's info string (or an injected language name) to a supported language.
fn canonical_name(lang: &str) -> Option<&'static str> {
    Some(match lang.to_ascii_lowercase().as_str() {
        "bash" | "sh" | "shell" | "zsh" | "console" => "bash",
        "c" | "h" => "c",
        "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "css" => "css",
        "go" | "golang" => "go",
        "html" | "htm" | "xhtml" => "html",
        "java" => "java",
        "javascript" | "js" | "jsx" | "mjs" | "cjs" | "node" => "javascript",
        "json" | "jsonc" | "json5" => "json",
        "python" | "py" | "py3" | "python3" => "python",
        "ruby" | "rb" => "ruby",
        "rust" | "rs" => "rust",
        "toml" => "toml",
        "tsx" => "tsx",
        "typescript" | "ts" | "mts" | "cts" => "typescript",
        "yaml" | "yml" => "yaml",
        _ => return None,
    })
}

/// Highlights `code` as `lang`, returning HTML for the contents of a `<code>` element.
///
/// Returns `None` if the language is not supported or the code could not be highlighted.
pub fn highlight(lang: &str, code: &str) -> Option<String> {
    let config = LANGUAGES.get(canonical_name(lang)?)?;
    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, code.as_bytes(), None, None, |injected| {
            LANGUAGES.get(canonical_name(injected)?)
        })
        .ok()?;

    let mut html = String::with_capacity(code.len() * 2);
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(h) => {
                html.push_str("<span style=\"");
                html.push_str(&STYLES[h.0]);
                html.push_str("\">");
            }
            HighlightEvent::HighlightEnd => html.push_str("</span>"),
            HighlightEvent::Source { start, end } => {
                escape_html_body_text(&mut html, &code[start..end]).ok()?;
            }
        }
    }
    Some(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_languages_load() {
        assert_eq!(LANGUAGES.len(), 16);
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
