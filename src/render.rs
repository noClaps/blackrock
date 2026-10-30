//! Markdown to HTML rendering, with Obsidian's extensions to markdown.

use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{
    CodeBlockKind, Event, LinkType, Options, Parser, Tag, TagEnd, TextMergeStream, html,
};
use pulldown_cmark_escape::{escape_href, escape_html, escape_html_body_text};

use crate::highlight;
use crate::vault::{self, Vault};

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH
        | Options::ENABLE_WIKILINKS
}

/// Renders an Obsidian-flavored markdown body (without frontmatter) to an HTML fragment.
///
/// Local images are looked up in `vault`, starting from the note's directory `note_dir`
/// (relative to the vault root). Returns all errors if any image or math can't be rendered.
pub fn to_html(markdown: &str, vault: &Vault, note_dir: &Path) -> Result<String, Vec<String>> {
    let markdown = strip_comments(markdown);
    let events: Vec<Event> = TextMergeStream::new(Parser::new_ext(&markdown, options())).collect();
    let config = MathCoreConfig {
        // Like MathJax in Obsidian, `\newcommand`s stay defined for the rest of the note.
        global_group: true,
        ..Default::default()
    };
    let mut renderer = Renderer {
        math: LatexToMathML::new(config, Vec::new()).expect("no custom macros"),
        ids: HashMap::new(),
        vault,
        note_dir,
        errors: Vec::new(),
    };
    let events = renderer.blocks(events);
    let events = hoist_figures(events);
    let events = renderer.inlines(events);
    if !renderer.errors.is_empty() {
        return Err(renderer.errors);
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    Ok(out)
}

struct Renderer<'v> {
    math: LatexToMathML,
    /// How often each heading id has been used.
    ids: HashMap<String, usize>,
    vault: &'v Vault,
    note_dir: &'v Path,
    errors: Vec<String>,
}

impl Renderer<'_> {
    /// Rewrites code blocks, wikilinks, embeds and callouts.
    fn blocks<'a>(&mut self, mut events: Vec<Event<'a>>) -> Vec<Event<'a>> {
        let mut out = Vec::new();
        // Reversed so that we can pop events off the end and push back the ones we only peeked.
        events.reverse();
        // For each open blockquote, whether it is a callout and if so, whether it is foldable.
        let mut quotes: Vec<Option<Callout>> = Vec::new();
        // The callout whose title we are currently in.
        let mut title: Option<Callout> = None;
        // How many links we are in. Images in links can't be figures.
        let mut link_depth = 0;

        while let Some(mut event) = events.pop() {
            match &mut event {
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    ..
                }) => {
                    link_depth += 1;
                    if let LinkType::WikiLink { .. } = link_type {
                        *dest_url = wikilink_href(dest_url).into();
                    }
                }
                Event::End(TagEnd::Link) => link_depth -= 1,
                _ => {}
            }
            match event {
                Event::Start(Tag::CodeBlock(kind)) => {
                    out.push(Event::Html(code_block(&kind, &mut events).into()));
                }
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    ..
                }) => {
                    let label = image_label(&mut events);
                    let image = if let LinkType::WikiLink { has_pothole } = link_type {
                        let label = has_pothole.then_some(label.as_str());
                        if is_image(&dest_url) {
                            self.image(&dest_url, label, true, "", link_depth > 0)
                        } else {
                            Event::InlineHtml(media_embed(&dest_url, label).into())
                        }
                    } else {
                        let link = percent_decode_str(&dest_url).decode_utf8_lossy();
                        self.image(&link, Some(&label), false, &title, link_depth > 0)
                    };
                    out.push(image);
                }
                Event::Start(Tag::BlockQuote(kind)) => {
                    let callout = start_callout(&mut events, &mut out);
                    if callout.is_none() {
                        out.push(Event::Start(Tag::BlockQuote(kind)));
                    }
                    quotes.push(callout.clone());
                    title = callout;
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

    /// Processes each run of inline content, see [`Self::run`].
    fn inlines<'a>(&mut self, events: Vec<Event<'a>>) -> Vec<Event<'a>> {
        let mut out = Vec::new();
        let mut run = Vec::new();
        for event in events {
            if is_inline(&event) {
                run.push(event);
            } else if run.is_empty() {
                out.push(event);
            } else {
                let end = self.run(&mut out, std::mem::take(&mut run), event);
                out.push(end);
            }
        }
        // Inline content is always inside a block, so `run` is empty here.
        out
    }

    /// Renders math, `==highlights==` and `#tags` in a run of inline events, strips a trailing
    /// `^block-id` and gives headings ids.
    ///
    /// The block containing the run has already been pushed to `out`, and `end` is the event after
    /// the run. Returns the (possibly rewritten) `end` event.
    fn run<'a>(
        &mut self,
        out: &mut Vec<Event<'a>>,
        mut run: Vec<Event<'a>>,
        mut end: Event<'a>,
    ) -> Event<'a> {
        match out.last_mut() {
            Some(Event::Start(Tag::Heading { id: id @ None, .. })) => {
                *id = Some(self.unique_id(slugify(&plain_text(&run))).into());
            }
            // A `$$…$$` block on its own is rendered without a surrounding paragraph.
            Some(start @ Event::Start(Tag::Paragraph))
                if is_display_math_only(&run) && end == Event::End(TagEnd::Paragraph) =>
            {
                *start = Event::Html("".into());
                end = Event::Html("\n".into());
            }
            Some(start @ Event::Start(Tag::Paragraph | Tag::Item)) => {
                if let Some(block_id) = strip_block_id(&mut run) {
                    let tag = if *start == Event::Start(Tag::Paragraph) {
                        "p"
                    } else {
                        "li"
                    };
                    *start = Event::Html(format!("<{tag} id=\"{}\">", attr(&block_id)).into());
                }
            }
            _ => {}
        }

        let highlights = find_highlights(&run);
        let mut highlights = highlights.iter().peekable();
        let mut processed = Vec::new();
        let mut link_depth = 0;
        let mut at_boundary = true;
        for (index, event) in run.into_iter().enumerate() {
            let next_at_boundary = matches!(event, Event::SoftBreak | Event::HardBreak);
            match event {
                Event::InlineMath(latex) => processed.push(self.math(&latex, MathDisplay::Inline)),
                Event::DisplayMath(latex) => processed.push(self.math(&latex, MathDisplay::Block)),
                Event::Text(text) => {
                    let mut marks = Vec::new();
                    while let Some(&(_, offset, open)) =
                        highlights.next_if(|(event, _, _)| *event == index)
                    {
                        marks.push((offset, open));
                    }
                    split_text(&text, &marks, link_depth == 0, at_boundary, &mut processed);
                }
                event => {
                    match event {
                        Event::Start(Tag::Link { .. }) => link_depth += 1,
                        Event::End(TagEnd::Link) => link_depth -= 1,
                        _ => {}
                    }
                    processed.push(event);
                }
            }
            at_boundary = next_at_boundary;
        }
        out.extend(processed);
        end
    }

    /// Renders an image. Local images are embedded as `data:` URLs.
    ///
    /// The label holds the alt text and optionally a size, as in `alt|300` or `alt|300x200`. For
    /// wikilinks, the label can also be just a size. An image with alt text becomes a figure with
    /// the alt text as its caption, unless it is inside a link.
    fn image<'a>(
        &mut self,
        link: &str,
        label: Option<&str>,
        is_wikilink: bool,
        title: &str,
        in_link: bool,
    ) -> Event<'a> {
        let label = label.unwrap_or_default();
        let (alt, size) = if let Some((alt, Some(size))) = label
            .rsplit_once('|')
            .map(|(alt, size)| (alt, size_attrs(size)))
        {
            (alt, size)
        } else if let Some(size) = size_attrs(label).filter(|_| is_wikilink) {
            ("", size)
        } else {
            (label, String::new())
        };
        let alt = alt.trim();

        let src = if is_remote(link) {
            href(link)
        } else {
            match self.vault.image_data_url(link, self.note_dir) {
                Ok(data_url) => data_url,
                Err(err) => return self.error(err),
            }
        };
        let title = if title.is_empty() {
            String::new()
        } else {
            format!(" title=\"{}\"", attr(title))
        };
        let html = format!("<img src=\"{src}\" alt=\"{}\"{title}{size}>", attr(alt));

        if alt.is_empty() || in_link {
            Event::InlineHtml(html.into())
        } else {
            // A block-level event, which `hoist_figures` moves out of paragraphs.
            let caption = text(alt);
            Event::Html(
                format!("<figure>\n{html}\n<figcaption>{caption}</figcaption>\n</figure>\n").into(),
            )
        }
    }

    /// Converts LaTeX to a MathML Core `<math>` element.
    fn math<'a>(&mut self, latex: &str, display: MathDisplay) -> Event<'a> {
        match self.math.convert_with_global_state(latex, display) {
            Ok(result) => Event::InlineHtml(result.mathml.into()),
            Err(err) => self.error(format!("cannot render math `{}`: {err}", latex.trim())),
        }
    }

    /// Records an error, returning an empty event to use in place of what couldn't be rendered.
    fn error<'a>(&mut self, err: String) -> Event<'a> {
        self.errors.push(err);
        Event::InlineHtml("".into())
    }

    /// Makes `id` unique within the document by appending a number if needed.
    fn unique_id(&mut self, id: String) -> String {
        let count = self.ids.entry(id.clone()).or_insert(0);
        *count += 1;
        match *count {
            1 => id,
            n => format!("{id}-{}", n - 1),
        }
    }
}

fn is_inline(event: &Event) -> bool {
    match event {
        Event::Text(_)
        | Event::Code(_)
        | Event::InlineMath(_)
        | Event::DisplayMath(_)
        | Event::InlineHtml(_)
        | Event::SoftBreak
        | Event::HardBreak
        | Event::FootnoteReference(_)
        | Event::TaskListMarker(_) => true,
        Event::Start(tag) => matches!(
            tag,
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
        ),
        Event::End(tag) => matches!(
            tag,
            TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image
        ),
        _ => false,
    }
}

fn plain_text(run: &[Event]) -> String {
    let mut text = String::new();
    for event in run {
        match event {
            Event::Text(s) | Event::Code(s) | Event::InlineMath(s) => text.push_str(s),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text
}

fn is_display_math_only(run: &[Event]) -> bool {
    run.iter()
        .any(|event| matches!(event, Event::DisplayMath(_)))
        && run.iter().all(|event| match event {
            Event::DisplayMath(_) | Event::SoftBreak | Event::HardBreak => true,
            Event::Text(text) => text.trim().is_empty(),
            _ => false,
        })
}

/// Removes a trailing ` ^block-id` from a paragraph or list item, returning `^block-id`.
fn strip_block_id(run: &mut Vec<Event>) -> Option<String> {
    let Some(Event::Text(text)) = run.last() else {
        return None;
    };
    let trimmed = text.trim_end();
    let caret = trimmed.rfind('^')?;
    let id = &trimmed[caret + 1..];
    let before = &trimmed[..caret];
    let valid_id = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    // The id must be separated from the content before it, and there must be content.
    let separated = before.ends_with(char::is_whitespace)
        || (before.is_empty()
            && run.len() > 1
            && matches!(run[run.len() - 2], Event::SoftBreak | Event::HardBreak));
    if !valid_id || !separated {
        return None;
    }
    let block_id = format!("^{id}");
    let kept = before.trim_end().to_owned();
    run.pop();
    if kept.is_empty() {
        if matches!(run.last(), Some(Event::SoftBreak | Event::HardBreak)) {
            run.pop();
        }
    } else {
        run.push(Event::Text(kept.into()));
    }
    Some(block_id)
}

/// Finds matching `==` pairs in a run, returning `(event index, byte offset, is opening)`.
///
/// Like emphasis, an opening `==` must be followed by a non-space and a closing `==` preceded by
/// a non-space. Both must be at the same level of inline nesting, so the `<mark>` elements nest
/// properly with other inline elements.
fn find_highlights(run: &[Event]) -> Vec<(usize, usize, bool)> {
    let mut marks = Vec::new();
    // The opening mark (event index, offset, nesting depth) waiting to be closed.
    let mut open: Option<(usize, usize, usize)> = None;
    let mut depth = 0;
    for (index, event) in run.iter().enumerate() {
        let text = match event {
            Event::Start(_) => {
                depth += 1;
                continue;
            }
            Event::End(_) => {
                depth -= 1;
                if open.is_some_and(|(_, _, open_depth)| open_depth > depth) {
                    open = None;
                }
                continue;
            }
            Event::Text(text) => text,
            _ => continue,
        };
        let mut i = 0;
        while let Some(found) = text[i..].find("==") {
            let at = i + found;
            let before = text[..at].chars().next_back();
            let after = text[at + 2..].chars().next();
            match open {
                None => {
                    let opens =
                        before != Some('=') && after.is_none_or(|c| !c.is_whitespace() && c != '=');
                    if opens {
                        open = Some((index, at, depth));
                        i = at + 2;
                        continue;
                    }
                }
                Some((open_index, open_offset, open_depth)) => {
                    let closes = after != Some('=')
                        && open_depth == depth
                        && before.is_none_or(|c| !c.is_whitespace() && c != '=');
                    if closes {
                        marks.push((open_index, open_offset, true));
                        marks.push((index, at, false));
                        open = None;
                        i = at + 2;
                        continue;
                    }
                }
            }
            i = at + 1;
        }
    }
    marks
}

/// Splits a text event at `==` marks (byte offset, is opening) and, if `tags` is set, `#tags`.
fn split_text<'a>(
    text: &str,
    marks: &[(usize, bool)],
    tags: bool,
    mut at_boundary: bool,
    out: &mut Vec<Event<'a>>,
) {
    let mut marks = marks.iter().peekable();
    let mut plain_start = 0;
    let mut i = 0;
    let flush = |out: &mut Vec<Event<'a>>, range: Range<usize>| {
        if !range.is_empty() {
            out.push(Event::Text(text[range].to_owned().into()));
        }
    };
    while i < text.len() {
        if let Some(&(_, open)) = marks.next_if(|(offset, _)| *offset == i) {
            flush(out, plain_start..i);
            out.push(Event::InlineHtml(
                if open { "<mark>" } else { "</mark>" }.into(),
            ));
            i += 2;
            plain_start = i;
            at_boundary = true;
            continue;
        }
        let c = text[i..].chars().next().unwrap();
        if c == '#' && tags && at_boundary {
            let tag = tag_at(&text[i + 1..]);
            if !tag.is_empty() {
                flush(out, plain_start..i);
                out.push(Event::InlineHtml(tag_html(tag).into()));
                i += 1 + tag.len();
                plain_start = i;
                at_boundary = false;
                continue;
            }
        }
        at_boundary = c.is_whitespace();
        i += c.len_utf8();
    }
    flush(out, plain_start..text.len());
}

/// Returns the tag at the start of `text` (after the `#`), or an empty string if there is none.
///
/// Tags consist of letters, numbers, `_`, `-` and `/`, and can't be all numbers.
fn tag_at(text: &str) -> &str {
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
        .unwrap_or(text.len());
    let tag = &text[..end];
    if tag.chars().all(|c| c.is_ascii_digit()) {
        ""
    } else {
        tag
    }
}

fn tag_html(tag: &str) -> String {
    format!(
        "<a class=\"tag\" href=\"{}\">#{}</a>",
        href(&format!("#{tag}")),
        text(tag)
    )
}

/// Removes Obsidian comments (`%%…%%`), except in code and math. An unclosed comment extends to
/// the end of the document.
fn strip_comments(markdown: &str) -> String {
    let protected: Vec<Range<usize>> = Parser::new_ext(markdown, options())
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Start(Tag::CodeBlock(_))
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_) => Some(range),
            _ => None,
        })
        .collect();
    let find_delimiter = |from: usize| {
        markdown[from..]
            .match_indices("%%")
            .map(|(i, _)| from + i)
            .find(|i| !protected.iter().any(|range| range.contains(i)))
    };

    let mut out = String::new();
    let mut copied = 0;
    while let Some(open) = find_delimiter(copied) {
        out.push_str(&markdown[copied..open]);
        copied = find_delimiter(open + 2).map_or(markdown.len(), |close| close + 2);
    }
    out.push_str(&markdown[copied..]);
    out
}

/// Takes an image's label (its alt text, or for a wikilink the part after `|`) from the end of
/// the reversed `events`, up to and including the image's end.
fn image_label(events: &mut Vec<Event>) -> String {
    let mut label = String::new();
    let mut depth = 0;
    while let Some(event) = events.pop() {
        match event {
            Event::Text(text) | Event::Code(text) => label.push_str(&text),
            Event::Start(Tag::Image { .. }) => depth += 1,
            Event::End(TagEnd::Image) if depth == 0 => break,
            Event::End(TagEnd::Image) => depth -= 1,
            _ => {}
        }
    }
    label
}

/// Called at the start of a blockquote. If it is a callout, consumes the callout marker from the
/// reversed `events`, pushes the callout's opening and the start of its title to `out`, and
/// returns the callout.
fn start_callout<'a>(events: &mut Vec<Event<'a>>, out: &mut Vec<Event<'a>>) -> Option<Callout> {
    let (Some(Event::Start(Tag::Paragraph)), Some(Event::Text(text))) =
        (events.last(), events.iter().rev().nth(1))
    else {
        return None;
    };
    let (callout, rest) = Callout::parse(text)?;
    let rest = rest.to_owned();
    events.pop(); // Start(Paragraph)
    events.pop(); // Text
    out.push(Event::Html(callout.open().into()));
    if !rest.is_empty() {
        out.push(Event::Text(rest.into()));
    } else if matches!(
        events.last(),
        Some(Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph))
    ) {
        out.push(Event::Text(callout.default_title().into()));
    }
    Some(callout)
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

    /// The element for the callout and its title: `<details>` and `<summary>` if it's foldable.
    fn elements(&self) -> (&'static str, &'static str) {
        match self.fold {
            Some(_) => ("details", "summary"),
            None => ("div", "div"),
        }
    }

    fn open(&self) -> String {
        let (outer, title) = self.elements();
        let open = if self.fold == Some(true) { " open" } else { "" };
        let kind = attr(&self.kind);
        format!(
            "<{outer} class=\"callout\" data-callout=\"{kind}\"{open}>\n<{title} class=\"callout-title\">"
        )
    }

    fn end_title(&self) -> String {
        format!(
            "</{}>\n<div class=\"callout-content\">\n",
            self.elements().1
        )
    }

    fn close(&self) -> String {
        format!("</div>\n</{}>\n", self.elements().0)
    }
}

/// Renders a code block, taking its contents from the end of the reversed `events`.
fn code_block(kind: &CodeBlockKind, events: &mut Vec<Event>) -> String {
    let mut code = String::new();
    while let Some(event) = events.pop() {
        match event {
            Event::Text(text) => code.push_str(&text),
            Event::End(TagEnd::CodeBlock) => break,
            _ => {}
        }
    }
    let lang = match kind {
        CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
        CodeBlockKind::Indented => "",
    };
    let class = if lang.is_empty() {
        String::new()
    } else {
        format!(" class=\"language-{}\"", attr(lang))
    };
    let code = highlight::highlight(lang, &code).unwrap_or_else(|| text(&code));
    format!("<pre><code{class}>{code}</code></pre>\n")
}

/// Turns a wikilink target like `Note#Heading` or `#^block` into a URL.
fn wikilink_href(target: &str) -> String {
    let Some((page, fragment)) = target.split_once('#') else {
        return target.trim().to_owned();
    };
    // `Note#Heading#Subheading` links to the subheading.
    let fragment = fragment.rsplit('#').next().unwrap().trim();
    if fragment.starts_with('^') {
        format!("{}#{fragment}", page.trim())
    } else {
        format!("{}#{}", page.trim(), slugify(fragment))
    }
}

/// Turns heading text into an `id`, e.g. `Hello, World!` into `hello-world`.
fn slugify(text: &str) -> String {
    let mut slug = String::new();
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

/// Moves figures (the only block-level HTML inside paragraphs) out of their paragraphs, splitting
/// the paragraph around them, since `<p>` can't contain `<figure>`.
fn hoist_figures(events: Vec<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    let mut in_paragraph = false;
    for event in events {
        match event {
            Event::Start(Tag::Paragraph) => {
                in_paragraph = true;
                out.push(event);
            }
            Event::End(TagEnd::Paragraph) => {
                in_paragraph = false;
                close_paragraph(&mut out);
            }
            Event::Html(_) if in_paragraph => {
                close_paragraph(&mut out);
                out.push(event);
                out.push(Event::Start(Tag::Paragraph));
            }
            // Don't start the rest of a split paragraph with a line break.
            _ if is_blank(&event) && out.last() == Some(&Event::Start(Tag::Paragraph)) => {}
            event => out.push(event),
        }
    }
    out
}

/// Ends the paragraph at the end of `out`, removing it if it is empty.
fn close_paragraph(out: &mut Vec<Event>) {
    while out.last().is_some_and(is_blank) {
        out.pop();
    }
    if out.last() == Some(&Event::Start(Tag::Paragraph)) {
        out.pop();
    } else {
        out.push(Event::End(TagEnd::Paragraph));
    }
}

fn is_blank(event: &Event) -> bool {
    match event {
        Event::SoftBreak | Event::HardBreak => true,
        Event::Text(text) => text.trim().is_empty(),
        _ => false,
    }
}

/// Whether a link points to another site rather than into the vault.
fn is_remote(link: &str) -> bool {
    link.starts_with("//")
        || link.split_once(':').is_some_and(|(scheme, _)| {
            scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        })
}

fn is_image(target: &str) -> bool {
    vault::image_mime_type(Path::new(target.split('#').next().unwrap())).is_some()
}

/// Renders an Obsidian embed of something other than an image, `![[target]]` or
/// `![[target|label]]`.
///
/// Audio, video and PDFs are embedded. Other files, like notes, can't be transcluded, so they
/// become links.
fn media_embed(target: &str, label: Option<&str>) -> String {
    let path = target.split('#').next().unwrap().trim();
    let extension = path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let src = href(path);
    match extension.as_str() {
        "mp3" | "wav" | "m4a" | "ogg" | "flac" | "3gp" | "oga" | "opus" => {
            format!("<audio controls src=\"{src}\"></audio>")
        }
        "mp4" | "webm" | "ogv" | "mov" | "mkv" => format!("<video controls src=\"{src}\"></video>"),
        "pdf" => {
            let title = attr(label.unwrap_or(path.rsplit('/').next().unwrap()));
            format!("<iframe src=\"{src}\" title=\"{title}\"></iframe>")
        }
        _ => format!(
            "<a class=\"embed\" href=\"{}\">{}</a>",
            href(&wikilink_href(target)),
            text(label.unwrap_or(target))
        ),
    }
}

/// Escapes text for an HTML attribute value.
fn attr(s: &str) -> String {
    let mut out = String::new();
    escape_html(&mut out, s).unwrap();
    out
}

/// Escapes text for HTML element content.
fn text(s: &str) -> String {
    let mut out = String::new();
    escape_html_body_text(&mut out, s).unwrap();
    out
}

/// Escapes a URL for an `href` or `src` attribute value.
fn href(s: &str) -> String {
    let mut out = String::new();
    escape_href(&mut out, s).unwrap();
    out
}

/// Parses an embed size like `300` or `300x200` into `width` and `height` attributes.
fn size_attrs(size: &str) -> Option<String> {
    let size = size.trim();
    Some(match size.split_once('x') {
        Some((width, height)) => {
            let (width, height): (u32, u32) = (width.parse().ok()?, height.parse().ok()?);
            format!(" width=\"{width}\" height=\"{height}\"")
        }
        None => format!(" width=\"{}\"", size.parse::<u32>().ok()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders markdown that doesn't reference any vault files.
    fn to_html(markdown: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".obsidian")).unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        super::to_html(markdown, &vault, Path::new("")).unwrap()
    }

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
        // A `$$` block on its own isn't wrapped in a paragraph.
        let html = to_html("$$\nx\n$$\n");
        assert!(html.starts_with("<math display=\"block\">"), "{html}");
        assert!(html.ends_with("</math>\n"), "{html}");
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
    fn media_embeds() {
        assert_eq!(
            to_html("![[song.mp3]]"),
            "<p><audio controls src=\"song.mp3\"></audio></p>\n"
        );
        assert_eq!(
            to_html("![[Other Note]]"),
            "<p><a class=\"embed\" href=\"Other%20Note\">Other Note</a></p>\n"
        );
        assert_eq!(
            to_html("![[a&b.pdf]]"),
            "<p><iframe src=\"a&amp;b.pdf\" title=\"a&amp;b.pdf\"></iframe></p>\n"
        );
    }

    /// A vault with `img/cat.png` (containing `cat`) and `notes/dog.svg` (containing `dog`).
    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".obsidian")).unwrap();
        std::fs::create_dir_all(dir.path().join("img")).unwrap();
        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("img/cat.png"), "cat").unwrap();
        std::fs::write(dir.path().join("notes/dog.svg"), "dog").unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        (dir, vault)
    }

    fn render_in_vault(markdown: &str) -> Result<String, Vec<String>> {
        let (_dir, vault) = vault();
        super::to_html(markdown, &vault, Path::new("notes"))
    }

    const CAT: &str = "data:image/png;base64,Y2F0";
    const DOG: &str = "data:image/svg+xml;base64,ZG9n";

    #[test]
    fn images_without_alt_text() {
        let html = |md| render_in_vault(md).unwrap();
        assert_eq!(
            html("![[cat.png]]"),
            format!("<p><img src=\"{CAT}\" alt=\"\"></p>\n")
        );
        assert_eq!(
            html("A ![[cat.png|300x200]] B"),
            format!("<p>A <img src=\"{CAT}\" alt=\"\" width=\"300\" height=\"200\"> B</p>\n")
        );
        assert_eq!(
            html("![](dog.svg)"),
            format!("<p><img src=\"{DOG}\" alt=\"\"></p>\n")
        );
        assert_eq!(
            html("![](../img/cat.png)"),
            format!("<p><img src=\"{CAT}\" alt=\"\"></p>\n")
        );
    }

    #[test]
    fn images_with_alt_text_are_figures() {
        let html = |md| render_in_vault(md).unwrap();
        let figure = |img: &str, caption: &str| {
            format!("<figure>\n{img}\n<figcaption>{caption}</figcaption>\n</figure>\n")
        };
        assert_eq!(
            html("![[cat.png|A cat]]"),
            figure(&format!("<img src=\"{CAT}\" alt=\"A cat\">"), "A cat")
        );
        assert_eq!(
            html("![[cat.png|Tom & \"Jerry\"|300]]"),
            figure(
                &format!("<img src=\"{CAT}\" alt=\"Tom &amp; &quot;Jerry&quot;\" width=\"300\">"),
                "Tom &amp; \"Jerry\""
            )
        );
        assert_eq!(
            html("![A *dog*](dog.svg \"Title\")"),
            figure(
                &format!("<img src=\"{DOG}\" alt=\"A dog\" title=\"Title\">"),
                "A dog"
            )
        );
        assert_eq!(
            html("![A cat](https://example.com/cat.png?a=1&b=2)"),
            figure(
                "<img src=\"https://example.com/cat.png?a=1&amp;b=2\" alt=\"A cat\">",
                "A cat"
            )
        );
    }

    #[test]
    fn figures_split_paragraphs() {
        let html = render_in_vault("Before\n![[cat.png|Cat]]\nafter *it*.").unwrap();
        assert_eq!(
            html,
            format!(
                "<p>Before</p>\n<figure>\n<img src=\"{CAT}\" alt=\"Cat\">\n\
                 <figcaption>Cat</figcaption>\n</figure>\n<p>after <em>it</em>.</p>\n"
            )
        );
    }

    #[test]
    fn images_in_links_are_not_figures() {
        let html = render_in_vault("[![Cat](cat.png)](https://example.com)").unwrap();
        assert_eq!(
            html,
            format!("<p><a href=\"https://example.com\"><img src=\"{CAT}\" alt=\"Cat\"></a></p>\n")
        );
    }

    #[test]
    fn missing_images_are_errors() {
        let errors =
            render_in_vault("![[missing.png]]\n\n![x](nope.jpg)\n\n![[cat.png]]").unwrap_err();
        assert_eq!(
            errors,
            [
                "image not found in vault: missing.png",
                "image not found in vault: nope.jpg"
            ]
        );
    }

    #[test]
    fn math_macros_persist() {
        let html = to_html("$\\newcommand{\\myreals}{\\mathbb{R}}$ $x \\in \\myreals$");
        assert!(html.contains("ℝ"), "{html}");
    }

    #[test]
    fn math_errors_are_reported() {
        let errors = render_in_vault("$\\nope$ and $$\\alsonope$$ but $x$").unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(
            errors[0].starts_with("cannot render math `\\nope`"),
            "{errors:?}"
        );
        assert!(
            errors[1].starts_with("cannot render math `\\alsonope`"),
            "{errors:?}"
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
    fn highlights() {
        assert_eq!(to_html("a ==b== c"), "<p>a <mark>b</mark> c</p>\n");
        assert_eq!(
            to_html("==a **b** c=="),
            "<p><mark>a <strong>b</strong> c</mark></p>\n"
        );
        assert_eq!(to_html("x === y == z"), "<p>x === y == z</p>\n");
        assert_eq!(to_html("==unclosed"), "<p>==unclosed</p>\n");
        // Marks crossing other inline elements would produce invalid HTML.
        assert_eq!(to_html("==a *b== c*"), "<p>==a <em>b== c</em></p>\n");
        assert_eq!(to_html("`==code==`"), "<p><code>==code==</code></p>\n");
    }

    #[test]
    fn tags() {
        assert_eq!(
            to_html("#tag and #nested/tag-1, not#tag or #123"),
            "<p><a class=\"tag\" href=\"#tag\">#tag</a> and \
             <a class=\"tag\" href=\"#nested/tag-1\">#nested/tag-1</a>, not#tag or #123</p>\n"
        );
        assert_eq!(
            to_html("[link #x](y)"),
            "<p><a href=\"y\">link #x</a></p>\n"
        );
    }

    #[test]
    fn heading_ids() {
        assert_eq!(
            to_html("# Hello *World*\n## Hello World\n## `x` y"),
            "<h1 id=\"hello-world\">Hello <em>World</em></h1>\n\
             <h2 id=\"hello-world-1\">Hello World</h2>\n\
             <h2 id=\"x-y\"><code>x</code> y</h2>\n"
        );
    }

    #[test]
    fn block_ids() {
        assert_eq!(
            to_html("Some text ^abc-1"),
            "<p id=\"^abc-1\">Some text</p>\n"
        );
        assert_eq!(to_html("Some text\n^abc"), "<p id=\"^abc\">Some text</p>\n");
        assert_eq!(
            to_html("- item ^x\n- other"),
            "<ul>\n<li id=\"^x\">item</li>\n<li>other</li>\n</ul>\n"
        );
        assert_eq!(to_html("2^10"), "<p>2^10</p>\n");
    }

    #[test]
    fn comments() {
        assert_eq!(to_html("a %%hidden%% b"), "<p>a  b</p>\n");
        assert_eq!(
            to_html("a\n\n%%\nblock\n\n# x\n%%\n\nb"),
            "<p>a</p>\n<p>b</p>\n"
        );
        assert_eq!(to_html("`%%code%%`"), "<p><code>%%code%%</code></p>\n");
        assert_eq!(to_html("a %% rest\n\nmore"), "<p>a</p>\n");
        let html = to_html("$$\n50\\%% x\n$$\n");
        // `\%` is a percent sign and the following `%` starts a LaTeX comment.
        assert!(html.contains("<mn>50</mn><mi>%</mi>"), "{html}");
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  Ünïcode  — dash "), "ünïcode-dash");
        assert_eq!(slugify("!!!"), "section");
    }
}
