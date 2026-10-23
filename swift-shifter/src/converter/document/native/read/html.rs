//! HTML → [`Document`]. Handles real-world pages, not just clean XHTML: page
//! chrome is dropped, whitespace collapsed the way browsers do, and inline
//! content outside `<p>` is gathered into paragraphs.

use scraper::{ElementRef, Html, Node};

use super::markdown::push_text;
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, ListItem, Meta, RawFormat, Table, inline_text,
};

/// Read a complete HTML page: metadata from `<head>`, content from where
/// [`content_root`] finds it.
pub fn read(html: &str) -> Document {
    let page = Html::parse_document(html);
    let root = page.root_element();
    let meta = read_meta(root);
    let content = content_root(root);
    let in_content = matches!(content.value().name(), "main" | "article");
    let mut walker = Walker {
        in_content,
        ..Walker::default()
    };
    walker.children(content);
    Document {
        meta,
        blocks: walker.finish(),
    }
}

/// Read one page of an ebook: everything in `<body>` is content (an EPUB
/// chapter's `<header>` or `<aside>` isn't site chrome).
pub fn read_book_page(html: &str) -> Vec<Block> {
    let page = Html::parse_document(html);
    let root = page.root_element();
    let body = find(root, &["body"]).unwrap_or(root);
    let mut walker = Walker {
        in_content: true,
        ..Walker::default()
    };
    walker.children(body);
    walker.finish()
}

/// The visible text of an HTML snippet, whitespace collapsed: for writers
/// that can't keep markup the AST doesn't model.
pub fn text_of(html: &str) -> String {
    let fragment = Html::parse_fragment(html);
    collapse_ws(&fragment.root_element().text().collect::<String>())
        .trim_matches(is_html_space)
        .to_string()
}

/// The id of an HTML snippet that's nothing but an empty anchor,
/// `<a id="…"></a>`, which marks a place for links to point to.
pub fn empty_anchor_id(html: &str) -> Option<String> {
    let fragment = Html::parse_fragment(html);
    let blank = |n: &Node| n.as_text().is_some_and(|t| t.trim().is_empty());
    let mut nodes = fragment
        .root_element()
        .children()
        .filter(|n| !blank(n.value()));
    let anchor = ElementRef::wrap(nodes.next()?)?;
    let id = anchor.value().attr("id")?;
    (nodes.next().is_none()
        && anchor.value().name() == "a"
        && anchor.children().all(|n| blank(n.value()))
        && !id.is_empty())
    .then(|| id.to_string())
}

/// Read an HTML snippet (e.g. an HTML block inside Markdown) as blocks.
pub fn read_fragment(html: &str) -> Vec<Block> {
    let fragment = Html::parse_fragment(html);
    let mut walker = Walker {
        in_content: true,
        ..Walker::default()
    };
    walker.children(fragment.root_element());
    walker.finish()
}

fn read_meta(root: ElementRef) -> Meta {
    let mut meta = Meta {
        lang: root.value().attr("lang").map(str::to_string),
        ..Meta::default()
    };
    for el in descendants(root) {
        match el.value().name() {
            "title" if meta.title.is_none() => {
                let title = collapse_ws(&el.text().collect::<String>())
                    .trim_matches(is_html_space)
                    .to_string();
                meta.title = (!title.is_empty()).then_some(title);
            }
            "meta" => {
                let name = el.value().attr("name").unwrap_or("").to_ascii_lowercase();
                let content = el.value().attr("content").map(|c| c.trim().to_string());
                match (name.as_str(), content) {
                    ("author", Some(c)) if !c.is_empty() => meta.authors.push(c),
                    ("description", Some(c)) if !c.is_empty() => meta.description = Some(c),
                    _ => {}
                }
            }
            "body" => break,
            _ => {}
        }
    }
    meta
}

/// Footnote roles an element plays, from EPUB's `epub:type`, ARIA's
/// `role="doc-…"` and the classes pandoc and our own writer use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteRole {
    /// A reference to a note (`<a epub:type="noteref" href="#n1">`).
    Ref,
    /// A note's body.
    Note,
    /// A section holding note bodies, usually as `<li id=…>`.
    Section,
    /// A link from a note back to its reference: dropped.
    Backlink,
}

fn note_role(el: ElementRef) -> Option<NoteRole> {
    let value = el.value();
    let epub_types = value.attr("epub:type").unwrap_or("").split_whitespace();
    let role = value.attr("role").unwrap_or("");
    let names = epub_types
        .chain(role.strip_prefix("doc-"))
        .chain(value.classes());
    for name in names {
        let found = match name {
            "noteref" | "footnote-ref" => NoteRole::Ref,
            "footnote" | "endnote" | "rearnote" => NoteRole::Note,
            "footnotes" | "endnotes" | "rearnotes" => NoteRole::Section,
            "backlink" | "footnote-back" => NoteRole::Backlink,
            _ => continue,
        };
        return Some(found);
    }
    None
}

/// The note label a reference's `href` points at: the id for `#n1`, the
/// whole target for a note in another file (`notes.xhtml#n1`).
fn note_label(href: &str) -> Option<String> {
    let href = href.trim();
    match href.split_once('#') {
        Some(("", id)) if !id.is_empty() => Some(id.to_string()),
        Some((_, id)) if !id.is_empty() => Some(href.to_string()),
        _ => None,
    }
}

fn descendants(root: ElementRef) -> impl Iterator<Item = ElementRef> {
    root.descendants().filter_map(ElementRef::wrap)
}

fn find<'a>(root: ElementRef<'a>, names: &[&str]) -> Option<ElementRef<'a>> {
    descendants(root).find(|el| names.contains(&el.value().name()))
}

/// Where the page's content is: `<main>`, else the page's only `<article>`
/// (several mean a listing, where everything matters), else `<body>`.
fn content_root(root: ElementRef) -> ElementRef {
    if let Some(main) = find(root, &["main"]) {
        return main;
    }
    let mut articles = descendants(root).filter(|el| el.value().name() == "article");
    if let (Some(article), None) = (articles.next(), articles.next()) {
        return article;
    }
    find(root, &["body"]).unwrap_or(root)
}

/// Never content, wherever they appear.
const SKIPPED: &[&str] = &[
    "head", "script", "style", "noscript", "template", "nav", "iframe", "button", "input",
    "select", "textarea", "svg", "canvas", "object", "embed",
];

/// Site chrome when they wrap the page, but real content (a title, notes)
/// inside `<main>` or `<article>`.
const CHROME: &[&str] = &["header", "footer", "aside"];

const BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "center",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "html",
    "li",
    "main",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
];

/// Turns a DOM subtree into blocks. Inline content is buffered in `inline`
/// until a block element (or the end) flushes it as a paragraph.
#[derive(Default)]
struct Walker {
    blocks: Vec<Block>,
    inline: Vec<Inline>,
    /// Inside `<main>`/`<article>`, where header/footer/aside are content.
    in_content: bool,
}

impl Walker {
    fn finish(mut self) -> Vec<Block> {
        self.flush();
        self.blocks
    }

    fn flush(&mut self) {
        let content = tidy(std::mem::take(&mut self.inline));
        if !content.is_empty() {
            self.blocks.push(Block::Paragraph(content));
        }
    }

    fn push_block(&mut self, block: Block) {
        self.flush();
        self.blocks.push(block);
    }

    fn children(&mut self, el: ElementRef) {
        for child in el.children() {
            match child.value() {
                Node::Text(t) => push_text(&mut self.inline, &collapse_ws(t)),
                Node::Element(_) => {
                    if let Some(child_el) = ElementRef::wrap(child) {
                        self.element(child_el);
                    }
                }
                _ => {}
            }
        }
    }

    /// Blocks of a nested container, read with this walker's context.
    fn sub_blocks(&self, el: ElementRef) -> Vec<Block> {
        let in_content = self.in_content || matches!(el.value().name(), "main" | "article");
        let mut walker = Walker {
            in_content,
            ..Walker::default()
        };
        walker.children(el);
        walker.finish()
    }

    fn element(&mut self, el: ElementRef) {
        let name = el.value().name();
        // Notes before chrome: EPUB notes are `<aside>`s.
        match note_role(el) {
            Some(NoteRole::Note) if el.value().id().is_some() && name != "a" => {
                let label = el.value().id().unwrap_or_default().to_string();
                let content = self.sub_blocks(el);
                self.push_block(Block::FootnoteDefinition { label, content });
                return;
            }
            Some(NoteRole::Section) => {
                self.notes_section(el);
                return;
            }
            _ => {}
        }
        if SKIPPED.contains(&name) || (CHROME.contains(&name) && !self.in_content) {
            return;
        }
        // An ebook's title and cover pages repeat its metadata, which
        // writers take from `Meta` anyway.
        let epub_type = el.value().attr("epub:type").unwrap_or("");
        if epub_type
            .split_whitespace()
            .any(|t| matches!(t, "titlepage" | "cover"))
        {
            return;
        }
        if name == "math" && el.value().attr("display") == Some("block") {
            let block = match math(el) {
                Inline::Math(tex) => Block::Math(tex),
                _ => Block::Raw {
                    format: RawFormat::Html,
                    text: el.html(),
                },
            };
            self.push_block(block);
            return;
        }
        if !BLOCK_ELEMENTS.contains(&name) {
            push_inline_element(&mut self.inline, el);
            return;
        }
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let content = tidy(inline_children(el));
                if !content.is_empty() {
                    let level = name[1..].parse().unwrap_or(1);
                    self.push_block(Block::Heading { level, content });
                }
            }
            "p" => {
                self.flush();
                let content = tidy(inline_children(el));
                // `<p><math display="block">…</math></p>` (pandoc) is
                // display math, not an inline formula.
                let block_math = el.children().filter_map(ElementRef::wrap).all(|c| {
                    c.value().name() == "math" && c.value().attr("display") == Some("block")
                });
                match content.as_slice() {
                    [Inline::Math(tex)] if block_math => self.blocks.push(Block::Math(tex.clone())),
                    [] => {}
                    _ => self.blocks.push(Block::Paragraph(content)),
                }
            }
            "blockquote" => {
                let blocks = self.sub_blocks(el);
                self.push_block(Block::Quote(blocks));
            }
            "ul" | "ol" => {
                let list = self.list(el);
                self.push_block(Block::List(list));
            }
            "pre" => self.push_block(code_block(el)),
            "hr" => self.push_block(Block::ThematicBreak),
            "table" => {
                let table = table(el);
                if !table.header.is_empty() || !table.rows.is_empty() {
                    self.push_block(Block::Table(table));
                }
            }
            "dt" => {
                let content = tidy(inline_children(el));
                if !content.is_empty() {
                    self.push_block(Block::Paragraph(vec![Inline::Strong(content)]));
                }
            }
            "dd" => {
                let blocks = self.sub_blocks(el);
                self.push_block(Block::Quote(blocks));
            }
            "figcaption" => {
                let content = tidy(inline_children(el));
                if !content.is_empty() {
                    self.push_block(Block::Paragraph(vec![Inline::Emph(content)]));
                }
            }
            // div, section, figure, li outside a list, …: their content.
            _ => {
                self.flush();
                let blocks = self.sub_blocks(el);
                self.blocks.extend(blocks);
            }
        }
    }

    /// A notes section: each `<li id=…>` is a note's body (pandoc and our
    /// own HTML), and anything else, such as note `<aside>`s, is read as
    /// usual. Rules and headings in the section are left out.
    fn notes_section(&mut self, el: ElementRef) {
        let items: Vec<ElementRef> = descendants(el)
            .filter(|li| li.value().name() == "li" && li.value().id().is_some())
            .collect();
        if items.is_empty() {
            let blocks = self.sub_blocks(el);
            self.flush();
            self.blocks.extend(
                blocks
                    .into_iter()
                    .filter(|b| matches!(b, Block::FootnoteDefinition { .. })),
            );
            return;
        }
        for li in items {
            let label = li.value().id().unwrap_or_default().to_string();
            let content = self.sub_blocks(li);
            self.push_block(Block::FootnoteDefinition { label, content });
        }
    }

    fn list(&self, el: ElementRef) -> List {
        let start = (el.value().name() == "ol").then(|| {
            el.value()
                .attr("start")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(1)
        });
        let items: Vec<ListItem> = el
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|c| c.value().name() == "li")
            .map(|li| {
                // The item's own checkbox, possibly in a <label> (pandoc),
                // never one of a nested list's.
                let task = descendants(li)
                    .find(|d| {
                        d.value().name() == "input"
                            && d.ancestors()
                                .filter_map(ElementRef::wrap)
                                .take_while(|a| *a != li)
                                .all(|a| !matches!(a.value().name(), "ul" | "ol"))
                    })
                    .filter(|input| input.value().attr("type") == Some("checkbox"))
                    .map(|input| input.value().attr("checked").is_some());
                ListItem {
                    task,
                    content: self.sub_blocks(li),
                }
            })
            .collect();
        // Loose when an item has several paragraphs; a nested list alone
        // doesn't make it loose.
        let tight = items.iter().all(|i| {
            i.content
                .iter()
                .filter(|b| matches!(b, Block::Paragraph(_)))
                .count()
                <= 1
        });
        List {
            start,
            tight,
            items,
        }
    }
}

fn inline_children(el: ElementRef) -> Vec<Inline> {
    let mut out = Vec::new();
    push_inline_children(&mut out, el);
    out
}

fn push_inline_children(out: &mut Vec<Inline>, el: ElementRef) {
    for child in el.children() {
        match child.value() {
            Node::Text(t) => push_text(out, &collapse_ws(t)),
            Node::Element(_) => {
                if let Some(child_el) = ElementRef::wrap(child) {
                    push_inline_element(out, child_el);
                }
            }
            _ => {}
        }
    }
}

fn push_inline_element(out: &mut Vec<Inline>, el: ElementRef) {
    let name = el.value().name();
    if SKIPPED.contains(&name) {
        return;
    }
    let inline = match name {
        "br" => Inline::LineBreak,
        "em" | "i" | "cite" | "dfn" | "var" => Inline::Emph(inline_children(el)),
        "strong" | "b" => Inline::Strong(inline_children(el)),
        "s" | "del" | "strike" => Inline::Strikethrough(inline_children(el)),
        "sup" => {
            let content = inline_children(el);
            // `<sup class="footnote-ref"><a href="#fn1">1</a></sup>`: the
            // reference itself, not superscript.
            let is_ref = |i: &Inline| match i {
                Inline::FootnoteRef(_) => true,
                Inline::Text(t) => t.trim().is_empty(),
                _ => false,
            };
            if content.iter().any(|i| matches!(i, Inline::FootnoteRef(_)))
                && content.iter().all(is_ref)
            {
                out.extend(
                    content
                        .into_iter()
                        .filter(|i| matches!(i, Inline::FootnoteRef(_))),
                );
                return;
            }
            Inline::Superscript(content)
        }
        "sub" => Inline::Subscript(inline_children(el)),
        "code" | "kbd" | "samp" | "tt" => Inline::Code(el.text().collect()),
        "a" => {
            let parent_role = el.parent().and_then(ElementRef::wrap).and_then(note_role);
            let role = note_role(el).or(parent_role.filter(|r| *r == NoteRole::Ref));
            match role {
                Some(NoteRole::Backlink) => return,
                Some(NoteRole::Ref) => {
                    if let Some(label) = el.value().attr("href").and_then(note_label) {
                        out.push(Inline::FootnoteRef(label));
                        return;
                    }
                }
                _ => {}
            }
            let content = inline_children(el);
            match el.value().attr("href") {
                Some(href) if !href.starts_with("javascript:") => Inline::Link {
                    url: href.trim().to_string(),
                    title: el.value().attr("title").map(str::to_string),
                    content,
                },
                // An anchor without a target is just its text.
                _ => {
                    out.extend(content);
                    return;
                }
            }
        }
        "img" => {
            let Some(src) = el.value().attr("src") else {
                return;
            };
            Inline::Image {
                url: src.trim().to_string(),
                title: el.value().attr("title").map(str::to_string),
                alt: el.value().attr("alt").unwrap_or("").to_string(),
            }
        }
        "math" => math(el),
        "q" => {
            push_text(out, "“");
            push_inline_children(out, el);
            push_text(out, "”");
            return;
        }
        // Block elements met inside inline content (e.g. <p> in a table
        // cell) are separated by line breaks.
        _ if BLOCK_ELEMENTS.contains(&name) => {
            if !out.is_empty() {
                out.push(Inline::LineBreak);
            }
            push_inline_children(out, el);
            return;
        }
        // span, u, mark, small, abbr, label, font, …: their content.
        _ => {
            push_inline_children(out, el);
            return;
        }
    };
    out.push(inline);
}

/// MathML: the TeX from its annotation when present (round-trips into our
/// `$…$`), otherwise the MathML itself as raw HTML.
fn math(el: ElementRef) -> Inline {
    let tex = el.value().attr("alttext").map(str::to_string).or_else(|| {
        descendants(el)
            .find(|a| {
                a.value().name() == "annotation"
                    && a.value()
                        .attr("encoding")
                        .is_some_and(|e| e.eq_ignore_ascii_case("application/x-tex"))
            })
            .map(|a| a.text().collect::<String>().trim().to_string())
    });
    match tex {
        Some(tex) if !tex.is_empty() => Inline::Math(tex),
        _ => Inline::Raw {
            format: RawFormat::Html,
            text: el.html(),
        },
    }
}

fn code_block(pre: ElementRef) -> Block {
    let code = descendants(pre).find(|c| c.value().name() == "code");
    let classes: Vec<&str> = [Some(pre), code]
        .into_iter()
        .flatten()
        .flat_map(|el| el.value().classes())
        .collect();
    // `language-rust` (Markdown renderers), or pandoc's `sourceCode rust`.
    let lang = classes
        .iter()
        .find_map(|class| {
            class
                .strip_prefix("language-")
                .or_else(|| class.strip_prefix("lang-"))
        })
        .or_else(|| {
            classes
                .contains(&"sourceCode")
                .then(|| classes.iter().find(|c| **c != "sourceCode").copied())
                .flatten()
        })
        .map(str::to_string);
    let text: String = pre.text().collect();
    Block::Code {
        lang,
        code: text.trim_matches('\n').to_string(),
    }
}

fn table(el: ElementRef) -> Table {
    let mut header: Vec<Vec<Inline>> = Vec::new();
    let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    // Column alignment, from the first row's cells.
    let mut align: Vec<Align> = Vec::new();
    for tr in descendants(el).filter(|e| e.value().name() == "tr") {
        // Skip rows of nested tables; they're read as part of their cell.
        if tr
            .ancestors()
            .filter_map(ElementRef::wrap)
            .find(|a| a.value().name() == "table")
            .is_some_and(|t| t.id() != el.id())
        {
            continue;
        }
        let cells: Vec<ElementRef> = tr
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|c| matches!(c.value().name(), "td" | "th"))
            .collect();
        let in_thead = tr
            .parent()
            .and_then(ElementRef::wrap)
            .is_some_and(|p| p.value().name() == "thead");
        let all_th = !cells.is_empty() && cells.iter().all(|c| c.value().name() == "th");
        let first_row = header.is_empty() && rows.is_empty();
        let mut row = Vec::new();
        for cell in &cells {
            if first_row {
                align.push(cell_align(*cell));
            }
            row.push(tidy(inline_children(*cell)));
            // A cell spanning columns is followed by empty cells, keeping
            // the grid rectangular.
            let span: usize = cell
                .value()
                .attr("colspan")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            row.extend(std::iter::repeat_n(Vec::new(), span.clamp(1, 100) - 1));
            if first_row {
                align.extend(std::iter::repeat_n(Align::Default, span.clamp(1, 100) - 1));
            }
        }
        if header.is_empty() && rows.is_empty() && (in_thead || all_th) {
            header = row;
        } else {
            rows.push(row);
        }
    }
    let width = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    for row in std::iter::once(&mut header).chain(rows.iter_mut()) {
        if !row.is_empty() {
            row.resize(width, Vec::new());
        }
    }
    align.resize(width, Align::Default);
    Table {
        align,
        header,
        rows,
    }
}

/// A cell's alignment from `style="text-align: …"` or the old `align`.
fn cell_align(cell: ElementRef) -> Align {
    let style = cell
        .value()
        .attr("style")
        .unwrap_or("")
        .to_ascii_lowercase();
    let from_style = style.split(';').find_map(|decl| {
        let (property, value) = decl.split_once(':')?;
        (property.trim() == "text-align").then(|| value.trim().to_string())
    });
    let value = from_style.or_else(|| cell.value().attr("align").map(str::to_ascii_lowercase));
    match value.as_deref() {
        Some("left") => Align::Left,
        Some("center") => Align::Center,
        Some("right") => Align::Right,
        _ => Align::Default,
    }
}

/// HTML's whitespace (ASCII only): `&nbsp;` and the ideographic space are
/// content, not layout.
fn is_html_space(c: char) -> bool {
    c.is_ascii_whitespace()
}

/// Collapse whitespace runs to one space, as HTML rendering does.
fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if is_html_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// Trim spaces at the edges of a run of inlines and around line breaks, and
/// drop runs that hold only whitespace.
fn tidy(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut inlines = hoist_spaces(inlines);
    // Spaces next to a line break are invisible.
    for i in 0..inlines.len() {
        let before_break = matches!(inlines.get(i + 1), Some(Inline::LineBreak));
        let after_break = i > 0 && matches!(inlines.get(i - 1), Some(Inline::LineBreak));
        if let Inline::Text(t) = &mut inlines[i] {
            if before_break {
                *t = t.trim_end_matches(is_html_space).to_string();
            }
            if after_break {
                *t = t.trim_start_matches(is_html_space).to_string();
            }
        }
    }
    inlines.retain(|i| !matches!(i, Inline::Text(t) if t.is_empty()));
    trim_edge(&mut inlines, Edge::Start);
    trim_edge(&mut inlines, Edge::End);
    while matches!(inlines.last(), Some(Inline::LineBreak)) {
        inlines.pop();
    }
    while matches!(inlines.first(), Some(Inline::LineBreak)) {
        inlines.remove(0);
    }
    if inline_text(&inlines).trim_matches(is_html_space).is_empty()
        && !inlines.iter().any(|i| {
            matches!(
                i,
                Inline::Image { .. } | Inline::Math(_) | Inline::Raw { .. }
            )
        })
    {
        return Vec::new();
    }
    inlines
}

/// Rebuilds a styled inline around new content.
type Rebuild = Box<dyn FnOnce(Vec<Inline>) -> Inline>;

/// Move whitespace at the edges of styled runs outside them (`<b> x </b>`
/// becomes ` **x** `), since Markdown emphasis can't start or end with a
/// space, and collapse the doubled spaces that leaves.
fn hoist_spaces(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::new();
    for inline in inlines {
        let (content, rebuild): (Vec<Inline>, Rebuild) = match inline {
            Inline::Emph(c) => (c, Box::new(Inline::Emph)),
            Inline::Strong(c) => (c, Box::new(Inline::Strong)),
            Inline::Strikethrough(c) => (c, Box::new(Inline::Strikethrough)),
            Inline::Superscript(c) => (c, Box::new(Inline::Superscript)),
            Inline::Subscript(c) => (c, Box::new(Inline::Subscript)),
            Inline::Link {
                url,
                title,
                content,
            } => (
                content,
                Box::new(move |content| Inline::Link {
                    url,
                    title,
                    content,
                }),
            ),
            Inline::Text(t) => {
                push_collapsed(&mut out, &t);
                continue;
            }
            other => {
                out.push(other);
                continue;
            }
        };
        let mut content = hoist_spaces(content);
        let leading = starts_with_space(&content);
        let trailing = ends_with_space(&content);
        trim_edge(&mut content, Edge::Start);
        trim_edge(&mut content, Edge::End);
        if leading {
            push_collapsed(&mut out, " ");
        }
        if !content.is_empty() {
            out.push(rebuild(content));
        }
        if trailing {
            push_collapsed(&mut out, " ");
        }
    }
    out
}

fn starts_with_space(inlines: &[Inline]) -> bool {
    matches!(inlines.first(), Some(Inline::Text(t)) if t.starts_with(' '))
}

fn ends_with_space(inlines: &[Inline]) -> bool {
    matches!(inlines.last(), Some(Inline::Text(t)) if t.ends_with(' '))
}

/// Append text, dropping a leading space when the previous text already
/// ends with one.
fn push_collapsed(out: &mut Vec<Inline>, text: &str) {
    let prev_space = matches!(out.last(), Some(Inline::Text(t)) if t.ends_with(' '));
    let text = if prev_space {
        text.trim_start_matches(' ')
    } else {
        text
    };
    push_text(out, text);
}

#[derive(Clone, Copy)]
enum Edge {
    Start,
    End,
}

/// Trim HTML whitespace at one edge of a run, descending into the first or
/// last styled run.
fn trim_edge(inlines: &mut Vec<Inline>, edge: Edge) {
    let idx = match edge {
        Edge::Start => 0,
        Edge::End => inlines.len().wrapping_sub(1),
    };
    let Some(inline) = inlines.get_mut(idx) else {
        return;
    };
    match inline {
        Inline::Text(t) => {
            *t = match edge {
                Edge::Start => t.trim_start_matches(is_html_space),
                Edge::End => t.trim_end_matches(is_html_space),
            }
            .to_string();
            if t.is_empty() {
                inlines.remove(idx);
                trim_edge(inlines, edge);
            }
        }
        Inline::Emph(c)
        | Inline::Strong(c)
        | Inline::Strikethrough(c)
        | Inline::Superscript(c)
        | Inline::Subscript(c)
        | Inline::Link { content: c, .. } => trim_edge(c, edge),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    #[test]
    fn reads_pandoc_and_epub_footnotes() {
        // pandoc's HTML: references in <sup>, notes as <li> in an endnotes
        // section, back-links dropped.
        let doc = read(
            "<body><p>A<sup><a href=\"#fn1\" class=\"footnote-ref\" role=\"doc-noteref\">1</a></sup>.</p>\
             <section role=\"doc-endnotes\"><hr><ol><li id=\"fn1\"><p>Note one. \
             <a href=\"#fnref1\" role=\"doc-backlink\">↩</a></p></li></ol></section></body>",
        );
        assert_eq!(
            doc.blocks,
            vec![
                Block::Paragraph(vec![
                    text("A"),
                    Inline::FootnoteRef("fn1".into()),
                    text(".")
                ]),
                Block::FootnoteDefinition {
                    label: "fn1".into(),
                    content: vec![Block::Paragraph(vec![text("Note one.")])],
                },
            ]
        );
        // EPUB: an <aside> note (page chrome elsewhere) and a reference to
        // a note in another file.
        let blocks = read_book_page(
            "<body><p>B<a epub:type=\"noteref\" href=\"notes.xhtml#n2\">2</a></p>\
             <aside epub:type=\"footnote\" id=\"n1\"><p>Aside note.</p></aside></body>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec![
                    text("B"),
                    Inline::FootnoteRef("notes.xhtml#n2".into())
                ]),
                Block::FootnoteDefinition {
                    label: "n1".into(),
                    content: vec![Block::Paragraph(vec![text("Aside note.")])],
                },
            ]
        );
    }

    #[test]
    fn book_pages_keep_headers_but_skip_title_pages() {
        let blocks = read_book_page(
            "<body><section epub:type=\"titlepage\"><h1>Book</h1><p>Author</p></section>\
             <header><h1>Chapter</h1></header><p>Text</p></body>",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 1,
                    content: vec![text("Chapter")]
                },
                Block::Paragraph(vec![text("Text")]),
            ]
        );
    }

    #[test]
    fn reads_pandoc_tasks_code_languages_and_alignment() {
        let doc = read(
            "<body><ul><li><label><input type=\"checkbox\" checked=\"\">Done</label>\
             <ul><li><input type=\"checkbox\">Nested</li></ul></li><li>Plain</li></ul>\
             <pre class=\"sourceCode rust\"><code class=\"sourceCode rust\">fn x() {}</code></pre>\
             <table><tr><th style=\"text-align: center;\">a</th><th align=\"right\">b</th><th>c</th></tr>\
             <tr><td>1</td><td>2</td><td>3</td></tr></table></body>",
        );
        let Block::List(list) = &doc.blocks[0] else {
            panic!("{doc:?}");
        };
        assert_eq!(list.items[0].task, Some(true));
        assert_eq!(
            list.items[1].task, None,
            "a nested checkbox isn't the item's"
        );
        assert!(matches!(&doc.blocks[1], Block::Code { lang: Some(l), .. } if l == "rust"));
        let Block::Table(table) = &doc.blocks[2] else {
            panic!("{doc:?}");
        };
        assert_eq!(table.align, [Align::Center, Align::Right, Align::Default]);
    }

    #[test]
    fn reads_metadata_and_prefers_main_content() {
        let doc = read(
            r#"<!doctype html><html lang="fr"><head><title> Le  Titre </title>
            <meta name="author" content="Ann"></head>
            <body><nav>Menu</nav><header>Site</header>
            <main><h1>Hello</h1><p>World</p></main><footer>©</footer></body></html>"#,
        );
        assert_eq!(doc.meta.title.as_deref(), Some("Le Titre"));
        assert_eq!(doc.meta.lang.as_deref(), Some("fr"));
        assert_eq!(doc.meta.authors, ["Ann"]);
        assert_eq!(
            doc.blocks,
            vec![
                Block::Heading {
                    level: 1,
                    content: vec![text("Hello")]
                },
                Block::Paragraph(vec![text("World")]),
            ]
        );
    }

    #[test]
    fn drops_scripts_and_page_chrome_without_main() {
        let doc = read(
            "<body><header>Logo</header><script>x()</script><style>p{}</style>\
             <div><p>Kept</p></div><footer>Bye</footer></body>",
        );
        assert_eq!(doc.blocks, vec![Block::Paragraph(vec![text("Kept")])]);
    }

    #[test]
    fn collapses_whitespace_and_gathers_loose_inline_content() {
        let blocks = read_fragment("<div>  Loose \n text <b> bold </b>\n<p>Para</p> tail </div>");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec![
                    text("Loose text "),
                    Inline::Strong(vec![text("bold")])
                ]),
                Block::Paragraph(vec![text("Para")]),
                Block::Paragraph(vec![text("tail")]),
            ]
        );
    }

    #[test]
    fn reads_lists_tasks_and_code() {
        let blocks = read_fragment(
            r#"<ol start="4"><li>a</li><li><input type="checkbox" checked> b</li></ol>
            <pre><code class="language-py">print(1)
</code></pre>"#,
        );
        let [Block::List(list), Block::Code { lang, code }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(list.start, Some(4));
        assert_eq!(list.items[1].task, Some(true));
        assert_eq!(lang.as_deref(), Some("py"));
        assert_eq!(code, "print(1)");
    }

    #[test]
    fn reads_tables_with_header_and_colspan() {
        let blocks = read_fragment(
            "<table><thead><tr><th>A</th><th>B</th><th>C</th></tr></thead>\
             <tbody><tr><td colspan=2>wide</td><td>c</td></tr><tr><td>1</td></tr></tbody></table>",
        );
        let [Block::Table(t)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(
            t.header,
            vec![vec![text("A")], vec![text("B")], vec![text("C")]]
        );
        assert_eq!(t.rows[0], vec![vec![text("wide")], vec![], vec![text("c")]]);
        assert_eq!(t.rows[1].len(), 3, "short rows are padded");
    }

    #[test]
    fn reads_links_images_and_math() {
        let blocks = read_fragment(
            r#"<p>See <a href="https://x.org" title="X">the site</a> <img src="a b.png" alt="pic">
            <math alttext="x^2"><mi>x</mi></math></p>"#,
        );
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![
                text("See "),
                Inline::Link {
                    url: "https://x.org".into(),
                    title: Some("X".into()),
                    content: vec![text("the site")]
                },
                text(" "),
                Inline::Image {
                    url: "a b.png".into(),
                    title: None,
                    alt: "pic".into()
                },
                text(" "),
                Inline::Math("x^2".into()),
            ])]
        );
    }

    #[test]
    fn several_articles_mean_the_whole_body_is_content() {
        let doc = read(
            "<body><article><p>First post</p></article><article><p>Second post</p></article>\
             <table><tr><td>cell</td></tr></table></body>",
        );
        let text = doc.plain_text();
        assert!(
            text.contains("First post") && text.contains("Second post") && text.contains("cell"),
            "{text}"
        );
    }

    #[test]
    fn page_wide_forms_keep_their_content() {
        let doc =
            read("<body><form id=\"aspnetForm\"><p>Real content</p><input name=q></form></body>");
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![text("Real content")])]
        );
    }

    #[test]
    fn non_breaking_and_ideographic_spaces_are_kept() {
        let blocks = read_fragment("<p>10&nbsp;km\u{3000}全角</p>");
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![text("10\u{a0}km\u{3000}全角")])]
        );
    }

    #[test]
    fn nested_list_alone_keeps_the_list_tight() {
        let blocks = read_fragment("<ul><li>one<ul><li>sub</li></ul></li><li>two</li></ul>");
        let [Block::List(list)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(list.tight);
        let blocks = read_fragment("<ul><li><p>a</p><p>b</p></li></ul>");
        let [Block::List(list)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(!list.tight);
    }

    #[test]
    fn block_mathml_becomes_display_math() {
        let blocks = read_fragment(
            r#"<p>Before</p><math display="block" alttext="\sum x"><mi>x</mi></math>"#,
        );
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec![text("Before")]),
                Block::Math(r"\sum x".into())
            ]
        );
    }

    #[test]
    fn br_breaks_lines_without_stray_spaces() {
        let blocks = read_fragment("<p>one <br> two</p>");
        assert_eq!(
            blocks,
            vec![Block::Paragraph(vec![
                text("one"),
                Inline::LineBreak,
                text("two")
            ])]
        );
    }
}
