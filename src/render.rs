//! Markdown to HTML rendering, with Obsidian's extensions to markdown.

use math_core::MathDisplay;
use pulldown_cmark::{
    CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd, TextMergeStream, html,
};
use pulldown_cmark_escape::{escape_href, escape_html, escape_html_body_text};

use crate::highlight;
use crate::math::MathRenderer;

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
    let events: Vec<Event> = TextMergeStream::new(Parser::new_ext(markdown, options())).collect();
    let mut renderer = Renderer {
        math: MathRenderer::new(),
    };
    let events = renderer.blocks(events);
    let events = renderer.inlines(events);
    let mut out = String::with_capacity(markdown.len() * 3 / 2);
    html::push_html(&mut out, events.into_iter());
    out
}

struct Renderer {
    math: MathRenderer,
}

impl Renderer {
    /// Rewrites code blocks, wikilinks, embeds and callouts.
    fn blocks<'a>(&mut self, mut events: Vec<Event<'a>>) -> Vec<Event<'a>> {
        let mut out = Vec::with_capacity(events.len());
        // Reversed so that we can pop events off the end and push back the ones we only peeked.
        events.reverse();
        // For each open blockquote, whether it is a callout and if so, whether it is foldable.
        let mut quotes: Vec<Option<Callout>> = Vec::new();
        // The callout whose title we are currently in.
        let mut title: Option<Callout> = None;

        while let Some(event) = events.pop() {
            match event {
                Event::Start(Tag::CodeBlock(kind)) => {
                    let mut code = String::new();
                    while let Some(event) = events.pop() {
                        match event {
                            Event::Text(text) => code.push_str(&text),
                            Event::End(TagEnd::CodeBlock) => break,
                            _ => {}
                        }
                    }
                    let lang = match &kind {
                        CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
                        CodeBlockKind::Indented => "",
                    };
                    out.push(Event::Html(code_block(lang, &code).into()));
                }
                Event::Start(Tag::Link {
                    link_type: link_type @ LinkType::WikiLink { .. },
                    dest_url,
                    title,
                    id,
                }) => out.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url: wikilink_href(&dest_url).into(),
                    title,
                    id,
                })),
                Event::Start(Tag::Image {
                    link_type: LinkType::WikiLink { has_pothole },
                    dest_url,
                    ..
                }) => {
                    let mut label = String::new();
                    while let Some(event) = events.pop() {
                        match event {
                            Event::Text(text) | Event::Code(text) => label.push_str(&text),
                            Event::End(TagEnd::Image) => break,
                            _ => {}
                        }
                    }
                    let label = has_pothole.then_some(label.as_str());
                    out.push(Event::InlineHtml(embed(&dest_url, label).into()));
                }
                Event::Start(Tag::BlockQuote(kind)) => {
                    let callout = match (events.last(), events.iter().rev().nth(1)) {
                        (Some(Event::Start(Tag::Paragraph)), Some(Event::Text(text))) => {
                            Callout::parse(text)
                        }
                        _ => None,
                    };
                    let Some((callout, rest)) = callout else {
                        quotes.push(None);
                        out.push(Event::Start(Tag::BlockQuote(kind)));
                        continue;
                    };
                    let rest = rest.to_owned();
                    events.pop(); // Start(Paragraph)
                    events.pop(); // Text
                    out.push(Event::Html(callout.open().into()));
                    if rest.is_empty() {
                        if matches!(
                            events.last(),
                            Some(
                                Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph)
                            )
                        ) {
                            out.push(Event::Text(callout.default_title().into()));
                        }
                    } else {
                        out.push(Event::Text(rest.into()));
                    }
                    quotes.push(Some(callout.clone()));
                    title = Some(callout);
                }
                Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph)
                    if title.is_some() =>
                {
                    let callout = title.take().unwrap();
                    out.push(Event::Html(callout.end_title().into()));
                    if !matches!(event, Event::End(TagEnd::Paragraph)) {
                        // The rest of the first paragraph is the start of the callout's content.
                        events.push(Event::Start(Tag::Paragraph));
                    }
                }
                Event::End(TagEnd::BlockQuote(kind)) => match quotes.pop().flatten() {
                    Some(callout) => out.push(Event::Html(callout.close().into())),
                    None => out.push(Event::End(TagEnd::BlockQuote(kind))),
                },
                event => out.push(event),
            }
        }
        out
    }

    /// Renders inline math.
    fn inlines<'a>(&mut self, events: Vec<Event<'a>>) -> Vec<Event<'a>> {
        events
            .into_iter()
            .map(|event| match event {
                Event::InlineMath(latex) => {
                    Event::InlineHtml(self.math.render(&latex, MathDisplay::Inline).into())
                }
                Event::DisplayMath(latex) => {
                    Event::InlineHtml(self.math.render(&latex, MathDisplay::Block).into())
                }
                event => event,
            })
            .collect()
    }
}

/// An Obsidian callout: a blockquote starting with `[!type]`.
#[derive(Clone)]
struct Callout {
    kind: String,
    /// `None` if the callout can't be folded, otherwise whether it starts out open.
    fold: Option<bool>,
}

impl Callout {
    /// Parses the first line of a callout, e.g. `[!tip]- Title`, returning the callout and the
    /// title (which may be empty).
    fn parse(text: &str) -> Option<(Callout, &str)> {
        let rest = text.strip_prefix("[!")?;
        let (kind, rest) = rest.split_once(']')?;
        if kind.is_empty() || kind.contains(char::is_whitespace) {
            return None;
        }
        let (fold, rest) = match rest.as_bytes().first() {
            Some(b'+') => (Some(true), &rest[1..]),
            Some(b'-') => (Some(false), &rest[1..]),
            _ => (None, rest),
        };
        let callout = Callout {
            kind: kind.to_lowercase(),
            fold,
        };
        Some((callout, rest.trim_start()))
    }

    fn default_title(&self) -> String {
        let mut chars = self.kind.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }

    fn open(&self) -> String {
        let mut html = String::from(match self.fold {
            Some(_) => "<details class=\"callout\" data-callout=\"",
            None => "<div class=\"callout\" data-callout=\"",
        });
        escape_html(&mut html, &self.kind).unwrap();
        html.push('"');
        match self.fold {
            Some(true) => html.push_str(" open>\n<summary class=\"callout-title\">"),
            Some(false) => html.push_str(">\n<summary class=\"callout-title\">"),
            None => html.push_str(">\n<div class=\"callout-title\">"),
        }
        html
    }

    fn end_title(&self) -> &'static str {
        match self.fold {
            Some(_) => "</summary>\n<div class=\"callout-content\">\n",
            None => "</div>\n<div class=\"callout-content\">\n",
        }
    }

    fn close(&self) -> &'static str {
        match self.fold {
            Some(_) => "</div>\n</details>\n",
            None => "</div>\n</div>\n",
        }
    }
}

fn code_block(lang: &str, code: &str) -> String {
    let mut html = String::from("<pre><code");
    if !lang.is_empty() {
        html.push_str(" class=\"language-");
        escape_html(&mut html, lang).unwrap();
        html.push('"');
    }
    html.push('>');
    match highlight::highlight(lang, code) {
        Some(highlighted) => html.push_str(&highlighted),
        None => escape_html_body_text(&mut html, code).unwrap(),
    }
    html.push_str("</code></pre>\n");
    html
}

/// Turns a wikilink target like `Note#Heading` or `#^block` into a URL.
fn wikilink_href(target: &str) -> String {
    let (page, fragment) = match target.split_once('#') {
        Some((page, fragment)) => (page, Some(fragment)),
        None => (target, None),
    };
    let mut href = page.trim().to_owned();
    if let Some(fragment) = fragment {
        href.push('#');
        // `Note#Heading#Subheading` links to the subheading.
        let fragment = fragment.rsplit('#').next().unwrap_or_default().trim();
        if fragment.starts_with('^') {
            href.push_str(fragment);
        } else {
            href.push_str(&slugify(fragment));
        }
    }
    href
}

/// Turns heading text into an `id`, e.g. `Hello, World!` into `hello-world`.
pub fn slugify(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if (c.is_whitespace() || c == '-' || c == '_') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "section".to_owned()
    } else {
        slug.to_owned()
    }
}

/// Renders an Obsidian embed, `![[target]]` or `![[target|label]]`.
///
/// Images, audio, video and PDFs are embedded. Other files, like notes, can't be transcluded
/// without the rest of the vault, so they become links.
fn embed(target: &str, label: Option<&str>) -> String {
    let path = target.split('#').next().unwrap_or_default().trim();
    let extension = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let mut html = String::new();
    let mut src = String::new();
    escape_href(&mut src, path).unwrap();
    // `escape_href` doesn't escape `&` and `'`, which matter in attributes.
    let src = src.replace('&', "&amp;").replace('\'', "&#39;");

    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "webp" | "avif" | "ico" | "tif"
        | "tiff" => {
            html.push_str("<img src=\"");
            html.push_str(&src);
            html.push_str("\" alt=\"");
            // The label is either a size like `300` or `300x200`, or alt text.
            match label.and_then(parse_size) {
                Some((width, height)) => {
                    escape_html(&mut html, file_name(path)).unwrap();
                    html.push_str(&format!("\" width=\"{width}"));
                    if let Some(height) = height {
                        html.push_str(&format!("\" height=\"{height}"));
                    }
                }
                None => escape_html(&mut html, label.unwrap_or(file_name(path))).unwrap(),
            }
            html.push_str("\">");
        }
        "mp3" | "wav" | "m4a" | "ogg" | "flac" | "3gp" | "oga" | "opus" => {
            html.push_str("<audio controls src=\"");
            html.push_str(&src);
            html.push_str("\"></audio>");
        }
        "mp4" | "webm" | "ogv" | "mov" | "mkv" => {
            html.push_str("<video controls src=\"");
            html.push_str(&src);
            html.push_str("\"></video>");
        }
        "pdf" => {
            html.push_str("<iframe src=\"");
            html.push_str(&src);
            html.push_str("\" title=\"");
            escape_html(&mut html, label.unwrap_or(file_name(path))).unwrap();
            html.push_str("\"></iframe>");
        }
        _ => {
            let mut href = String::new();
            escape_href(&mut href, &wikilink_href(target)).unwrap();
            html.push_str("<a class=\"embed\" href=\"");
            escape_html(&mut html, &href).unwrap();
            html.push_str("\">");
            escape_html_body_text(&mut html, label.unwrap_or(target)).unwrap();
            html.push_str("</a>");
        }
    }
    html
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parses an embed size like `300` or `300x200`.
fn parse_size(label: &str) -> Option<(u32, Option<u32>)> {
    match label.trim().split_once('x') {
        Some((width, height)) => Some((width.parse().ok()?, Some(height.parse().ok()?))),
        None => Some((label.trim().parse().ok()?, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_code_blocks() {
        let html = to_html("```rust\nlet x = 1;\n```\n");
        assert!(
            html.starts_with("<pre><code class=\"language-rust\"><span style="),
            "{html}"
        );
        assert!(html.ends_with("</code></pre>\n"), "{html}");
    }

    #[test]
    fn escapes_unknown_code_blocks() {
        assert_eq!(
            to_html("```foo\n<a>\n```\n"),
            "<pre><code class=\"language-foo\">&lt;a&gt;\n</code></pre>\n"
        );
        assert_eq!(
            to_html("    <a>\n"),
            "<pre><code>&lt;a&gt;\n</code></pre>\n"
        );
    }

    #[test]
    fn renders_math() {
        assert_eq!(to_html("$x$\n"), "<p><math><mi>x</mi></math></p>\n");
        let html = to_html("$$\nx\n$$\n");
        assert!(html.contains("<math display=\"block\">"), "{html}");
    }

    #[test]
    fn wikilinks() {
        assert_eq!(
            to_html("[[My Note]]"),
            "<p><a href=\"My%20Note\">My Note</a></p>\n"
        );
        assert_eq!(
            to_html("[[Note#Some Heading|alias]]"),
            "<p><a href=\"Note#some-heading\">alias</a></p>\n"
        );
        assert_eq!(
            to_html("[[#^abc|block]]"),
            "<p><a href=\"#^abc\">block</a></p>\n"
        );
    }

    #[test]
    fn embeds() {
        assert_eq!(
            to_html("![[cat.png]]"),
            "<p><img src=\"cat.png\" alt=\"cat.png\"></p>\n"
        );
        assert_eq!(
            to_html("![[a b.png|300x200]]"),
            "<p><img src=\"a%20b.png\" alt=\"a b.png\" width=\"300\" height=\"200\"></p>\n"
        );
        assert_eq!(
            to_html("![[cat.png|A cat]]"),
            "<p><img src=\"cat.png\" alt=\"A cat\"></p>\n"
        );
        assert_eq!(
            to_html("![[song.mp3]]"),
            "<p><audio controls src=\"song.mp3\"></audio></p>\n"
        );
        assert_eq!(
            to_html("![[Other Note]]"),
            "<p><a class=\"embed\" href=\"Other%20Note\">Other Note</a></p>\n"
        );
    }

    #[test]
    fn callouts() {
        assert_eq!(
            to_html("> [!NOTE]\n> Body"),
            "<div class=\"callout\" data-callout=\"note\">\n\
             <div class=\"callout-title\">Note</div>\n\
             <div class=\"callout-content\">\n<p>Body</p>\n</div>\n</div>\n"
        );
        assert_eq!(
            to_html("> [!tip]- Custom *title*\n>\n> Body"),
            "<details class=\"callout\" data-callout=\"tip\">\n\
             <summary class=\"callout-title\">Custom <em>title</em></summary>\n\
             <div class=\"callout-content\">\n<p>Body</p>\n</div>\n</details>\n"
        );
        assert_eq!(
            to_html("> [!faq]+ Open"),
            "<details class=\"callout\" data-callout=\"faq\" open>\n\
             <summary class=\"callout-title\">Open</summary>\n\
             <div class=\"callout-content\">\n</div>\n</details>\n"
        );
    }

    #[test]
    fn nested_callouts_and_plain_quotes() {
        let html = to_html("> [!note]\n> > [!warning] Inner\n> > Text\n\n> plain");
        assert_eq!(
            html,
            "<div class=\"callout\" data-callout=\"note\">\n\
             <div class=\"callout-title\">Note</div>\n\
             <div class=\"callout-content\">\n\
             <div class=\"callout\" data-callout=\"warning\">\n\
             <div class=\"callout-title\">Inner</div>\n\
             <div class=\"callout-content\">\n<p>Text</p>\n</div>\n</div>\n\
             </div>\n</div>\n\
             <blockquote>\n<p>plain</p>\n</blockquote>\n"
        );
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Ünïcode  — dash "), "ünïcode-dash");
        assert_eq!(slugify("!!!"), "section");
    }
}
