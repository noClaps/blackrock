//! Markdown to HTML rendering.

use pulldown_cmark::{Options, Parser, TextMergeStream, html};

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_WIKILINKS
}

/// Renders an Obsidian-flavored markdown body (without frontmatter) to an HTML fragment.
pub fn to_html(markdown: &str) -> String {
    let events = TextMergeStream::new(Parser::new_ext(markdown, options()));
    let mut out = String::with_capacity(markdown.len() * 3 / 2);
    html::push_html(&mut out, events);
    out
}
