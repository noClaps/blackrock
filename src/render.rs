//! Markdown to HTML rendering, with Obsidian's extensions to markdown.

use std::collections::HashMap;
use std::fmt::Write;
use std::ops::Range;
use std::path::Path;

use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{
    CodeBlockKind, CowStr, Event, LinkType, Options, Parser, Tag, TagEnd, TextMergeStream, html,
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
    let events = TextMergeStream::new(Parser::new_ext(&markdown, options())).collect();
    let events = standalone_block_ids(events);
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
    let events = footnotes(renderer.inlines(events));
    if !renderer.errors.is_empty() {
        return Err(renderer.errors);
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    Ok(add_block_ids(&out))
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
                    } else if let Some((page, id)) = dest_url.split_once("#^")
                        && is_block_id(id)
                    {
                        // Block ids are rendered without their `^`.
                        *dest_url = format!("{page}#{id}").into();
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
                    id,
                }) => {
                    let label = image_label(&mut events);
                    match link_type {
                        // Other files, like notes, can't be embedded, so they become links.
                        LinkType::WikiLink { .. } if !is_image(&dest_url) => {
                            out.push(Event::Start(Tag::Link {
                                link_type,
                                dest_url: wikilink_href(&dest_url).into(),
                                title,
                                id,
                            }));
                            out.extend(label);
                            out.push(Event::End(TagEnd::Link));
                        }
                        // The parser never puts wikilink images inside links.
                        LinkType::WikiLink { has_pothole } => {
                            let label = if has_pothole { label } else { Vec::new() };
                            out.push(self.image(&dest_url, label, true, "", false));
                        }
                        _ => {
                            let link = percent_decode_str(&dest_url).decode_utf8_lossy();
                            let in_link = link_depth > 0;
                            out.push(self.image(&link, label, false, &title, in_link));
                        }
                    }
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
            Some(Event::Start(Tag::Paragraph | Tag::Item)) => {
                if let Some(block_id) = strip_block_id(&mut run) {
                    out.insert(out.len() - 1, block_id_marker(&block_id));
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
                Event::InlineMath(_) | Event::DisplayMath(_) => processed.push(self.math(event)),
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
    /// wikilinks, the label can also be just a size. An image with alt text becomes a figure,
    /// unless it is inside a link, with the label's markup (and rendered math) as its caption and
    /// its plain text (with math as LaTeX) as the alt text.
    fn image<'a>(
        &mut self,
        link: &str,
        mut label: Vec<Event>,
        is_wikilink: bool,
        title: &str,
        in_link: bool,
    ) -> Event<'a> {
        // The size follows the last `|`, which must be in the label's last text.
        let whole_label_is_size = is_wikilink && label.len() == 1;
        let mut size = String::new();
        if let Some(Event::Text(last)) = label.last_mut()
            && let Some((alt, attrs)) = (last.rsplit_once('|'))
                .or(whole_label_is_size.then_some(("", last)))
                .and_then(|(alt, size)| Some((alt.to_owned(), size_attrs(size)?)))
        {
            size = attrs;
            *last = alt.into();
        }
        let alt = plain_text(&label);
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
            let mut figcaption = String::new();
            let label = label.into_iter().map(|event| self.math(event));
            html::push_html(&mut figcaption, label);
            let figcaption = figcaption.trim();
            // A block-level event, which `hoist_figures` moves out of paragraphs.
            Event::Html(
                format!("<figure>\n{html}\n<figcaption>{figcaption}</figcaption>\n</figure>\n")
                    .into(),
            )
        }
    }

    /// Converts a math event's LaTeX to a MathML Core `<math>` element. Other events are returned
    /// unchanged.
    fn math<'a>(&mut self, event: Event<'a>) -> Event<'a> {
        let (latex, display) = match &event {
            Event::InlineMath(latex) => (latex, MathDisplay::Inline),
            Event::DisplayMath(latex) => (latex, MathDisplay::Block),
            _ => return event,
        };
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
            Event::Text(s) | Event::Code(s) | Event::InlineMath(s) | Event::DisplayMath(s) => {
                text.push_str(s);
            }
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

/// Removes a trailing ` ^block-id` from a paragraph or list item, returning `block-id`.
fn strip_block_id(run: &mut Vec<Event>) -> Option<String> {
    let Some(Event::Text(text)) = run.last() else {
        return None;
    };
    let trimmed = text.trim_end();
    let caret = trimmed.rfind('^')?;
    let id = &trimmed[caret + 1..];
    let before = &trimmed[..caret];
    // The id must be separated from the content before it, and there must be content.
    let separated = before.ends_with(char::is_whitespace)
        || (before.is_empty()
            && run.len() > 1
            && matches!(run[run.len() - 2], Event::SoftBreak | Event::HardBreak));
    if !is_block_id(id) || !separated {
        return None;
    }
    let block_id = id.to_owned();
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

fn is_block_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Gives a block id on its own line, like `^id` after a table or a `$$…$$` block, to the block
/// before it. `pulldown-cmark` can't add attributes to most elements, so this puts a marker before
/// the block, which [`add_block_ids`] turns into an `id` on its opening tag.
fn standalone_block_ids(mut events: Vec<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    // Reversed so that we can pop events off the end and peek at the next ones.
    events.reverse();
    // The `out` index of each open element's start.
    let mut starts = Vec::new();
    // For each open element (and the document), the `out` index of its last child block's start.
    let mut last_child = vec![None];
    while let Some(event) = events.pop() {
        let id = match (&event, events.last(), events.iter().rev().nth(1)) {
            (
                Event::Start(Tag::Paragraph),
                Some(Event::Text(text)),
                Some(Event::End(TagEnd::Paragraph)),
            ) => text.trim().strip_prefix('^').filter(|id| is_block_id(id)),
            _ => None,
        };
        if let (Some(id), Some(Some(start))) = (id, last_child.last()) {
            out.insert(*start, block_id_marker(id));
            events.pop(); // Text
            events.pop(); // End(Paragraph)
            continue;
        }
        match event {
            Event::Start(_) => {
                starts.push(out.len());
                last_child.push(None);
            }
            Event::End(_) => {
                last_child.pop();
                *last_child.last_mut().unwrap() = starts.pop();
            }
            _ => *last_child.last_mut().unwrap() = Some(out.len()),
        }
        out.push(event);
    }
    out
}

/// A marker that [`add_block_ids`] turns into an `id` on the opening tag of the element after it.
fn block_id_marker<'a>(id: &str) -> Event<'a> {
    // NUL never appears in the output otherwise, since the parser replaces it.
    Event::Html(format!("\0{id}\0\n").into())
}

/// Replaces each [`block_id_marker`] with an `id` on the element after it,
/// unless that element already has one.
fn add_block_ids(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some((before, marked)) = rest.split_once('\0') {
        let (id, after) = marked.split_once("\0\n").unwrap();
        out.push_str(before);
        rest = after;
        let tag = &after[..after.find('>').unwrap_or(after.len())];
        if let Some(name) = tag.strip_prefix('<')
            && !tag.contains(" id=")
        {
            let name_end = 1 + name
                .find(|c: char| !c.is_ascii_alphanumeric())
                .unwrap_or(name.len());
            // Writing to a `String` can't fail.
            let _ = write!(out, "{} id=\"{id}\"", &after[..name_end]);
            rest = &after[name_end..];
        }
    }
    out.push_str(rest);
    out
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
                None if before != Some('=')
                    && after.is_none_or(|c| !c.is_whitespace() && c != '=') =>
                {
                    open = Some((index, at, depth));
                }
                Some((open_index, open_offset, open_depth))
                    if after != Some('=')
                        && open_depth == depth
                        && before.is_none_or(|c| !c.is_whitespace() && c != '=') =>
                {
                    marks.push((open_index, open_offset, true));
                    marks.push((index, at, false));
                    open = None;
                }
                _ => {}
            }
            // No mark can start at `at + 1`, since marks can't be next to another `=`.
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
///
/// Only text, code, math, links, emphasis and line breaks are kept. Nested images are reduced to
/// their labels.
fn image_label<'a>(events: &mut Vec<Event<'a>>) -> Vec<Event<'a>> {
    let mut label = Vec::new();
    let mut depth = 0;
    while let Some(event) = events.pop() {
        match event {
            Event::Start(Tag::Image { .. }) => depth += 1,
            Event::End(TagEnd::Image) if depth == 0 => break,
            Event::End(TagEnd::Image) => depth -= 1,
            Event::Start(Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. })
            | Event::End(
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link,
            )
            | Event::Text(_)
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::SoftBreak
            | Event::HardBreak => label.push(event),
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
    // Code blocks only contain text, so this stops after popping the block's end.
    while let Some(Event::Text(text)) = events.pop() {
        code.push_str(&text);
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

/// Turns a wikilink target like `Note#Heading` or `#^block` into a URL, like `Note#heading` or
/// `#block`.
fn wikilink_href(target: &str) -> String {
    let Some((page, fragment)) = target.split_once('#') else {
        return target.trim().to_owned();
    };
    // `Note#Heading#Subheading` links to the subheading.
    let fragment = fragment.rsplit('#').next().unwrap().trim();
    if let Some(block_id) = fragment.strip_prefix('^') {
        format!("{}#{block_id}", page.trim())
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

/// Moves footnote definitions into a list at the end of the document, numbered in the order
/// they are first referenced, each with a link back to its first reference. Definitions that are
/// never referenced are dropped, as on GitHub.
fn footnotes(events: Vec<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    let mut definitions = HashMap::new();
    // The definition being collected, if any.
    let mut definition: Option<(CowStr, Vec<Event>)> = None;
    // Referenced labels, in the order they are first referenced.
    let mut labels = Vec::new();
    for event in events {
        match event {
            Event::Start(Tag::FootnoteDefinition(label)) => definition = Some((label, Vec::new())),
            Event::End(TagEnd::FootnoteDefinition) => {
                let (label, body) = definition.take().expect("definitions don't nest");
                // Like links, the first definition of a label wins.
                definitions.entry(label).or_insert(body);
            }
            // References in definitions are numbered when the definition is output, so that
            // references in unused definitions don't count.
            event => match &mut definition {
                Some((_, body)) => body.push(event),
                None => out.push(footnote_reference(event, &mut labels)),
            },
        }
    }

    if labels.is_empty() {
        return out;
    }
    out.push(Event::Html("<section class=\"footnotes\">\n<hr />\n<ol>\n".into()));
    // Outputting a definition can reference more footnotes, which are added to the end.
    let mut index = 0;
    while let Some(label) = labels.get(index) {
        index += 1;
        let mut body = definitions.remove(label).unwrap_or_default();
        out.push(Event::Html(format!("<li id=\"fn-{index}\">\n").into()));
        let backref = format!(
            "<a href=\"#fnref-{index}\" class=\"footnote-backref\" \
             aria-label=\"Back to reference {index}\">\u{21a9}\u{fe0e}</a>"
        );
        // The link back goes at the end of the last paragraph, or in a paragraph of its own.
        if body.last() == Some(&Event::End(TagEnd::Paragraph)) {
            body.insert(
                body.len() - 1,
                Event::InlineHtml(format!(" {backref}").into()),
            );
        } else {
            body.push(Event::Start(Tag::Paragraph));
            body.push(Event::InlineHtml(backref.into()));
            body.push(Event::End(TagEnd::Paragraph));
        }
        for event in body {
            out.push(footnote_reference(event, &mut labels));
        }
        out.push(Event::Html("</li>\n".into()));
    }
    out.push(Event::Html("</ol>\n</section>\n".into()));
    out
}

/// Renders a footnote reference as a link to its footnote, numbering it if it is the first
/// reference to it. Other events are returned unchanged.
fn footnote_reference<'a>(event: Event<'a>, labels: &mut Vec<CowStr<'a>>) -> Event<'a> {
    let Event::FootnoteReference(label) = event else {
        return event;
    };
    let (n, id) = if let Some(index) = labels.iter().position(|l| *l == label) {
        (index + 1, String::new())
    } else {
        labels.push(label);
        (labels.len(), format!(" id=\"fnref-{}\"", labels.len()))
    };
    Event::InlineHtml(
        format!("<sup class=\"footnote-ref\"><a href=\"#fn-{n}\"{id}>{n}</a></sup>").into(),
    )
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
    use crate::vault::tests::TempDir;

    /// Renders markdown that doesn't reference any vault files.
    fn to_html(markdown: &str) -> String {
        let dir = TempDir::new();
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
        let html = to_html("$$a$$ $$b$$");
        assert!(html.starts_with("<math display=\"block\">"), "{html}");
        // With other text, it stays in the paragraph.
        let html = to_html("a $$x$$");
        assert!(html.starts_with("<p>a <math display=\"block\">"), "{html}");
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
            "<p><a href=\"#abc\">block</a></p>\n"
        );
    }

    #[test]
    fn gfm_extensions() {
        assert_eq!(
            to_html("| a |\n| - |\n| b |\n\n~~s~~ x[^1]\n\n- [ ] t\n\n[^1]: n"),
            "<table><thead><tr><th>a</th></tr></thead><tbody>\n<tr><td>b</td></tr>\n</tbody></table>\n\
             <p><del>s</del> x<sup class=\"footnote-ref\"><a href=\"#fn-1\" id=\"fnref-1\">1</a></sup></p>\n\
             <ul>\n<li><input disabled=\"\" type=\"checkbox\"/>\nt</li>\n</ul>\n\
             <section class=\"footnotes\">\n<hr />\n<ol>\n<li id=\"fn-1\">\n\
             <p>n <a href=\"#fnref-1\" class=\"footnote-backref\" aria-label=\"Back to reference 1\">\u{21a9}\u{fe0e}</a></p>\n\
             </li>\n</ol>\n</section>\n"
        );
    }

    #[test]
    fn footnotes() {
        let backref = |n| {
            format!(
                "<a href=\"#fnref-{n}\" class=\"footnote-backref\" \
                 aria-label=\"Back to reference {n}\">\u{21a9}\u{fe0e}</a>"
            )
        };
        // Footnotes are numbered by first reference, wherever they are defined. Repeated
        // references link to the same footnote, and undefined references stay as text.
        assert_eq!(
            to_html("[^b]: Bee.\n\nA[^a] B[^b] A[^a] X[^x]\n\n[^a]: Ay."),
            format!(
                "<p>A<sup class=\"footnote-ref\"><a href=\"#fn-1\" id=\"fnref-1\">1</a></sup> \
                 B<sup class=\"footnote-ref\"><a href=\"#fn-2\" id=\"fnref-2\">2</a></sup> \
                 A<sup class=\"footnote-ref\"><a href=\"#fn-1\">1</a></sup> X[^x]</p>\n\
                 <section class=\"footnotes\">\n<hr />\n<ol>\n\
                 <li id=\"fn-1\">\n<p>Ay. {}</p>\n</li>\n\
                 <li id=\"fn-2\">\n<p>Bee. {}</p>\n</li>\n\
                 </ol>\n</section>\n",
                backref(1),
                backref(2)
            )
        );
        // Footnotes referenced from other footnotes come after them. Unused footnotes, and the
        // references in them, are dropped. A footnote not ending in a paragraph gets its link
        // back in a paragraph of its own.
        assert_eq!(
            to_html(
                "A[^a]\n\n[^a]: See[^b].\n\n[^b]: Code:\n\n        x\n\n[^u]: Uses[^c].\n\n[^c]: C."
            ),
            format!(
                "<p>A<sup class=\"footnote-ref\"><a href=\"#fn-1\" id=\"fnref-1\">1</a></sup></p>\n\
                 <section class=\"footnotes\">\n<hr />\n<ol>\n\
                 <li id=\"fn-1\">\n<p>See<sup class=\"footnote-ref\"><a href=\"#fn-2\" id=\"fnref-2\">2</a></sup>. {}</p>\n</li>\n\
                 <li id=\"fn-2\">\n<p>Code:</p>\n<pre><code>x\n</code></pre>\n<p>{}</p>\n</li>\n\
                 </ol>\n</section>\n",
                backref(1),
                backref(2)
            )
        );
    }

    #[test]
    fn other_embeds_are_links() {
        assert_eq!(
            to_html("![[Other Note#Part]]"),
            "<p><a href=\"Other%20Note#part\">Other Note#Part</a></p>\n"
        );
        assert_eq!(
            to_html("![[a&b.pdf|The PDF]]"),
            "<p><a href=\"a&amp;b.pdf\">The PDF</a></p>\n"
        );
    }

    /// A vault with `img/cat.png` (containing `cat`) and `notes/dog.svg` (containing `dog`).
    fn vault() -> (TempDir, Vault) {
        let dir = TempDir::new();
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
                "A <em>dog</em>"
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
    fn math_in_captions() {
        let html = render_in_vault("![Area $r^2$ and $$x<y$$|300](cat.png)").unwrap();
        assert_eq!(
            html,
            format!(
                "<figure>\n<img src=\"{CAT}\" alt=\"Area r^2 and x&lt;y\" width=\"300\">\n\
                 <figcaption>Area <math><msup><mi>r</mi><mn>2</mn></msup></math> and \
                 <math display=\"block\"><mi>x</mi><mo>&lt;</mo><mi>y</mi></math>\
                 </figcaption>\n</figure>\n"
            )
        );
        // Images in links aren't figures, but keep the math in their alt text.
        assert_eq!(
            render_in_vault("[![A $b$](cat.png)](u)").unwrap(),
            format!("<p><a href=\"u\"><img src=\"{CAT}\" alt=\"A b\"></a></p>\n")
        );
        let errors = render_in_vault("![Bad $\\nope$](cat.png)").unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].starts_with("cannot render math `\\nope`"),
            "{errors:?}"
        );
    }

    #[test]
    fn links_in_captions() {
        let html = |md| render_in_vault(md).unwrap();
        let figure = |alt: &str, size: &str, caption: &str| {
            format!(
                "<figure>\n<img src=\"{CAT}\" alt=\"{alt}\"{size}>\n\
                 <figcaption>{caption}</figcaption>\n</figure>\n"
            )
        };
        assert_eq!(
            html("![A [cat](https://example.com/?a=1&b=2 \"Cat\") here|300](cat.png)"),
            figure(
                "A cat here",
                " width=\"300\"",
                "A <a href=\"https://example.com/?a=1&amp;b=2\" title=\"Cat\">cat</a> here"
            )
        );
        assert_eq!(
            html("![See [**`a<b`**](u)\nnow](cat.png)"),
            figure(
                "See a&lt;b now",
                "",
                "See <a href=\"u\"><strong><code>a&lt;b</code></strong></a>\nnow"
            )
        );
        // A size can't contain markup.
        assert_eq!(
            html("![[cat.png|A *cat*|3**0**0]]"),
            figure("A cat|300", "", "A <em>cat</em>|3<strong>0</strong>0")
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
    fn nested_images_use_the_inner_alt_text() {
        assert_eq!(
            render_in_vault("![![inner](cat.png)](cat.png)").unwrap(),
            format!(
                "<figure>\n<img src=\"{CAT}\" alt=\"inner\">\n\
                 <figcaption>inner</figcaption>\n</figure>\n"
            )
        );
    }

    #[test]
    fn images_after_links_are_figures() {
        let html = render_in_vault("[a](b) ![Cat](cat.png)").unwrap();
        assert!(
            html.starts_with("<p><a href=\"b\">a</a></p>\n<figure>"),
            "{html}"
        );
    }

    #[test]
    fn adjacent_figures_have_no_paragraph_between() {
        let html = render_in_vault("![[cat.png|A]] ![[cat.png|B]]").unwrap();
        assert!(html.contains("</figure>\n<figure>"), "{html}");
    }

    #[test]
    fn missing_images_are_errors() {
        let errors = render_in_vault("![[missing.png]]\n\n![x](nope.jpg)\n\n![[cat.png]]\n\n![]()")
            .unwrap_err();
        assert_eq!(
            errors,
            [
                "image not found in vault: missing.png",
                "image not found in vault: nope.jpg",
                "image not found in vault: ",
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
    fn quotes_that_are_not_callouts() {
        assert_eq!(
            to_html("> **bold**"),
            "<blockquote>\n<p><strong>bold</strong></p>\n</blockquote>\n"
        );
        assert_eq!(
            to_html("> [!] x"),
            "<blockquote>\n<p>[!] x</p>\n</blockquote>\n"
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
        assert_eq!(to_html("a == b=="), "<p>a == b==</p>\n");
        assert_eq!(to_html("==a =="), "<p>==a ==</p>\n");
        assert_eq!(to_html("==a==b=="), "<p><mark>a</mark>b==</p>\n");
        // An unclosed mark inside emphasis doesn't stop later marks.
        assert_eq!(
            to_html("*==a* ==b=="),
            "<p><em>==a</em> <mark>b</mark></p>\n"
        );
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
        assert_eq!(
            to_html("[a](b) #x"),
            "<p><a href=\"b\">a</a> <a class=\"tag\" href=\"#x\">#x</a></p>\n"
        );
    }

    #[test]
    fn multiline_heading_ids() {
        assert_eq!(
            to_html("Two\nlines\n==="),
            "<h1 id=\"two-lines\">Two\nlines</h1>\n"
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
            "<p id=\"abc-1\">Some text</p>\n"
        );
        assert_eq!(to_html("Some text\n^abc"), "<p id=\"abc\">Some text</p>\n");
        assert_eq!(
            to_html("- item ^x\n- other"),
            "<ul>\n<li id=\"x\">item</li>\n<li>other</li>\n</ul>\n"
        );
        assert_eq!(to_html("2^10"), "<p>2^10</p>\n");
        assert_eq!(to_html("a ^x!"), "<p>a ^x!</p>\n");
        assert_eq!(to_html("^abc"), "<p>^abc</p>\n");
        // Links to block ids drop the `^` too.
        assert_eq!(
            to_html("[a](#^abc) [b](Note#^x-1) [c](#^no!) [d](^e)"),
            "<p><a href=\"#abc\">a</a> <a href=\"Note#x-1\">b</a> \
             <a href=\"#^no!\">c</a> <a href=\"^e\">d</a></p>\n"
        );
    }

    #[test]
    fn standalone_block_ids_go_on_the_previous_block() {
        assert_eq!(
            to_html("$$\nx\n$$\n\n^math"),
            "<math id=\"math\" display=\"block\"><mi>x</mi></math>\n"
        );
        assert_eq!(
            to_html("| a |\n|--:|\n| 1 |\n\n^t"),
            "<table id=\"t\"><thead><tr><th style=\"text-align: right\">a</th></tr></thead>\
             <tbody>\n<tr><td style=\"text-align: right\">1</td></tr>\n</tbody></table>\n"
        );
        assert_eq!(
            to_html("> [!tip]- Hi\n> body\n\n^c"),
            "<details id=\"c\" class=\"callout\" data-callout=\"tip\">\n\
             <summary class=\"callout-title\">Hi</summary>\n\
             <div class=\"callout-content\">\n<p>body</p>\n</div>\n</details>\n"
        );
        // Only the nearest block, inside the quote, gets the id.
        assert_eq!(
            to_html("> a\n>\n> - b\n>\n> ^l\n\n---\n\n^r"),
            "<blockquote>\n<p>a</p>\n<ul id=\"l\">\n<li>b</li>\n</ul>\n</blockquote>\n\
             <hr id=\"r\" />\n"
        );
        assert_eq!(
            to_html("```\ncode\n```\n\n^c"),
            "<pre id=\"c\"><code>code\n</code></pre>\n"
        );
        // Elements that already have an id keep it.
        assert_eq!(to_html("a ^first\n\n^second"), "<p id=\"first\">a</p>\n");
        assert_eq!(to_html("# A\n\n^b"), "<h1 id=\"a\">A</h1>\n");
        // Without a block before it, it stays text.
        assert_eq!(to_html("^a\n\n^b"), "<p id=\"b\">^a</p>\n");
    }

    #[test]
    fn comments() {
        assert_eq!(to_html("a %%hidden%% b"), "<p>a  b</p>\n");
        assert_eq!(to_html("%%x%% a"), "<p>a</p>\n");
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
        assert_eq!(slugify("a - b_c"), "a-b-c");
        assert_eq!(slugify("a,b-c"), "ab-c");
    }
}
