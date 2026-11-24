use std::{
    collections::HashMap,
    fmt::Write,
    fs,
    path::{Path, PathBuf},
};

use ferromark::{
    Allocator, HeadingIdPlanner, ParseResult, Parser, ParserOptions,
    ast::{AlignKind, BlockQuote, Document, FootnoteDefinition, Image, List, Node, Table},
    collect_heading_text, slugify_heading,
};
use math_core::{LatexToMathML, MathCoreConfig, MathDisplay};
use percent_encoding::percent_decode_str;

use crate::{
    escape::{attr, href, text},
    highlight,
    vault::Vault,
};

fn options() -> ParserOptions {
    ParserOptions {
        math: true,
        highlight: true,
        image_alt_content: true,
        autolinks: false,
        // Frontmatter isn't rendered.
        front_matter: true,
        ..ParserOptions::gfm()
    }
}

/// Parses an Obsidian-flavored markdown note, including its frontmatter.
pub fn parse<'a>(arena: &'a Allocator, source: &'a str) -> ParseResult<Document<'a>> {
    Parser::with_options(arena, source, options()).parse()
}

/// Renders a note parsed from `source` to an HTML fragment, leaving out its frontmatter.
///
/// Local images are looked up in `vault`, starting from the note's directory `note_dir`
/// (relative to the vault root). Returns all errors if any image or math can't be rendered.
/// Links to notes or headings that can't be found are left as they are, with a warning added to
/// `warnings` for each.
pub fn to_html(
    document: &Document,
    source: &str,
    vault: &Vault,
    note_dir: &Path,
    warnings: &mut Vec<String>,
) -> Result<String, Vec<String>> {
    let mut definitions = HashMap::new();
    footnote_definitions(&document.children, &mut definitions);
    let config = MathCoreConfig {
        // Like MathJax in Obsidian, `\newcommand`s stay defined for the rest of the note.
        global_group: true,
        ..Default::default()
    };
    let mut ids = HeadingIdPlanner::new();
    let mut headings = Vec::new();
    collect_headings(&document.children, &mut ids, &mut headings);
    let mut renderer = Renderer {
        source,
        math: LatexToMathML::new(config, Vec::new()).expect("no custom macros"),
        heading_ids: (headings.iter())
            .map(|(start, heading)| (*start, heading.id.clone()))
            .collect(),
        headings: headings.into_iter().map(|(_, heading)| heading).collect(),
        ids,
        note_headings: HashMap::new(),
        footnote_labels: Vec::new(),
        vault,
        note_dir,
        errors: Vec::new(),
        warnings,
        link_depth: 0,
        in_label: false,
        at_boundary: true,
    };
    let mut out = String::new();
    renderer.blocks(&document.children, false, &mut out);
    renderer.footnotes(&definitions, &mut out);
    if renderer.errors.is_empty() {
        Ok(out)
    } else {
        Err(renderer.errors)
    }
}

/// Inline content with adjacent text merged, since the parser splits text at characters that
/// could have been markup.
enum Piece<'n, 'a> {
    Text(String),
    Node(&'n Node<'a>),
}

fn pieces<'n, 'a>(nodes: &'n [Node<'a>]) -> Vec<Piece<'n, 'a>> {
    let mut pieces = Vec::new();
    for node in nodes {
        match (node, pieces.last_mut()) {
            (Node::Text(text), Some(Piece::Text(last))) => last.push_str(text.value),
            (Node::Text(text), _) => pieces.push(Piece::Text(text.value.to_owned())),
            (node, _) => pieces.push(Piece::Node(node)),
        }
    }
    pieces
}

/// The inline children of a node, if it has any.
fn inline_children<'n, 'a>(node: &'n Node<'a>) -> Option<&'n [Node<'a>]> {
    Some(match node {
        Node::Emphasis(n) => &n.children,
        Node::Strong(n) => &n.children,
        Node::Delete(n) => &n.children,
        Node::Highlight(n) => &n.children,
        Node::Link(n) => &n.children,
        Node::Image(n) => &n.alt_content.as_ref()?.children,
        _ => return None,
    })
}

/// Collects the footnote definitions in `nodes` and the blocks in them by label.
fn footnote_definitions<'n, 'a>(
    nodes: &'n [Node<'a>],
    definitions: &mut HashMap<&'a str, &'n FootnoteDefinition<'a>>,
) {
    for node in nodes {
        match node {
            Node::FootnoteDefinition(definition) => {
                // Like links, the first definition of a label wins.
                definitions
                    .entry(definition.identifier)
                    .or_insert(definition);
                footnote_definitions(&definition.children, definitions);
            }
            Node::BlockQuote(quote) => footnote_definitions(&quote.children, definitions),
            Node::List(list) => {
                for item in &list.children {
                    footnote_definitions(&item.children, definitions);
                }
            }
            // Other blocks can't contain definitions.
            _ => {}
        }
    }
}

/// A heading that links can point to.
struct Heading {
    level: u8,
    /// What links to the heading are compared by: the slug of its plain text, with math as
    /// LaTeX, since that's what Obsidian puts in links.
    key: String,
    id: String,
}

/// Collects the headings in `nodes` in order, with their source offsets, giving each an id from
/// `ids`. Headings in footnotes are left out, since footnotes are rendered last, if at all.
fn collect_headings(nodes: &[Node], ids: &mut HeadingIdPlanner, out: &mut Vec<(u32, Heading)>) {
    for node in nodes {
        match node {
            Node::Heading(heading) => out.push((
                heading.span.start,
                Heading {
                    level: heading.depth,
                    key: slugify_heading(&plain_text(&pieces(&heading.children))),
                    id: ids.plan(&slugify_heading(&collect_heading_text(&heading.children))),
                },
            )),
            Node::BlockQuote(quote) => collect_headings(&quote.children, ids, out),
            Node::List(list) => {
                for item in &list.children {
                    collect_headings(&item.children, ids, out);
                }
            }
            _ => {}
        }
    }
}

/// Finds the id of the heading a link fragment like `Heading` or `Top#Heading` points to: the
/// first heading matching the last part whose enclosing headings match the other parts, in
/// order (but not necessarily directly).
fn find_heading<'h>(headings: &'h [Heading], path: &[String]) -> Option<&'h str> {
    let (last, parents) = path.split_last()?;
    let mut enclosing: Vec<&Heading> = Vec::new();
    for heading in headings {
        while enclosing.last().is_some_and(|h| h.level >= heading.level) {
            enclosing.pop();
        }
        if heading.key == *last {
            let mut enclosing_keys = enclosing.iter().map(|h| &h.key);
            if parents.iter().all(|p| enclosing_keys.any(|k| k == p)) {
                return Some(&heading.id);
            }
        }
        enclosing.push(heading);
    }
    None
}

/// The headings of the note at `path`.
fn note_headings(path: &Path) -> Result<Vec<Heading>, String> {
    let source =
        fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let arena = Allocator::for_source_len(source.len());
    let document =
        parse(&arena, &source).map_err(|err| format!("cannot parse {}: {err}", path.display()))?;
    let mut headings = Vec::new();
    collect_headings(
        &document.children,
        &mut HeadingIdPlanner::new(),
        &mut headings,
    );
    Ok(headings.into_iter().map(|(_, heading)| heading).collect())
}

struct Renderer<'s, 'v> {
    /// The parsed markdown, which node spans point into.
    source: &'s str,
    math: LatexToMathML,
    /// The ids of the note's headings outside footnotes, by source offset.
    heading_ids: HashMap<u32, String>,
    /// The note's headings outside footnotes, in order.
    headings: Vec<Heading>,
    /// The heading ids used so far.
    ids: HeadingIdPlanner,
    /// The headings of other notes that links point to, by path, or why they can't be read.
    note_headings: HashMap<PathBuf, Result<Vec<Heading>, String>>,
    /// Referenced footnote labels, in the order they are first referenced.
    footnote_labels: Vec<String>,
    vault: &'v Vault,
    note_dir: &'v Path,
    errors: Vec<String>,
    warnings: &'v mut Vec<String>,
    /// How many links we are in. Links can't contain tags, and images in them aren't figures.
    link_depth: usize,
    /// Whether we are in an image's label, where nested images are reduced to their labels.
    in_label: bool,
    /// Whether the next character starts a word, so a `#` there can start a tag.
    at_boundary: bool,
}

impl Renderer<'_, '_> {
    /// Renders blocks, giving a block id on its own line, like `^id` after a table, to the block
    /// before it. In a tight list item (`tight`), paragraphs aren't wrapped in `<p>`.
    fn blocks(&mut self, nodes: &[Node], tight: bool, out: &mut String) {
        let mut nodes = nodes.iter().peekable();
        while let Some(node) = nodes.next() {
            let mut html = String::new();
            match node {
                // Definitions are output where they are used.
                Node::Definition(_) | Node::FootnoteDefinition(_) => continue,
                Node::Paragraph(paragraph) if tight => {
                    html = self.paragraph(&pieces(&paragraph.children), false);
                }
                node => {
                    if tight && !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    self.block(node, &mut html);
                }
            }
            while let Some(id) = nodes.peek().and_then(|node| standalone_block_id(node)) {
                add_id(&mut html, &id);
                nodes.next();
            }
            out.push_str(&html);
        }
    }

    fn block(&mut self, node: &Node, out: &mut String) {
        match node {
            Node::Paragraph(paragraph) => {
                let mut pieces = pieces(&paragraph.children);
                let id = strip_block_id(&mut pieces);
                let mut html = self.paragraph(&pieces, true);
                if let Some(id) = id {
                    add_id(&mut html, &id);
                }
                out.push_str(&html);
            }
            Node::Heading(heading) => {
                let pieces = pieces(&heading.children);
                let id = self.heading_ids.get(&heading.span.start).cloned();
                let id = id.unwrap_or_else(|| {
                    (self.ids).plan(&slugify_heading(&collect_heading_text(&heading.children)))
                });
                let level = heading.depth;
                _ = write!(out, "<h{level} id=\"{}\">", attr(&id));
                self.inlines(&pieces, out);
                _ = writeln!(out, "</h{level}>");
            }
            Node::ThematicBreak(_) => out.push_str("<hr />\n"),
            Node::BlockQuote(quote) => self.block_quote(quote, out),
            Node::List(list) => self.list(list, out),
            Node::CodeBlock(code) => {
                out.push_str(&code_block(code.lang.unwrap_or(""), code.value));
            }
            Node::MathBlock(math) => {
                let source = &self.source[math.span.start as usize..math.span.end as usize];
                let mut html = String::new();
                if source.trim_end().contains('\n') {
                    html = self.math(math.value, MathDisplay::Block);
                } else {
                    // The parser reads a line like `$$a$$ $$b$$` as one block, `a$$ $$b`.
                    for (index, part) in math.value.split("$$").enumerate() {
                        if index % 2 == 0 {
                            html.push_str(&self.math(part, MathDisplay::Block));
                        } else {
                            html.push_str(&text(part));
                        }
                    }
                }
                _ = writeln!(out, "<div>{html}</div>");
            }
            Node::Html(html) => {
                out.push_str(html.value);
                if !html.value.ends_with('\n') {
                    out.push('\n');
                }
            }
            Node::Table(table) => self.table(table, out),
            // The parser options never produce other blocks.
            _ => {}
        }
    }

    /// Renders a paragraph, or the content of a tight list item if `wrap` is false.
    ///
    /// Figures are moved out of the paragraph, splitting it around them, since `<p>` can't
    /// contain `<figure>`. A paragraph of only `$$…$$` is rendered in a `<div>` instead.
    fn paragraph(&mut self, pieces: &[Piece], wrap: bool) -> String {
        let mut html = String::new();
        let mut text = String::new();
        self.at_boundary = true;
        if wrap && self.is_display_math_only(pieces) {
            self.inlines(pieces, &mut text);
            _ = writeln!(html, "<div>{}</div>", text.trim());
            return html;
        }
        let flush = |html: &mut String, text: &mut String| {
            let content = text.trim();
            if !content.is_empty() {
                if wrap {
                    _ = writeln!(html, "<p>{content}</p>");
                } else {
                    html.push_str(content);
                }
            }
            text.clear();
        };
        for piece in pieces {
            if let Piece::Node(Node::Image(image)) = piece
                && let Some(figure) = self.figure(image)
            {
                flush(&mut html, &mut text);
                html.push_str(&figure);
            } else {
                self.inlines(std::slice::from_ref(piece), &mut text);
            }
        }
        flush(&mut html, &mut text);
        html
    }

    fn is_display_math_only(&self, pieces: &[Piece]) -> bool {
        let is_display = |piece: &Piece| matches!(piece, Piece::Node(Node::InlineMath(math)) if self.is_display(math.span));
        pieces.iter().any(is_display)
            && pieces.iter().all(|piece| match piece {
                Piece::Text(text) => text.trim().is_empty(),
                Piece::Node(Node::Break(_)) => true,
                piece @ Piece::Node(_) => is_display(piece),
            })
    }

    /// Whether inline math is `$$…$$`, which is displayed as a block.
    fn is_display(&self, span: ferromark::ast::Span) -> bool {
        self.source[span.start as usize..].starts_with("$$")
    }

    /// Renders an Obsidian callout (a blockquote starting with `[!type]`) or a plain blockquote.
    fn block_quote(&mut self, quote: &BlockQuote, out: &mut String) {
        let mut title = match quote.children.first() {
            Some(Node::Paragraph(first)) => pieces(&first.children),
            _ => Vec::new(),
        };
        let callout = match title.first() {
            Some(Piece::Text(text)) => {
                let line = text.split('\n').next().unwrap();
                Callout::parse(line).map(|(callout, rest)| (callout, rest.len()))
            }
            _ => None,
        };
        let Some((callout, rest_len)) = callout else {
            out.push_str("<blockquote>\n");
            self.blocks(&quote.children, false, out);
            out.push_str("</blockquote>\n");
            return;
        };

        // The title is the rest of the first line, and the rest of the paragraph is the start of
        // the content.
        let Some(Piece::Text(text)) = title.first_mut() else {
            unreachable!()
        };
        let line_len = text.find('\n').unwrap_or(text.len());
        *text = text[line_len - rest_len..].to_owned();
        let mut content = Vec::new();
        let line_end = title.iter().position(|piece| match piece {
            Piece::Text(text) => text.contains('\n'),
            Piece::Node(node) => matches!(node, Node::Break(_)),
        });
        if let Some(index) = line_end {
            content = title.split_off(index + 1);
            // A line break is dropped.
            if let Some(Piece::Text(text)) = title.pop() {
                let (before, after) = text.split_once('\n').unwrap();
                title.push(Piece::Text(before.to_owned()));
                content.insert(0, Piece::Text(after.to_owned()));
            }
        }
        let is_blank = |pieces: &[Piece]| {
            (pieces.iter())
                .all(|piece| matches!(piece, Piece::Text(text) if text.trim().is_empty()))
        };
        if is_blank(&title) {
            title = vec![Piece::Text(callout.default_title())];
        }

        out.push_str(&callout.open());
        self.at_boundary = true;
        self.inlines(&title, out);
        out.push_str(&callout.end_title());
        if !is_blank(&content) {
            let id = strip_block_id(&mut content);
            let mut html = self.paragraph(&content, true);
            if let Some(id) = id {
                add_id(&mut html, &id);
            }
            out.push_str(&html);
        }
        self.blocks(&quote.children[1..], false, out);
        out.push_str(&callout.close());
    }

    fn list(&mut self, list: &List, out: &mut String) {
        let tight = !list.spread && list.children.iter().all(|item| !item.spread);
        let close = match (list.ordered, list.start) {
            (false, _) => {
                out.push_str("<ul>\n");
                "</ul>\n"
            }
            (true, None | Some(1)) => {
                out.push_str("<ol>\n");
                "</ol>\n"
            }
            (true, Some(start)) => {
                _ = writeln!(out, "<ol start=\"{start}\">");
                "</ol>\n"
            }
        };
        for item in &list.children {
            let mut html = String::from("<li>");
            if let Some(checked) = item.checked {
                let checked = if checked { " checked=\"\"" } else { "" };
                _ = writeln!(html, "<input disabled=\"\" type=\"checkbox\"{checked}/>");
            } else if !tight {
                html.push('\n');
            }
            let mut children = &item.children[..];
            let mut id = None;
            // In a tight item, a block id ending the first paragraph goes on the item.
            if tight && let Some(Node::Paragraph(paragraph)) = children.first() {
                let mut pieces = pieces(&paragraph.children);
                id = strip_block_id(&mut pieces);
                html.push_str(&self.paragraph(&pieces, false));
                children = &children[1..];
            }
            self.blocks(children, tight, &mut html);
            html.push_str("</li>\n");
            if let Some(id) = id {
                add_id(&mut html, &id);
            }
            out.push_str(&html);
        }
        out.push_str(close);
    }

    fn table(&mut self, table: &Table, out: &mut String) {
        out.push_str("<div>\n<table>");
        for (index, row) in table.children.iter().enumerate() {
            let cell_tag = if index == 0 {
                out.push_str("<thead>");
                "th"
            } else {
                "td"
            };
            out.push_str("<tr>");
            for (column, cell) in row.children.iter().enumerate() {
                let style = match table.align.get(column) {
                    Some(AlignKind::Left) => " style=\"text-align: left\"",
                    Some(AlignKind::Center) => " style=\"text-align: center\"",
                    Some(AlignKind::Right) => " style=\"text-align: right\"",
                    _ => "",
                };
                _ = write!(out, "<{cell_tag}{style}>");
                self.at_boundary = true;
                self.inlines(&pieces(&cell.children), out);
                _ = write!(out, "</{cell_tag}>");
            }
            out.push_str(if index == 0 {
                "</tr></thead><tbody>\n"
            } else {
                "</tr>\n"
            });
        }
        out.push_str("</tbody></table>\n</div>\n");
    }

    fn inlines(&mut self, pieces: &[Piece], out: &mut String) {
        for piece in pieces {
            match piece {
                Piece::Text(text) => self.text(text, out),
                Piece::Node(node) => self.inline(node, out),
            }
        }
    }

    fn inline(&mut self, node: &Node, out: &mut String) {
        let wrap = |this: &mut Self, tag: &str, children: &[Node], out: &mut String| {
            _ = write!(out, "<{tag}>");
            this.inlines(&pieces(children), out);
            _ = write!(out, "</{tag}>");
        };
        match node {
            Node::Text(text) => self.text(text.value, out),
            Node::Emphasis(n) => wrap(self, "em", &n.children, out),
            Node::Strong(n) => wrap(self, "strong", &n.children, out),
            Node::Delete(n) => wrap(self, "del", &n.children, out),
            Node::Highlight(n) => {
                self.at_boundary = true;
                wrap(self, "mark", &n.children, out);
                self.at_boundary = true;
            }
            Node::Break(_) => {
                out.push_str("<br />\n");
                self.at_boundary = true;
            }
            Node::Link(link) => {
                let url = self.link_url(link.url);
                _ = write!(out, "<a href=\"{}\"", href(&url));
                if let Some(title) = link.title.filter(|title| !title.is_empty()) {
                    _ = write!(out, " title=\"{}\"", attr(title));
                }
                out.push('>');
                self.link_depth += 1;
                self.inlines(&pieces(&link.children), out);
                self.link_depth -= 1;
                out.push_str("</a>");
                self.at_boundary = false;
            }
            node => {
                match node {
                    Node::InlineCode(code) => _ = write!(out, "<code>{}</code>", text(code.value)),
                    Node::InlineMath(math) => {
                        let display = if self.is_display(math.span) {
                            MathDisplay::Block
                        } else {
                            MathDisplay::Inline
                        };
                        out.push_str(&self.math(math.value, display));
                    }
                    Node::Html(html) => out.push_str(html.value),
                    Node::Image(image) if self.in_label => self.inlines(&label(image).0, out),
                    Node::Image(image) => out.push_str(&self.image(image)),
                    Node::FootnoteReference(reference) => {
                        out.push_str(&self.footnote_reference(reference.identifier));
                    }
                    // The parser options never produce other inline nodes.
                    _ => {}
                }
                self.at_boundary = false;
            }
        }
    }

    /// Rewrites a link into the vault to the URL of what it points to. Links to notes become
    /// absolute URLs from the vault root without `.md`, like `/notes/Other%20Note`. Block ids
    /// are rendered without their `^`. Obsidian's heading fragments, like `#Some%20heading`, or
    /// `#Top#Heading` for a heading under another, become the id of the heading they point to,
    /// looked up in the linked note. If the heading can't be found, the fragment is left as it
    /// is, with a warning. If the note can't be found, the link is left as it is, with a warning
    /// if it has a fragment.
    fn link_url(&mut self, url: &str) -> String {
        if is_remote(url) {
            return url.to_owned();
        }
        let (page, fragment) = url.split_once('#').unwrap_or((url, ""));
        let page_name = percent_decode_str(page).decode_utf8_lossy();
        let (page_url, note) = if page.is_empty() {
            (String::new(), None)
        } else if let Some(note) = self.vault.note_path(&page_name, self.note_dir) {
            (self.vault.note_url(&note), Some(note))
        } else {
            if !fragment.is_empty() {
                self.warnings
                    .push(format!("linked note not found in vault: {page_name}"));
            }
            return url.to_owned();
        };
        if fragment.is_empty() {
            return if page.is_empty() {
                url.to_owned()
            } else {
                page_url
            };
        }
        if let Some(id) = fragment.strip_prefix('^') {
            let id = if is_block_id(id) { id } else { fragment };
            return format!("{page_url}#{id}");
        }
        let path: Vec<_> = (fragment.split('#'))
            .map(|part| slugify_heading(&percent_decode_str(part).decode_utf8_lossy()))
            .collect();
        let headings = match note {
            None => &self.headings,
            Some(note) => {
                let headings =
                    (self.note_headings.entry(note)).or_insert_with_key(|note| note_headings(note));
                match headings {
                    Ok(headings) => headings,
                    Err(err) => {
                        self.warnings.push(err.clone());
                        return format!("{page_url}#{fragment}");
                    }
                }
            }
        };
        if let Some(id) = find_heading(headings, &path) {
            format!("{page_url}#{id}")
        } else {
            let heading = percent_decode_str(fragment).decode_utf8_lossy();
            self.warnings.push(if page.is_empty() {
                format!("linked heading not found: #{heading}")
            } else {
                format!("linked heading not found in {page_name}: #{heading}")
            });
            format!("{page_url}#{fragment}")
        }
    }

    /// Renders text, turning `#tags` outside links into tag links.
    fn text(&mut self, s: &str, out: &mut String) {
        let tags = self.link_depth == 0 && !self.in_label;
        let mut plain_start = 0;
        let mut i = 0;
        while let Some(c) = s[i..].chars().next() {
            if c == '#' && tags && self.at_boundary {
                let tag = tag_at(&s[i + 1..]);
                if !tag.is_empty() {
                    out.push_str(&text(&s[plain_start..i]));
                    out.push_str(&tag_html(tag));
                    i += 1 + tag.len();
                    plain_start = i;
                    self.at_boundary = false;
                    continue;
                }
            }
            self.at_boundary = c.is_whitespace();
            i += c.len_utf8();
        }
        out.push_str(&text(&s[plain_start..]));
    }

    /// Renders an image with alt text outside links as a figure, with the label's markup (and
    /// rendered math) as its caption. Returns `None` for other images.
    fn figure(&mut self, image: &Image) -> Option<String> {
        let (label, _) = label(image);
        if self.link_depth > 0 || plain_text(&label).trim().is_empty() {
            return None;
        }
        let img = self.image(image);
        let mut caption = String::new();
        self.in_label = true;
        self.inlines(&label, &mut caption);
        self.in_label = false;
        Some(format!(
            "<figure>\n{img}\n<figcaption>{}</figcaption>\n</figure>\n",
            caption.trim()
        ))
    }

    /// Renders an `<img>`, with the label's plain text (with math as LaTeX) as its alt text. Local
    /// images are embedded as `data:` URLs.
    fn image(&mut self, image: &Image) -> String {
        let (label, size) = label(image);
        let alt = plain_text(&label);
        let link = percent_decode_str(image.url).decode_utf8_lossy();
        let src = if is_remote(&link) {
            href(&link)
        } else {
            match self.vault.image_data_url(&link, self.note_dir) {
                Ok(data_url) => data_url,
                Err(err) => {
                    self.errors.push(err.to_string());
                    return String::new();
                }
            }
        };
        let title = match image.title.filter(|title| !title.is_empty()) {
            Some(title) => format!(" title=\"{}\"", attr(title)),
            None => String::new(),
        };
        format!(
            "<img src=\"{src}\" alt=\"{}\"{title}{size}>",
            attr(alt.trim())
        )
    }

    /// Converts LaTeX to a MathML Core `<math>` element.
    fn math(&mut self, latex: &str, display: MathDisplay) -> String {
        match self.math.convert_with_global_state(latex, display) {
            Ok(result) => result.mathml,
            Err(err) => {
                let err = format!("cannot render math `{}`: {err}", latex.trim());
                self.errors.push(err);
                String::new()
            }
        }
    }

    /// Outputs the referenced footnotes in a list, numbered in the order they are first
    /// referenced, each with a link back to its first reference. Definitions that are never
    /// referenced are dropped, as on GitHub.
    fn footnotes(&mut self, definitions: &HashMap<&str, &FootnoteDefinition>, out: &mut String) {
        if self.footnote_labels.is_empty() {
            return;
        }
        out.push_str("<section class=\"footnotes\">\n<hr />\n<ol>\n");
        // Outputting a footnote can reference more footnotes, which are added to the end.
        let mut index = 0;
        while let Some(label) = self.footnote_labels.get(index).cloned() {
            index += 1;
            _ = writeln!(out, "<li id=\"fn-{index}\">");
            let backref = format!(
                "<a href=\"#fnref-{index}\" class=\"footnote-backref\" \
                 aria-label=\"Back to reference {index}\">\u{21a9}\u{fe0e}</a>"
            );
            let mut body = String::new();
            let children = definitions.get(&*label).map_or(&[][..], |d| &d.children);
            self.blocks(children, false, &mut body);
            // The link back goes at the end of the last paragraph, or in a paragraph of its own.
            if matches!(children.last(), Some(Node::Paragraph(_))) && body.ends_with("</p>\n") {
                body.insert_str(body.len() - "</p>\n".len(), &format!(" {backref}"));
            } else {
                _ = writeln!(body, "<p>{backref}</p>");
            }
            out.push_str(&body);
            out.push_str("</li>\n");
        }
        out.push_str("</ol>\n</section>\n");
    }

    /// Renders a footnote reference as a link to its footnote, numbering it if it is the first
    /// reference to it.
    fn footnote_reference(&mut self, label: &str) -> String {
        let (n, id) = if let Some(index) = self.footnote_labels.iter().position(|l| l == label) {
            (index + 1, String::new())
        } else {
            self.footnote_labels.push(label.to_owned());
            let n = self.footnote_labels.len();
            (n, format!(" id=\"fnref-{n}\""))
        };
        format!("<sup class=\"footnote-ref\"><a href=\"#fn-{n}\"{id}>{n}</a></sup>")
    }
}

/// An image's label and size attributes.
///
/// The label is its alt text, which can end with a size, as in `alt|300` or `alt|300x200`.
fn label<'n, 'a>(image: &'n Image<'a>) -> (Vec<Piece<'n, 'a>>, String) {
    let mut label = (image.alt_content.as_ref()).map_or_else(Vec::new, |alt| pieces(&alt.children));
    let mut size = String::new();
    // The size follows the last `|`, which must be in the label's last text.
    if let Some(Piece::Text(last)) = label.last_mut()
        && let Some((alt, attrs)) = (last.rsplit_once('|'))
            .and_then(|(alt, size)| Some((alt.to_owned(), size_attrs(size)?)))
    {
        size = attrs;
        *last = alt;
    }
    (label, size)
}

/// The plain text of inline content, with math as LaTeX and images as their labels.
fn plain_text(pieces: &[Piece]) -> String {
    let mut text = String::new();
    for piece in pieces {
        match piece {
            // Line breaks within a paragraph are spaces.
            Piece::Text(s) => text.extend(s.chars().map(|c| if c == '\n' { ' ' } else { c })),
            Piece::Node(node) => match node {
                Node::InlineCode(code) => text.push_str(code.value),
                Node::InlineMath(math) => text.push_str(math.value),
                Node::Break(_) => text.push(' '),
                node => {
                    if let Some(children) = inline_children(node) {
                        text.push_str(&plain_text(&self::pieces(children)));
                    }
                }
            },
        }
    }
    text
}

/// The id of a paragraph that is just a block id, like `^id`.
fn standalone_block_id(node: &Node) -> Option<String> {
    let Node::Paragraph(paragraph) = node else {
        return None;
    };
    let [Piece::Text(text)] = &pieces(&paragraph.children)[..] else {
        return None;
    };
    let id = text.trim().strip_prefix('^')?;
    is_block_id(id).then(|| id.to_owned())
}

/// Removes a trailing ` ^block-id` from a paragraph or list item, returning `block-id`.
fn strip_block_id(pieces: &mut Vec<Piece>) -> Option<String> {
    let Some(Piece::Text(text)) = pieces.last() else {
        return None;
    };
    let trimmed = text.trim_end();
    let caret = trimmed.rfind('^')?;
    let id = &trimmed[caret + 1..];
    let before = &trimmed[..caret];
    // The id must be separated from the content before it, and there must be content.
    let separated = before.ends_with(char::is_whitespace)
        || (before.is_empty()
            && pieces.len() > 1
            && matches!(pieces[pieces.len() - 2], Piece::Node(Node::Break(_))));
    if !is_block_id(id) || !separated {
        return None;
    }
    let block_id = id.to_owned();
    let kept = before.trim_end().to_owned();
    pieces.pop();
    if kept.is_empty() {
        if matches!(pieces.last(), Some(Piece::Node(Node::Break(_)))) {
            pieces.pop();
        }
    } else {
        pieces.push(Piece::Text(kept));
    }
    Some(block_id)
}

fn is_block_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Adds an `id` to the first element in `html`, unless it already has one.
fn add_id(html: &mut String, id: &str) {
    let tag = &html[..html.find('>').unwrap_or(html.len())];
    if let Some(name) = tag.strip_prefix('<')
        && !tag.contains(" id=")
    {
        let name_end = 1 + name
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(name.len());
        html.insert_str(name_end, &format!(" id=\"{}\"", attr(id)));
    }
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

/// An Obsidian callout: a blockquote starting with `[!type]`.
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
            .map_or_default(|first| first.to_uppercase().chain(chars).collect())
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

/// Renders a code block.
fn code_block(info: &str, code: &str) -> String {
    let lang = info.split_whitespace().next().unwrap_or("");
    let class = if lang.is_empty() {
        String::new()
    } else {
        format!(" class=\"language-{}\"", attr(lang))
    };
    let code = highlight::highlight(lang, code).unwrap_or_else(|| text(code));
    format!("<pre><code{class}>{code}</code></pre>\n")
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

/// Parses an image size like `300` or `300x200` into `width` and `height` attributes.
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

    fn render(markdown: &str, vault: &Vault, note_dir: &Path) -> Result<String, Vec<String>> {
        let (html, warnings) = render_with_warnings(markdown, vault, note_dir);
        assert_eq!(warnings, Vec::<String>::new(), "{markdown:?}");
        html
    }

    fn render_with_warnings(
        markdown: &str,
        vault: &Vault,
        note_dir: &Path,
    ) -> (Result<String, Vec<String>>, Vec<String>) {
        let arena = Allocator::for_source_len(markdown.len());
        let document = parse(&arena, markdown).unwrap();
        let mut warnings = Vec::new();
        let html = super::to_html(&document, markdown, vault, note_dir, &mut warnings);
        (html, warnings)
    }

    /// Renders markdown that doesn't reference any vault files.
    fn to_html(markdown: &str) -> String {
        let dir = TempDir::new();
        let vault = Vault::open(dir.path()).unwrap();
        render(markdown, &vault, Path::new("")).unwrap()
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
        // A `$$` block on its own is wrapped in a `<div>` instead of a paragraph.
        assert_eq!(
            to_html("$$\nx\n$$\n"),
            "<div><math display=\"block\"><mi>x</mi></math></div>\n"
        );
        let html = to_html("$$a$$ $$b$$");
        assert!(html.starts_with("<div><math display=\"block\">"), "{html}");
        // With other text, it stays in the paragraph.
        let html = to_html("a $$x$$");
        assert!(html.starts_with("<p>a <math display=\"block\">"), "{html}");
    }

    #[test]
    fn display_math_blocks_contain_any_lines() {
        let minus = "<math display=\"block\"><mo lspace=\"0\" rspace=\"0\">−</mo><mi>x</mi></math>";
        // Lines that would start a list, heading or quote stay part of the math.
        assert_eq!(to_html("$$\n- x\n$$\n"), format!("<div>{minus}</div>\n"));
        let html = to_html("$$\n\\begin{matrix}\n- & a \\\\\n+ & b\n\\end{matrix}\n$$\n");
        assert!(
            html.starts_with("<div><math display=\"block\"><mtable"),
            "{html}"
        );
        assert!(!html.contains("<li>"), "{html}");
        // Also in callouts and list items.
        assert_eq!(
            to_html("> [!note]\n> $$\n> - x\n> $$\n"),
            format!(
                "<div class=\"callout\" data-callout=\"note\">\n\
                 <div class=\"callout-title\">Note</div>\n\
                 <div class=\"callout-content\">\n<div>{minus}</div>\n</div>\n</div>\n"
            )
        );
        assert_eq!(
            to_html("- a\n\n  $$\n  - x\n  $$\n"),
            format!("<ul>\n<li>\n<p>a</p>\n<div>{minus}</div>\n</li>\n</ul>\n")
        );
        // `$$` lines in code are left alone, as is a `$$` line without a closing one.
        assert_eq!(
            to_html("```\n$$\n- x\n$$\n```\n"),
            "<pre><code>$$\n- x\n$$\n</code></pre>\n"
        );
        assert_eq!(to_html("a\n\n$$\n"), "<p>a</p>\n<p>$$</p>\n");
        // `%` starts a LaTeX comment, and single `$`s keep their rules.
        let html = to_html("$$\n- 50\\%% x\n$$\n\n$5 and $10");
        assert!(html.contains("<mn>50</mn><mi>%</mi>"), "{html}");
        assert!(html.ends_with("<p>$5 and $10</p>\n"), "{html}");
    }

    #[test]
    fn gfm_extensions() {
        assert_eq!(
            to_html("| a |\n| - |\n| b |\n\n~~s~~ x[^1]\n\n- [ ] t\n\n[^1]: n"),
            "<div>\n<table><thead><tr><th>a</th></tr></thead><tbody>\n<tr><td>b</td></tr>\n</tbody></table>\n</div>\n\
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

    /// A vault with `img/cat.png` (containing `cat`) and `notes/dog.svg` (containing `dog`).
    fn vault() -> (TempDir, Vault) {
        let dir = TempDir::new();
        std::fs::create_dir_all(dir.path().join("img")).unwrap();
        std::fs::create_dir_all(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("img/cat.png"), "cat").unwrap();
        std::fs::write(dir.path().join("notes/dog.svg"), "dog").unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        (dir, vault)
    }

    fn render_in_vault(markdown: &str) -> Result<String, Vec<String>> {
        let (_dir, vault) = vault();
        render(markdown, &vault, Path::new("notes"))
    }

    const CAT: &str = "data:image/png;base64,Y2F0";
    const DOG: &str = "data:image/svg+xml,dog";

    #[test]
    fn images_without_alt_text() {
        let html = |md| render_in_vault(md).unwrap();
        assert_eq!(
            html("![](cat.png)"),
            format!("<p><img src=\"{CAT}\" alt=\"\"></p>\n")
        );
        assert_eq!(
            html("A ![|300x200](cat.png) B"),
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
            html("![A cat](cat.png)"),
            figure(&format!("<img src=\"{CAT}\" alt=\"A cat\">"), "A cat")
        );
        assert_eq!(
            html("![Tom & \"Jerry\"|300](cat.png)"),
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
            html("![A *cat*|3**0**0](cat.png)"),
            figure("A cat|300", "", "A <em>cat</em>|3<strong>0</strong>0")
        );
    }

    #[test]
    fn figures_split_paragraphs() {
        let html = render_in_vault("Before\n![Cat](cat.png)\nafter *it*.").unwrap();
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
        let html = render_in_vault("![A](cat.png) ![B](cat.png)").unwrap();
        assert!(html.contains("</figure>\n<figure>"), "{html}");
    }

    #[test]
    fn missing_images_are_errors() {
        let errors = render_in_vault("![](missing.png)\n\n![x](nope.jpg)\n\n![](cat.png)\n\n![]()")
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
    fn frontmatter_is_not_rendered() {
        assert_eq!(to_html("---\ntitle: Hi\n---\n# A"), "<h1 id=\"a\">A</h1>\n");
        // Without frontmatter, `---` lines are breaks or headings.
        assert_eq!(to_html("a\n---\n"), "<h2 id=\"a\">a</h2>\n");
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
        // Marks can't cross other inline elements.
        assert_eq!(to_html("==a *b== c*"), "<p><mark>a *b</mark> c*</p>\n");
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
        // Punctuation separates words, math is left out, and later headings never reuse an id.
        assert_eq!(
            to_html("# What's new?\n# The $x$ term\n# A\n# A\n# A-1"),
            "<h1 id=\"what-s-new\">What's new?</h1>\n\
             <h1 id=\"the-term\">The <math><mi>x</mi></math> term</h1>\n\
             <h1 id=\"a\">A</h1>\n<h1 id=\"a-1\">A</h1>\n<h1 id=\"a-1-1\">A-1</h1>\n"
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
        let (dir, vault) = vault();
        std::fs::write(dir.path().join("Note.md"), "").unwrap();
        assert_eq!(
            render(
                "[a](#^abc) [b](Note#^x-1) [c](#^no!) [d](^e) [e](Note#^no!)",
                &vault,
                Path::new("notes")
            )
            .unwrap(),
            "<p><a href=\"#abc\">a</a> <a href=\"/Note#x-1\">b</a> \
             <a href=\"#^no!\">c</a> <a href=\"^e\">d</a> <a href=\"/Note#^no!\">e</a></p>\n"
        );
    }

    #[test]
    fn heading_links() {
        assert_eq!(
            to_html(
                "# Sequences and series\n# What's new?\n\
                 [a](#Sequences%20and%20series) [b](#What's%20new?) [c](#sequences-and-series) \
                 [d](https://x.com/#Some%20Heading) [e](#) [f](Missing)"
            ),
            "<h1 id=\"sequences-and-series\">Sequences and series</h1>\n\
             <h1 id=\"what-s-new\">What's new?</h1>\n\
             <p><a href=\"#sequences-and-series\">a</a> <a href=\"#what-s-new\">b</a> \
             <a href=\"#sequences-and-series\">c</a> \
             <a href=\"https://x.com/#Some%20Heading\">d</a> <a href=\"#\">e</a> \
             <a href=\"Missing\">f</a></p>\n"
        );
    }

    #[test]
    fn nested_heading_links() {
        let links = |md: &str| {
            let html = to_html(md);
            html.split("href=\"#")
                .skip(1)
                .map(|rest| rest[..rest.find('"').unwrap()].to_owned())
                .collect::<Vec<_>>()
        };
        let note = "# Week 1\n## Exercises\n### Hard\n# Week 2\n## Exercises\n### Hard\n";
        assert_eq!(
            links(&format!(
                "{note}[a](#Exercises) [b](#Week%202#Exercises) [c](#Week%202#Hard) \
                 [d](#Week%202#Exercises#Hard) [e](#Week%201#Hard)"
            )),
            ["exercises", "exercises-1", "hard-1", "hard-1", "hard"]
        );
        // Ids that clash with an earlier heading's, and math as LaTeX.
        assert_eq!(
            links("# A\n# A\n# A-1\n# The $x$ term\n[a](#A-1) [b](#The%20$x$%20term)"),
            ["a-1-1", "the-term"]
        );
        // Headings in quotes and lists count, and footnote headings don't take their ids.
        let md = "> # Q\n\n- # L\n\n[a](#Q) [b](#L) [^1]\n\n[^1]: # Q\n\n# Q";
        assert_eq!(links(md)[..2], ["q", "l"]);
        let html = to_html(md);
        assert!(html.contains("<h1 id=\"q-1\">Q</h1>\n<section"), "{html}");
        assert!(html.contains("<h1 id=\"q-2\">Q</h1>"), "{html}");
    }

    #[test]
    fn links_to_other_notes_headings() {
        let (dir, vault) = vault();
        std::fs::write(
            dir.path().join("notes/Other Note.md"),
            "# Top\n## Same\n# Next\n## Same\n",
        )
        .unwrap();
        let html = render(
            "[a](Other%20Note#Next#Same) [b](Other%20Note.md#Same) \
             [c](../notes/Other%20Note#Next#Same)",
            &vault,
            Path::new("notes"),
        )
        .unwrap();
        assert_eq!(
            html,
            "<p><a href=\"/notes/Other%20Note#same-1\">a</a> \
             <a href=\"/notes/Other%20Note#same\">b</a> \
             <a href=\"/notes/Other%20Note#same-1\">c</a></p>\n"
        );
    }

    #[test]
    fn links_to_notes_are_absolute() {
        let (dir, vault) = vault();
        let write = |path: &str, content: &str| {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        };
        write("notes/b.md", "# Some Heading\n");
        write("Top #1?.md", "");
        write("notes/deep/Café.md", "");
        let html = render(
            "[a](b.md#Some%20Heading) [b](b) [c](/notes/b.md) [d](Top%20%231%3F) \
             [e](deep/Caf%C3%A9.md) [f](Caf%C3%A9) [g](dog.svg) [h](Missing) [i](mailto:x@y.z) \
             [j](b#)",
            &vault,
            Path::new("notes"),
        )
        .unwrap();
        assert_eq!(
            html,
            "<p><a href=\"/notes/b#some-heading\">a</a> <a href=\"/notes/b\">b</a> \
             <a href=\"/notes/b\">c</a> <a href=\"/Top%20%231%3F\">d</a> \
             <a href=\"/notes/deep/Caf%C3%A9\">e</a> <a href=\"/notes/deep/Caf%C3%A9\">f</a> \
             <a href=\"dog.svg\">g</a> <a href=\"Missing\">h</a> \
             <a href=\"mailto:x@y.z\">i</a> <a href=\"/notes/b\">j</a></p>\n"
        );
    }

    #[test]
    fn missing_link_targets_are_warnings() {
        let (dir, vault) = vault();
        std::fs::write(dir.path().join("notes/Other.md"), "# Top\n## Sub\n").unwrap();
        let (html, warnings) = render_with_warnings(
            "# Here\n[a](#Nowhere) [b](#Sub#Top) [c](Missing#Some%20Heading) \
             [d](Other#Top#Nope) [e](Other#Top#Sub)",
            &vault,
            Path::new("notes"),
        );
        // The links are left as they are.
        assert_eq!(
            html.unwrap(),
            "<h1 id=\"here\">Here</h1>\n<p><a href=\"#Nowhere\">a</a> <a href=\"#Sub#Top\">b</a> \
             <a href=\"Missing#Some%20Heading\">c</a> <a href=\"/notes/Other#Top#Nope\">d</a> \
             <a href=\"/notes/Other#sub\">e</a></p>\n"
        );
        assert_eq!(
            warnings,
            [
                "linked heading not found: #Nowhere",
                "linked heading not found: #Sub#Top",
                "linked note not found in vault: Missing",
                "linked heading not found in Other: #Top#Nope",
            ]
        );
    }

    #[test]
    fn standalone_block_ids_go_on_the_previous_block() {
        assert_eq!(
            to_html("$$\nx\n$$\n\n^math"),
            "<div id=\"math\"><math display=\"block\"><mi>x</mi></math></div>\n"
        );
        assert_eq!(
            to_html("| a |\n|--:|\n| 1 |\n\n^t"),
            "<div id=\"t\">\n<table><thead><tr><th style=\"text-align: right\">a</th></tr></thead>\
             <tbody>\n<tr><td style=\"text-align: right\">1</td></tr>\n</tbody></table>\n</div>\n"
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
}
