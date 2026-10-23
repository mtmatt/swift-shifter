//! [`Document`] → standalone HTML5: a styled page with light/dark themes,
//! math as MathML, heading ids and numbered footnotes.

use pulldown_latex::event::{Content, Event, ScriptPosition};
use pulldown_latex::{Parser, RenderConfig, Storage, config::DisplayMode};
use typst::math::Limits;
use typst::utils::default_math_class;
use unicode_math_class::MathClass;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use super::{DocumentIds, Footnotes, renderable_tex};
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, RawFormat, Table, inline_text,
};

pub fn write(doc: &Document) -> String {
    let mut w = Writer::new(doc, NoteStyle::Section);
    w.blocks(&doc.blocks);
    w.footnote_section();
    page(doc, &w.out)
}

/// Where footnote bodies go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteStyle {
    /// One numbered list at the end of the page.
    Section,
    /// EPUB: an `<aside epub:type="footnote">` per note, at the end of the
    /// chapter that first references it (see [`Writer::chapter`]).
    EpubAsides,
}

/// A heading as written, for tables of contents.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WrittenHeading {
    pub level: u8,
    pub text: String,
    pub id: String,
}

fn page(doc: &Document, body: &str) -> String {
    let lang = doc
        .effective_lang()
        .map(|l| format!(" lang=\"{}\"", escape_attr(&l)))
        .unwrap_or_default();
    let title = escape(doc.meta.title.as_deref().unwrap_or(""));
    let mut head = String::new();
    for author in &doc.meta.authors {
        head.push_str(&format!(
            "<meta name=\"author\" content=\"{}\">\n",
            escape_attr(author)
        ));
    }
    if let Some(description) = &doc.meta.description {
        head.push_str(&format!(
            "<meta name=\"description\" content=\"{}\">\n",
            escape_attr(description)
        ));
    }
    format!(
        "<!DOCTYPE html>\n<html{lang}>\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n{head}<style>\n{STYLE}{MATH_STYLE}</style>\n</head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n"
    )
}

/// Renders blocks to HTML. One writer can render a document in several
/// parts (EPUB chapters) with shared footnote numbers and heading ids.
pub(crate) struct Writer {
    out: String,
    notes: Footnotes,
    /// Body of note `n` at index `n - 1`.
    note_bodies: Vec<Vec<Block>>,
    style: NoteStyle,
    /// References written so far per note number, so each gets its own id.
    ref_counts: RefCell<HashMap<usize, usize>>,
    /// [`NoteStyle::EpubAsides`]: notes referenced in the current chapter
    /// whose body hasn't been written yet, in reference order.
    pending_notes: RefCell<Vec<usize>>,
    written_notes: HashSet<usize>,
    ids: DocumentIds,
    /// The note whose body is being written (see [`DocumentIds::next`]),
    /// 0 for the main text.
    part: usize,
    /// Top-level headings written since the last [`Writer::chapter`]
    /// (headings in quotes, lists and notes aren't part of the outline).
    headings: Vec<WrittenHeading>,
    /// How deep inside quotes, lists or notes the writer is.
    depth: usize,
}

impl Writer {
    pub fn new(doc: &Document, style: NoteStyle) -> Self {
        let (notes, note_bodies) = Footnotes::collect(doc);
        Writer {
            out: String::new(),
            ids: DocumentIds::new(&doc.blocks, &note_bodies),
            part: 0,
            notes,
            note_bodies,
            style,
            ref_counts: RefCell::new(HashMap::new()),
            pending_notes: RefCell::new(Vec::new()),
            written_notes: HashSet::new(),
            headings: Vec::new(),
            depth: 0,
        }
    }

    /// One part of the document as an HTML fragment, followed by the asides
    /// of the notes it references first (and the notes those reference), and
    /// its top-level headings.
    pub fn chapter(&mut self, blocks: &[Block]) -> (String, Vec<WrittenHeading>) {
        self.blocks(blocks);
        while !self.pending_notes.borrow().is_empty() {
            let pending = std::mem::take(&mut *self.pending_notes.borrow_mut());
            for n in pending {
                if self.written_notes.insert(n) {
                    self.aside(n);
                }
            }
        }
        (
            std::mem::take(&mut self.out),
            std::mem::take(&mut self.headings),
        )
    }

    fn aside(&mut self, n: usize) {
        let body = self.note_bodies[n - 1].clone();
        self.out.push_str(&format!(
            "<aside id=\"fn-{n}\" epub:type=\"footnote\" role=\"doc-footnote\">\n"
        ));
        self.note(n, &body);
        self.out.push_str(&format!(
            "<p><a href=\"#fnref-{n}\" class=\"footnote-back\" aria-label=\"Back to reference {n}\">↩</a></p>\n</aside>\n"
        ));
    }

    fn blocks(&mut self, blocks: &[Block]) {
        for block in blocks {
            self.block(block);
        }
    }

    fn nested(&mut self, write: impl FnOnce(&mut Self)) {
        self.depth += 1;
        write(self);
        self.depth -= 1;
    }

    /// The body of note `n`.
    fn note(&mut self, n: usize, body: &[Block]) {
        let part = std::mem::replace(&mut self.part, n);
        self.nested(|w| w.blocks(body));
        self.part = part;
    }

    fn block(&mut self, block: &Block) {
        match block {
            Block::Heading { level, content } => {
                let level = level.clamp(&1, &6);
                let id = self.ids.next(self.part, content);
                if self.depth == 0 {
                    self.headings.push(WrittenHeading {
                        level: *level,
                        text: inline_text(content),
                        id: id.clone(),
                    });
                }
                let inner = self.inlines(content);
                self.out.push_str(&format!(
                    "<h{level} id=\"{}\">{inner}</h{level}>\n",
                    escape_attr(&id)
                ));
            }
            Block::Paragraph(content) => {
                let inner = self.inlines(content);
                self.out.push_str(&format!("<p>{inner}</p>\n"));
            }
            Block::Quote(children) => {
                self.out.push_str("<blockquote>\n");
                self.nested(|w| w.blocks(children));
                self.out.push_str("</blockquote>\n");
            }
            Block::List(list) => self.nested(|w| w.list(list)),
            Block::Code { lang, code } => {
                let class = lang
                    .as_deref()
                    .map(|l| format!(" class=\"language-{}\"", escape_attr(l)))
                    .unwrap_or_default();
                self.out.push_str(&format!(
                    "<pre><code{class}>{}</code></pre>\n",
                    escape(code)
                ));
            }
            Block::Math(tex) => {
                self.out.push_str(&format!(
                    "<div class=\"math-display\">{}</div>\n",
                    math(tex, DisplayMode::Block)
                ));
            }
            Block::Table(table) => self.table(table),
            Block::ThematicBreak => self.out.push_str("<hr>\n"),
            // Written once, numbered, at the end.
            Block::FootnoteDefinition { .. } => {}
            Block::Raw {
                format: RawFormat::Html,
                text,
            } => {
                self.out.push_str(text.trim_end());
                self.out.push('\n');
            }
        }
    }

    fn list(&mut self, list: &List) {
        let (open, close) = match list.start {
            Some(1) => ("<ol>".to_string(), "</ol>"),
            Some(start) => (format!("<ol start=\"{start}\">"), "</ol>"),
            None => ("<ul>".to_string(), "</ul>"),
        };
        self.out.push_str(&open);
        self.out.push('\n');
        for item in &list.items {
            match item.task {
                Some(done) => {
                    let checked = if done { " checked" } else { "" };
                    self.out.push_str(&format!(
                        "<li class=\"task\"><input type=\"checkbox\" disabled{checked}> "
                    ));
                }
                None => self.out.push_str("<li>"),
            }
            // Tight lists show their paragraphs without <p> spacing, even
            // when an item also holds a nested list.
            if list.tight {
                for (i, child) in item.content.iter().enumerate() {
                    match child {
                        Block::Paragraph(content) => {
                            if i > 0 {
                                self.out.push_str("<br>\n");
                            }
                            let inner = self.inlines(content);
                            self.out.push_str(&inner);
                        }
                        other => {
                            self.out.push('\n');
                            self.block(other);
                        }
                    }
                }
            } else {
                self.out.push('\n');
                self.blocks(&item.content);
            }
            self.out.push_str("</li>\n");
        }
        self.out.push_str(close);
        self.out.push('\n');
    }

    fn table(&mut self, table: &Table) {
        let style = |i: usize| match table.align.get(i).copied().unwrap_or(Align::Default) {
            Align::Default => String::new(),
            Align::Left => " style=\"text-align: left\"".into(),
            Align::Center => " style=\"text-align: center\"".into(),
            Align::Right => " style=\"text-align: right\"".into(),
        };
        self.out.push_str("<table>\n");
        if !table.header.is_empty() {
            self.out.push_str("<thead>\n<tr>");
            for (i, cell) in table.header.iter().enumerate() {
                let inner = self.inlines(cell);
                self.out.push_str(&format!("<th{}>{inner}</th>", style(i)));
            }
            self.out.push_str("</tr>\n</thead>\n");
        }
        if !table.rows.is_empty() {
            self.out.push_str("<tbody>\n");
            for row in &table.rows {
                self.out.push_str("<tr>");
                for (i, cell) in row.iter().enumerate() {
                    let inner = self.inlines(cell);
                    self.out.push_str(&format!("<td{}>{inner}</td>", style(i)));
                }
                self.out.push_str("</tr>\n");
            }
            self.out.push_str("</tbody>\n");
        }
        self.out.push_str("</table>\n");
    }

    fn footnote_section(&mut self) {
        if self.note_bodies.is_empty() {
            return;
        }
        let bodies = std::mem::take(&mut self.note_bodies);
        self.out
            .push_str("<section class=\"footnotes\">\n<hr>\n<ol>\n");
        for (i, body) in bodies.iter().enumerate() {
            let n = i + 1;
            self.out.push_str(&format!("<li id=\"fn-{n}\">\n"));
            self.note(n, body);
            // Back to the first reference.
            self.out.push_str(&format!(
                "<a href=\"#fnref-{n}\" class=\"footnote-back\" aria-label=\"Back to reference {n}\">↩</a>\n</li>\n"
            ));
        }
        self.out.push_str("</ol>\n</section>\n");
    }

    fn inlines(&self, inlines: &[Inline]) -> String {
        let mut out = String::new();
        for inline in inlines {
            self.inline(&mut out, inline);
        }
        out
    }

    fn inline(&self, out: &mut String, inline: &Inline) {
        match inline {
            Inline::Text(t) => out.push_str(&escape(t)),
            Inline::Emph(c) => wrap(out, "em", &self.inlines(c)),
            Inline::Strong(c) => wrap(out, "strong", &self.inlines(c)),
            Inline::Strikethrough(c) => wrap(out, "del", &self.inlines(c)),
            Inline::Superscript(c) => wrap(out, "sup", &self.inlines(c)),
            Inline::Subscript(c) => wrap(out, "sub", &self.inlines(c)),
            Inline::Code(code) => wrap(out, "code", &escape(code)),
            Inline::Math(tex) => out.push_str(&math(tex, DisplayMode::Inline)),
            Inline::Link {
                url,
                title,
                content,
            } => {
                let title = title
                    .as_deref()
                    .map(|t| format!(" title=\"{}\"", escape_attr(t)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "<a href=\"{}\"{title}>{}</a>",
                    escape_attr(url),
                    self.inlines(content)
                ));
            }
            Inline::Image { url, title, alt } => {
                let title = title
                    .as_deref()
                    .map(|t| format!(" title=\"{}\"", escape_attr(t)))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\"{title} loading=\"lazy\">",
                    escape_attr(url),
                    escape_attr(alt)
                ));
            }
            Inline::FootnoteRef(label) => match self.notes.number(label) {
                Some(n) => {
                    // The first reference is `fnref-N` (the back-link's
                    // target); later ones to the same note `fnref-N-2`, ….
                    let mut counts = self.ref_counts.borrow_mut();
                    let count = counts.entry(n).or_insert(0);
                    *count += 1;
                    let id = match *count {
                        1 => format!("fnref-{n}"),
                        k => format!("fnref-{n}-{k}"),
                    };
                    let epub = match self.style {
                        NoteStyle::Section => "",
                        NoteStyle::EpubAsides => {
                            if !self.written_notes.contains(&n) {
                                let mut pending = self.pending_notes.borrow_mut();
                                if !pending.contains(&n) {
                                    pending.push(n);
                                }
                            }
                            " epub:type=\"noteref\" role=\"doc-noteref\""
                        }
                    };
                    out.push_str(&format!(
                        "<sup class=\"footnote-ref\"><a href=\"#fn-{n}\" id=\"{id}\"{epub}>{n}</a></sup>"
                    ));
                }
                None => out.push_str(&escape(&format!("[^{label}]"))),
            },
            Inline::LineBreak => out.push_str("<br>\n"),
            Inline::SoftBreak => out.push('\n'),
            Inline::Raw { text, .. } => out.push_str(text),
        }
    }
}

fn wrap(out: &mut String, tag: &str, inner: &str) {
    out.push_str(&format!("<{tag}>{inner}</{tag}>"));
}

/// MathML for a TeX formula, annotated with the TeX so it round-trips. TeX
/// the renderer can't parse is shown as code instead of an error box.
fn math(tex: &str, display: DisplayMode) -> String {
    let storage = Storage::new();
    let renderable = renderable_tex(tex);
    let mut events: Vec<_> = Parser::new(&renderable, &storage).collect();
    if events.iter().any(Result::is_err) {
        return format!("<code class=\"math\">{}</code>", escape(tex));
    }
    promote_large_operators(&mut events);
    // pulldown-latex writes the annotation verbatim, so `<` in the TeX must
    // be escaped or it would open a tag.
    let annotation = escape(tex);
    let config = RenderConfig {
        display_mode: display,
        annotation: Some(&annotation),
        ..RenderConfig::default()
    };
    let mut mathml = String::new();
    match pulldown_latex::push_mathml(&mut mathml, events.into_iter(), config) {
        Ok(())
            if matches!(display, DisplayMode::Inline) && tex.starts_with("\\begin{aligned}[b]") =>
        {
            last_row_on_baseline(&mathml).unwrap_or(mathml)
        }
        Ok(()) => mathml,
        Err(_) => format!("<code class=\"math\">{}</code>", escape(tex)),
    }
}

/// The table pulldown-latex writes for `aligned`.
const ALIGNED: &str = "<mtable class=\"menv-alignlike menv-align\">";

/// Inline math that is nothing but the start-aligned rows the Typst reader
/// writes for a multi-row equation in text (`aligned[b]` with each row
/// after an `&`), set with its last row on the text's baseline, as typst
/// and `[b]` set them. Browsers centre a table on the math axis, so the
/// rows above the last become an accent over it, which keeps their size,
/// with a padding in `MATH_STYLE` for the row gap the table had. Neither
/// takes any width to speak of (WebKit doesn't draw what takes none), so
/// both start at the same place, and a phantom of the whole table, of no
/// height, gives the formula its width. `None` for any other math.
fn last_row_on_baseline(mathml: &str) -> Option<String> {
    let start = mathml.find(ALIGNED)?;
    let end = mathml.find("</mtable></mrow><annotation")? + "</mtable>".len();
    if !mathml[..start].ends_with("<semantics><mrow>") {
        return None;
    }
    let table = &mathml[start..end];
    let rows = elements(
        &table[ALIGNED.len()..table.len() - "</mtable>".len()],
        "mtr",
    )?;
    let mut cells = rows
        .iter()
        .map(
            |row| match elements(row.strip_prefix("<mtr>")?.strip_suffix("</mtr>")?, "mtd")?[..] {
                ["<mtd></mtd>", cell] => Some(cell),
                _ => None,
            },
        )
        .collect::<Option<Vec<_>>>()?;
    let last = cells.pop()?.strip_prefix("<mtd>")?.strip_suffix("</mtd>")?;
    if cells.is_empty() {
        return None;
    }
    let above = &rows[..cells.len()];
    Some(format!(
        "{}<mover accent=\"true\" class=\"last-row-on-baseline\">\
         <mpadded width=\"0.001em\"><mrow>{last}</mrow></mpadded>\
         <mpadded width=\"0.001em\">{ALIGNED}{}</mtable></mpadded></mover>\
         <mphantom><mpadded height=\"0\" depth=\"0\">{table}</mpadded></mphantom>{}",
        &mathml[..start],
        above.concat(),
        &mathml[end..],
    ))
}

/// The `<tag>` elements `mathml` is a sequence of, or `None` if it has
/// anything else outside them.
fn elements<'a>(mathml: &'a str, tag: &str) -> Option<Vec<&'a str>> {
    let close = format!("</{tag}>");
    let (mut elements, mut depth, mut start) = (Vec::new(), 0, 0);
    for (i, _) in mathml.match_indices('<') {
        let rest = &mathml[i..];
        if rest.starts_with(&close) {
            if depth == 0 {
                return None;
            }
            depth -= 1;
            if depth == 0 {
                elements.push(&mathml[start..i + close.len()]);
                start = i + close.len();
            }
        } else if rest[1..].starts_with(tag) && rest[1 + tag.len()..].starts_with(['>', ' ']) {
            if depth == 0 && i != start {
                return None;
            }
            depth += 1;
        }
    }
    (depth == 0 && start == mathml.len()).then_some(elements)
}

/// Turns typed large operators (`∑`, `∫`), which pulldown-latex takes for
/// ordinary characters, into the operators its `\sum` and `\int` are, with
/// typst's limits: above and below in display math, but beside integrals.
/// Radicals, which typst reads as roots and LaTeX's `\surd` is an ordinary
/// symbol of, stay ordinary.
fn promote_large_operators<E>(events: &mut [Result<Event, E>]) {
    for i in 0..events.len() {
        let Ok(Event::Content(Content::Ordinary { content, .. })) = events[i] else {
            continue;
        };
        if default_math_class(content) != Some(MathClass::Large)
            || matches!(content, '√' | '∛' | '∜')
        {
            continue;
        }
        events[i] = Ok(Event::Content(Content::LargeOp {
            content,
            small: false,
        }));
        // A script comes before its base. Scripts beside it, where
        // pulldown-latex puts an ordinary character's, are the operator's
        // limits; an accent's or `\overset`'s stay above or below it.
        if matches!(Limits::for_char(content), Limits::Display)
            && let Some(Ok(Event::Script { position, .. })) =
                i.checked_sub(1).map(|before| &mut events[before])
            && *position == ScriptPosition::Right
        {
            *position = ScriptPosition::Movable;
        }
    }
}

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(text: &str) -> String {
    escape(text).replace('"', "&quot;")
}

/// Readable defaults in both colour schemes; CJK text gets fonts and line
/// breaking suited to it via the `lang` attribute, and is justified, except
/// in MathML's table cells, which keep their own alignment.
const STYLE: &str = r#":root { color-scheme: light dark; --fg: #1d1d1f; --bg: #ffffff; --muted: #6e6e73; --rule: #d2d2d7; --code-bg: #f5f5f7; --link: #0a66c2; }
@media (prefers-color-scheme: dark) { :root { --fg: #f5f5f7; --bg: #161618; --muted: #a1a1a6; --rule: #3a3a3c; --code-bg: #232326; --link: #6cb4ff; } }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 17px/1.65 -apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", "Helvetica Neue", Arial, "PingFang SC", "Hiragino Sans", "Microsoft YaHei", "Noto Sans CJK SC", sans-serif; }
main { max-width: 46rem; margin: 0 auto; padding: 3rem 1.25rem 4rem; }
:lang(zh):not(mtd), :lang(ja):not(mtd), :lang(ko):not(mtd) { line-break: strict; text-align: justify; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin: 2em 0 0.6em; }
h1 { font-size: 2em; margin-top: 0; }
p, ul, ol, blockquote, pre, table, .math-display { margin: 0 0 1.1em; }
a { color: var(--link); }
img { max-width: 100%; height: auto; }
blockquote { margin-left: 0; padding-left: 1em; border-left: 3px solid var(--rule); color: var(--muted); }
code, pre { font-family: ui-monospace, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace; font-size: 0.88em; }
code { background: var(--code-bg); padding: 0.1em 0.3em; border-radius: 4px; }
pre { background: var(--code-bg); padding: 1em; border-radius: 8px; overflow-x: auto; }
pre code { background: none; padding: 0; }
table { border-collapse: collapse; display: block; overflow-x: auto; }
th, td { border: 1px solid var(--rule); padding: 0.4em 0.75em; vertical-align: top; }
th { background: var(--code-bg); }
hr { border: 0; border-top: 1px solid var(--rule); margin: 2em 0; }
li > ul, li > ol { margin: 0.2em 0 0; }
li.task { list-style: none; }
li.task input { margin: 0 0.4em 0 -1.3em; }
.math-display { overflow-x: auto; overflow-y: hidden; padding: 0.2em 0; }
.footnotes { font-size: 0.9em; color: var(--muted); }
.footnote-back { text-decoration: none; }
"#;

/// Column alignment for the MathML of TeX environments, by the classes
/// pulldown-latex gives their tables (adapted from its `styles.css`).
/// MathML centres table cells, so without these rules `aligned` loses its
/// right- and left-aligned columns at `&`, and `cases` and arrays their
/// left or right ones. Shared by the HTML page and the EPUB stylesheet.
///
/// Right alignment takes `-webkit-right` (and Firefox's `-moz-right`):
/// Chrome lays a cell's content out as blocks, which plain `right` doesn't
/// move. Only the tables of `aligned` (and `align`, `split`, …) and `cases`
/// lose MathML's cell padding, and get amsmath's 1em between `aligned`'s
/// column pairs and before `cases`' conditions instead. pulldown-latex
/// takes the padding off every cell, which would also close up the columns
/// of MathML passed through from the source. The empty row a trailing `\\`
/// leaves is hidden, as LaTeX ignores it.
///
/// Unlike pulldown-latex's, the selectors have no `m|` prefix: these rules
/// follow others, where an `@namespace` declaring it would be ignored, and
/// unprefixed selectors match MathML in HTML and XHTML alike. Left out:
/// its fonts, equation numbers and array borders, its `multline` rules
/// (which left-align every row), and its negation slash, which is drawn in
/// black and would vanish in dark mode.
pub(crate) const MATH_STYLE: &str = r#"mtable.menv-alignlike > mtr > mtd:nth-child(odd), mtable.menv-cells-right > mtr > mtd, mtd.cell-right { text-align: right; text-align: -webkit-right; text-align: -moz-right; }
mtable.menv-alignlike > mtr > mtd:nth-child(even), mtable.menv-cells-left > mtr > mtd, mtd.cell-left { text-align: left; }
mtable.menv-alignlike > mtr > mtd, mtable.menv-cases > mtr > mtd { padding-left: 0; padding-right: 0; }
mtable.menv-align > mtr > mtd:nth-child(2n+3) { padding-left: 1em; }
mtable.menv-cases > mtr > mtd:first-child { padding-right: 1em; }
mtr:last-child > mtd:empty:only-child { display: none; }
mover.last-row-on-baseline > mpadded > mtable > mtr:last-child > mtd { padding-bottom: 1ex; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::read::markdown;

    fn body(md: &str) -> String {
        let html = write(&markdown::read(md));
        let start = html.find("<main>\n").unwrap() + 7;
        let end = html.find("</main>").unwrap();
        html[start..end].to_string()
    }

    #[test]
    fn writes_a_standalone_page_with_metadata() {
        let html = write(&markdown::read(
            "---\ntitle: A & B\nauthor: Ann\n---\n\n文字內容很多很多\n",
        ));
        assert!(html.starts_with("<!DOCTYPE html>\n<html lang=\"zh-Hant\">"));
        assert!(html.contains("<title>A &amp; B</title>"));
        assert!(html.contains("<meta name=\"author\" content=\"Ann\">"));
        assert!(html.contains("<meta charset=\"utf-8\">"));
    }

    #[test]
    fn escapes_text_and_attributes() {
        assert_eq!(
            body("a < b & [x](https://e.org/?a=1&b=\"2\")\n"),
            "<p>a &lt; b &amp; <a href=\"https://e.org/?a=1&amp;b=&quot;2&quot;\">x</a></p>\n"
        );
    }

    #[test]
    fn headings_get_ids_and_lists_render() {
        assert_eq!(
            body("## Intro\n\n- a\n- [x] b\n\n3. c\n"),
            "<h2 id=\"intro\">Intro</h2>\n<ul>\n<li>a</li>\n\
             <li class=\"task\"><input type=\"checkbox\" disabled checked> b</li>\n</ul>\n\
             <ol start=\"3\">\n<li>c</li>\n</ol>\n"
        );
    }

    #[test]
    fn tight_items_with_nested_lists_stay_tight() {
        assert_eq!(
            body("- a\n  - b\n"),
            "<ul>\n<li>a\n<ul>\n<li>b</li>\n</ul>\n</li>\n</ul>\n"
        );
    }

    #[test]
    fn tables_carry_alignment() {
        let html = body("| a | b |\n|:-:|--:|\n| 1 | 2 |\n");
        assert!(html.contains("<th style=\"text-align: center\">a</th>"));
        assert!(html.contains("<td style=\"text-align: right\">2</td>"));
    }

    #[test]
    fn math_becomes_mathml_with_tex_annotation() {
        let html = body("$x^2$\n");
        assert!(html.contains("<math"), "{html}");
        assert!(html.contains("x^2"), "TeX annotation kept: {html}");
        let display = body("$$\\frac{1}{2}$$\n");
        assert!(display.contains("display=\"block\""), "{display}");
    }

    /// Regression for #65: typed large operators are operators, with
    /// typst's limits: above and below in display math (`∑`, `⋃`), and
    /// beside it inline and on integrals.
    #[test]
    fn typed_large_operators_take_limits() {
        let display = math("∑_{i=1}^n i + ⋃_k A_k + ∫_0^1 f", DisplayMode::Block);
        for drawn in [
            "<munderover><mo movablelimits=\"false\">∑</mo>",
            "<munder><mo movablelimits=\"false\">⋃</mo>",
            "<msubsup><mo movablelimits=\"false\">∫</mo>",
        ] {
            assert!(display.contains(drawn), "{drawn} in {display}");
        }
        let inline = math("∑_{i=1}^n i + ∏ x", DisplayMode::Inline);
        for drawn in [
            "<msubsup><mo movablelimits=\"false\">∑</mo>",
            "<mo movablelimits=\"false\">∏</mo>",
        ] {
            assert!(inline.contains(drawn), "{drawn} in {inline}");
        }
        // The annotation keeps the TeX as it was.
        assert!(display.contains("∑_{i=1}^n i"), "{display}");
        // An accent or `\overset` stays above or below it, inline too, and
        // a radical is no operator.
        for (tex, drawn) in [
            ("\\hat ∑", "<mover><mo movablelimits=\"false\">∑</mo>"),
            ("\\overset{a}∑", "<mover><mo movablelimits=\"false\">∑</mo>"),
            (
                "\\underbrace ∑_a",
                "<munder><munder><mo movablelimits=\"false\">∑</mo>",
            ),
            ("√2", "<mi>√</mi>"),
        ] {
            let inline = math(tex, DisplayMode::Inline);
            assert!(inline.contains(drawn), "{drawn} in {inline}");
        }
        // Characters in text are text.
        assert!(
            math("\\text{∑_a}", DisplayMode::Inline).contains("<mtext>∑_a</mtext>"),
            "text"
        );
    }

    #[test]
    fn math_annotation_is_escaped() {
        let html = body("$a<b$\n");
        assert!(html.contains("a&lt;b</annotation>"), "{html}");
        assert!(!html.contains("a<b"), "{html}");
    }

    #[test]
    fn repeated_footnote_refs_get_unique_ids() {
        let html = body("One[^n] two[^n].\n\n[^n]: Note.\n");
        assert!(html.contains("id=\"fnref-1\""), "{html}");
        assert!(html.contains("id=\"fnref-1-2\""), "{html}");
    }

    #[test]
    fn raw_html_blocks_pass_through() {
        let html = body(
            "<details><summary>More</summary>\n\nHidden\n\n</details>\n\n<iframe src=\"https://e.org\"></iframe>\n",
        );
        assert!(html.contains("<details><summary>More</summary>"), "{html}");
        assert!(
            html.contains("<iframe src=\"https://e.org\"></iframe>"),
            "{html}"
        );
    }

    #[test]
    fn unparseable_math_falls_back_to_code() {
        let html = body("$\\frac{1}$\n");
        assert!(
            html.contains("<code class=\"math\">\\frac{1}</code>"),
            "{html}"
        );
    }

    #[test]
    fn footnotes_are_numbered_and_linked() {
        let html = body("Text[^note].\n\n[^note]: The note.\n");
        assert!(html.contains("<a href=\"#fn-1\" id=\"fnref-1\">1</a>"));
        assert!(html.contains("<li id=\"fn-1\">\n<p>The note.</p>"));
    }

    #[test]
    fn code_blocks_keep_language_and_escape() {
        assert_eq!(
            body("```html\n<b>\n```\n"),
            "<pre><code class=\"language-html\">&lt;b&gt;</code></pre>\n"
        );
    }

    /// `MATH_STYLE` aligns columns by the classes pulldown-latex gives the
    /// tables of `aligned`, `cases` and arrays, so a pulldown-latex upgrade
    /// that renames them fails here rather than in a browser. Inline rows
    /// with their last on the baseline keep those of `aligned` above it.
    #[test]
    fn math_style_targets_classes_pulldown_latex_writes() {
        let mathml: String = [
            r"\begin{aligned} a &= b & c &= d \end{aligned}",
            r"\begin{cases} a & b \end{cases}",
            r"\begin{array}{lr} a & b \end{array}",
            r"\begin{subarray}{r} a \end{subarray}",
        ]
        .into_iter()
        .map(|tex| math(tex, DisplayMode::Block))
        .chain([math(
            r"\begin{aligned}[b]&{a } \\ &{ b}\end{aligned}",
            DisplayMode::Inline,
        )])
        .collect();
        let written: HashSet<&str> = mathml
            .split("class=\"")
            .skip(1)
            .filter_map(|rest| rest.split_once('"'))
            .flat_map(|(classes, _)| classes.split_whitespace())
            .collect();
        // The class names in `MATH_STYLE`'s selectors, one rule per line.
        let styled: HashSet<&str> = MATH_STYLE
            .lines()
            .filter_map(|rule| rule.split_once('{'))
            .flat_map(|(selectors, _)| selectors.split('.').skip(1))
            .map(|rest| {
                let end = rest
                    .find(|c: char| c != '-' && !c.is_alphanumeric())
                    .unwrap_or(rest.len());
                &rest[..end]
            })
            .collect();
        for class in [
            "menv-alignlike",
            "menv-align",
            "menv-cases",
            "menv-cells-left",
            "menv-cells-right",
            "cell-left",
            "cell-right",
            "last-row-on-baseline",
        ] {
            assert!(styled.contains(class), "{class} isn't styled");
        }
        for class in &styled {
            assert!(written.contains(class), "{class} isn't in {mathml}");
        }
        // A trailing `\\` leaves an empty last row, which `MATH_STYLE` hides.
        let trailing = math(r"\begin{pmatrix} a \\ \end{pmatrix}", DisplayMode::Block);
        assert!(
            trailing.contains("<mtr><mtd></mtd></mtr></mtable>"),
            "{trailing}"
        );
    }

    /// typst sets the rows of a multi-row equation in text with the last
    /// on the baseline, where a table would be centred on the math axis.
    #[test]
    fn inline_rows_end_on_the_baseline() {
        let doc = crate::converter::document::native::read::typst::read("x $a \\ -b \\ = c$ y\n");
        let html = write(&doc);
        let rows = |cells: &[&str]| -> String {
            cells
                .iter()
                .map(|cell| format!("<mtr><mtd></mtd><mtd><mrow>{cell}</mrow></mtd></mtr>"))
                .collect()
        };
        let (a, b, c) = ("<mi>a</mi>", "<mi>−</mi><mi>b</mi>", "<mo>=</mo><mi>c</mi>");
        let expected = format!(
            "<semantics><mrow><mover accent=\"true\" class=\"last-row-on-baseline\">\
             <mpadded width=\"0.001em\"><mrow><mrow>{c}</mrow></mrow></mpadded>\
             <mpadded width=\"0.001em\">{ALIGNED}{}</mtable></mpadded></mover>\
             <mphantom><mpadded height=\"0\" depth=\"0\">{ALIGNED}{}</mtable></mpadded></mphantom>\
             </mrow><annotation",
            rows(&[a, b]),
            rows(&[a, b, c]),
        );
        assert!(html.contains(&expected), "{html}");
        // The rows above keep the table's gap to the last.
        assert!(
            MATH_STYLE.contains("mover.last-row-on-baseline > mpadded > mtable > mtr:last-child > mtd { padding-bottom: 1ex; }"),
            "{MATH_STYLE}"
        );
        // A table inside the last row stays in it.
        let nested = math(
            r"\begin{aligned}[b]&{a} \\ &{\begin{matrix} 1 \\ 2 \end{matrix}}\end{aligned}",
            DisplayMode::Inline,
        );
        let base = nested.split("<mpadded width=\"0.001em\">").nth(1).unwrap();
        assert!(
            base.contains("<mn>1</mn>") && base.contains("<mn>2</mn>"),
            "{nested}"
        );
        // Rows without `[b]`, as a Markdown author writes `aligned`, stay
        // centred, as LaTeX sets them.
        let markdown = body("Rows $\\begin{aligned}&{a } \\\\ &{ b}\\end{aligned}$ here.\n");
        assert!(
            markdown.contains("<mtable") && !markdown.contains("<mover"),
            "{markdown}"
        );
        // So do display math, rows with columns, a single row and rows
        // inside other math.
        for (tex, display) in [
            (
                r"\begin{aligned}[b]&{a } \\ &{ b}\end{aligned}",
                DisplayMode::Block,
            ),
            (
                r"\begin{aligned}[b]a &= b \\  c &= d\end{aligned}",
                DisplayMode::Inline,
            ),
            (r"\begin{aligned}[b]&{a }\end{aligned}", DisplayMode::Inline),
            (
                r"\sqrt{\begin{aligned}[b]&{a } \\ &{ b}\end{aligned}}",
                DisplayMode::Inline,
            ),
            (
                r"x \begin{aligned}[b]&{a } \\ &{ b}\end{aligned}",
                DisplayMode::Inline,
            ),
        ] {
            let mathml = math(tex, display);
            assert!(!mathml.contains("<mover"), "{tex}: {mathml}");
        }
    }

    #[test]
    fn pages_carry_the_math_style() {
        let html = write(&markdown::read("Text.\n"));
        assert!(html.contains(&format!("{MATH_STYLE}</style>")), "{html}");
    }

    /// CJK text is justified, but not in MathML's cells, where it would
    /// override the centring of matrices, `gathered` and `\substack`.
    #[test]
    fn cjk_justification_skips_math_cells() {
        let justify = STYLE
            .lines()
            .find(|rule| rule.contains("text-align: justify"))
            .unwrap();
        let (selectors, _) = justify.split_once('{').unwrap();
        assert!(
            selectors
                .split(',')
                .all(|s| s.trim().ends_with(":not(mtd)")),
            "{justify}"
        );
    }
}
