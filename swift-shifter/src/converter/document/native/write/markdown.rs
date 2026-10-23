//! [`Document`] → GitHub-flavoured Markdown, readable as plain text and
//! round-trippable through our own reader.

use serde::Serialize;

use super::{BlockWriter, Footnotes, indent_rest};
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, RawFormat, Table,
};

pub fn write(doc: &Document) -> String {
    let (notes, note_bodies) = Footnotes::collect(doc);
    let w = Writer { notes };
    let mut parts = Vec::new();
    if let Some(front) = front_matter(doc) {
        parts.push(front);
    }
    let body = w.blocks(&doc.blocks);
    if !body.is_empty() {
        parts.push(body);
    }
    for (i, body) in note_bodies.iter().enumerate() {
        parts.push(format!(
            "[^{}]: {}",
            i + 1,
            indent_rest(&w.blocks(body), "    ")
        ));
    }
    let mut out = parts.join("\n\n");
    out.push('\n');
    out
}

#[derive(Serialize)]
struct FrontMatter<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    author: &'a Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    date: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lang: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

fn front_matter(doc: &Document) -> Option<String> {
    let m = &doc.meta;
    let fm = FrontMatter {
        title: m.title.as_deref(),
        author: &m.authors,
        date: m.date.as_deref(),
        lang: m.lang.as_deref(),
        description: m.description.as_deref(),
    };
    if fm.title.is_none()
        && fm.author.is_empty()
        && fm.date.is_none()
        && fm.lang.is_none()
        && fm.description.is_none()
    {
        return None;
    }
    let yaml = serde_yaml::to_string(&fm).ok()?;
    Some(format!("---\n{yaml}---"))
}

struct Writer {
    notes: Footnotes,
}

impl BlockWriter for Writer {
    fn block(&self, block: &Block) -> Option<String> {
        self.block_with_marker(block, ListMarker::Primary)
    }

    /// Like the default, but consecutive lists of the same kind alternate
    /// markers (`-`/`*`, `.`/`)`): with one marker they'd read back as a
    /// single list.
    fn blocks_joined(&self, blocks: &[Block], sep: &str) -> String {
        let mut parts = Vec::new();
        // The previous written block, if it was a list: (ordered, marker).
        let mut prev_list: Option<(bool, ListMarker)> = None;
        for block in blocks {
            let marker = match (block, prev_list) {
                (Block::List(list), Some((ordered, marker))) if list.start.is_some() == ordered => {
                    marker.other()
                }
                _ => ListMarker::Primary,
            };
            // Blocks written elsewhere (footnotes) don't separate lists.
            let Some(text) = self.block_with_marker(block, marker) else {
                continue;
            };
            prev_list = match block {
                Block::List(list) => Some((list.start.is_some(), marker)),
                _ => None,
            };
            parts.push(text);
        }
        parts.join(sep)
    }
}

/// Which of a list type's two markers to use.
#[derive(Clone, Copy, PartialEq)]
enum ListMarker {
    /// `-` or `1.`
    Primary,
    /// `*` or `1)`
    Alternate,
}

impl ListMarker {
    fn other(self) -> Self {
        match self {
            ListMarker::Primary => ListMarker::Alternate,
            ListMarker::Alternate => ListMarker::Primary,
        }
    }
}

impl Writer {
    fn block_with_marker(&self, block: &Block, marker: ListMarker) -> Option<String> {
        Some(match block {
            Block::Heading { level, content } => {
                let hashes = "#".repeat(usize::from(*level.clamp(&1, &6)));
                let mut text = self.inlines(content, Ctx::Heading);
                // A trailing `#` would be read as a closing sequence.
                if text.ends_with('#') && !text.ends_with("\\#") {
                    text.insert(text.len() - 1, '\\');
                }
                format!("{hashes} {text}")
            }
            Block::Paragraph(content) => self.inlines(content, Ctx::Block),
            Block::Quote(children) => quote(&self.blocks(children)),
            Block::List(list) => self.list(list, marker),
            Block::Code { lang, code } => {
                let fence = "`".repeat(longest_run(code, '`').max(2) + 1);
                format!("{fence}{}\n{code}\n{fence}", lang.as_deref().unwrap_or(""))
            }
            // Empty `$$` reads back as text, or as display math around
            // the text after it; an empty formula shows nothing anyway.
            Block::Math(tex) if tex.trim().is_empty() => return None,
            Block::Math(tex) => format!("$$\n{}\n$$", tex.trim()),
            Block::Table(table) => self.table(table),
            Block::ThematicBreak => "---".to_string(),
            Block::FootnoteDefinition { .. } => return None,
            Block::Raw {
                format: RawFormat::Html,
                text,
            } => text.clone(),
        })
    }

    fn list(&self, list: &List, marker: ListMarker) -> String {
        let mut items = Vec::new();
        for (i, item) in list.items.iter().enumerate() {
            let number = list.start.unwrap_or(1) + i as u64;
            let mut prefix = match (list.start, marker) {
                (Some(_), ListMarker::Primary) => format!("{number}. "),
                (Some(_), ListMarker::Alternate) => format!("{number}) "),
                (None, ListMarker::Primary) => "- ".to_string(),
                (None, ListMarker::Alternate) => "* ".to_string(),
            };
            let width = prefix.len();
            if let Some(done) = item.task {
                prefix.push_str(if done { "[x] " } else { "[ ] " });
            }
            let sep = if list.tight { "\n" } else { "\n\n" };
            let body = self.blocks_joined(&item.content, sep);
            items.push(format!(
                "{prefix}{}",
                indent_rest(&body, &" ".repeat(width))
            ));
        }
        items.join(if list.tight { "\n" } else { "\n\n" })
    }

    fn table(&self, table: &Table) -> String {
        let width = table
            .header
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
        let row = |cells: &[Vec<Inline>]| {
            let mut out = String::from("|");
            for i in 0..width {
                let cell = cells.get(i).map(|c| self.inlines(c, Ctx::TableCell));
                out.push_str(&format!(" {} |", cell.unwrap_or_default()));
            }
            out
        };
        let mut lines = vec![row(&table.header)];
        let mut rule = String::from("|");
        for i in 0..width {
            rule.push_str(
                match table.align.get(i).copied().unwrap_or(Align::Default) {
                    Align::Default => " --- |",
                    Align::Left => " :-- |",
                    Align::Center => " :-: |",
                    Align::Right => " --: |",
                },
            );
        }
        lines.push(rule);
        lines.extend(table.rows.iter().map(|r| row(r)));
        lines.join("\n")
    }

    fn inlines(&self, inlines: &[Inline], ctx: Ctx) -> String {
        let mut out = String::new();
        for (i, inline) in inlines.iter().enumerate() {
            self.inline(&mut out, inline, ctx, &inlines[i + 1..]);
        }
        out
    }

    /// Write `inline`; `rest` is what follows it in the same run of inlines.
    fn inline(&self, out: &mut String, inline: &Inline, ctx: Ctx, rest: &[Inline]) {
        match inline {
            Inline::Text(t) => escape_into(out, t, ctx),
            Inline::Emph(c) => self.emphasis(out, "*", "em", c, ctx, rest),
            Inline::Strong(c) => self.emphasis(out, "**", "strong", c, ctx, rest),
            Inline::Strikethrough(c) => wrap(out, "~~", &self.inlines(c, ctx)),
            Inline::Superscript(c) => wrap(out, "^", &self.inlines(c, ctx)),
            Inline::Subscript(c) => wrap(out, "~", &self.inlines(c, ctx)),
            Inline::Code(code) => out.push_str(&code_span(code, ctx)),
            Inline::Math(tex) if tex.trim().is_empty() => {}
            Inline::Math(tex) => out.push_str(&format!("${}$", single_line(tex.trim(), ctx))),
            Inline::Link {
                url,
                title,
                content,
            } => {
                // Bare URLs read best as autolinks.
                if title.is_none()
                    && content.as_slice() == [Inline::Text(url.clone())]
                    && is_autolinkable(url)
                {
                    out.push_str(&format!("<{url}>"));
                } else {
                    let text = self.inlines(content, ctx);
                    out.push_str(&format!(
                        "[{text}]({}{})",
                        destination(url),
                        link_title(title)
                    ));
                }
            }
            Inline::Image { url, title, alt } => {
                out.push_str("![");
                escape_into(out, alt, ctx);
                out.push_str(&format!("]({}{})", destination(url), link_title(title)));
            }
            Inline::FootnoteRef(label) => match self.notes.number(label) {
                Some(n) => out.push_str(&format!("[^{n}]")),
                None => escape_into(out, &format!("[^{label}]"), ctx),
            },
            Inline::LineBreak => out.push_str(match ctx {
                Ctx::Block => "\\\n",
                Ctx::TableCell => "<br>",
                Ctx::Heading => " ",
            }),
            Inline::SoftBreak => out.push_str(match ctx {
                Ctx::Block => "\n",
                Ctx::TableCell | Ctx::Heading => " ",
            }),
            Inline::Raw { text, .. } => out.push_str(text),
        }
    }

    /// Emphasis or strong text, as `delim` runs where CommonMark would read
    /// them, otherwise as the HTML `tag` (`x<em>(y)</em>`), which our reader
    /// folds back into the same node.
    fn emphasis(
        &self,
        out: &mut String,
        delim: &str,
        tag: &str,
        content: &[Inline],
        ctx: Ctx,
        rest: &[Inline],
    ) {
        let inner = self.inlines(content, ctx);
        let (Some(first), Some(last)) = (inner.chars().next(), inner.chars().last()) else {
            return;
        };
        // Content always ends inside a delimiter or a tag, both punctuation,
        // which flanks like the end of the text: only what follows here in
        // the same run of inlines can stop a closing run.
        let before = out.chars().last();
        let after = self.first_char(rest);
        if left_flanking(before, first) && right_flanking(last, after) {
            wrap(out, delim, &inner);
        } else {
            out.push_str(&format!("<{tag}>{inner}</{tag}>"));
        }
    }

    /// The first character `inlines` would write, or one of the same
    /// flanking kind, without writing them.
    fn first_char(&self, inlines: &[Inline]) -> Option<char> {
        inlines.iter().find_map(|inline| match inline {
            // Escapes only add a backslash before punctuation; a leading
            // space is only rewritten at the start of a line.
            Inline::Text(t) | Inline::Raw { text: t, .. } => t.chars().next(),
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strikethrough(c)
            | Inline::Superscript(c)
            | Inline::Subscript(c) => self.first_char(c).map(|_| '*'),
            Inline::Math(tex) if tex.trim().is_empty() => None,
            Inline::Code(_)
            | Inline::Math(_)
            | Inline::Link { .. }
            | Inline::Image { .. }
            | Inline::FootnoteRef(_) => Some('['),
            Inline::LineBreak | Inline::SoftBreak => Some(' '),
        })
    }
}

/// Whether a delimiter run between `before` and `next` is left-flanking,
/// so it can open emphasis (CommonMark, "Emphasis and strong emphasis").
fn left_flanking(before: Option<char>, next: char) -> bool {
    !next.is_whitespace() && (!is_punctuation(next) || before.is_none_or(space_or_punctuation))
}

/// Whether a delimiter run between `prev` and `after` is right-flanking,
/// so it can close emphasis.
fn right_flanking(prev: char, after: Option<char>) -> bool {
    !prev.is_whitespace() && (!is_punctuation(prev) || after.is_none_or(space_or_punctuation))
}

fn space_or_punctuation(c: char) -> bool {
    c.is_whitespace() || is_punctuation(c)
}

/// CommonMark's Unicode punctuation (categories P and S), approximated
/// outside ASCII as anything that's neither alphanumeric nor whitespace.
fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation() || (!c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace())
}

/// Where inline content is written. Table cells and headings must stay on
/// one line; table cells also can't hold unescaped pipes.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Block,
    TableCell,
    Heading,
}

fn wrap(out: &mut String, delim: &str, inner: &str) {
    if inner.is_empty() {
        return;
    }
    out.push_str(delim);
    out.push_str(inner);
    out.push_str(delim);
}

/// Append `text`, escaping only what would otherwise change meaning, so
/// ordinary text (`snake_case`, `3 * 4`) stays readable. Whether a character
/// opens a line is read from what's already in `out`.
fn escape_into(out: &mut String, text: &str, ctx: Ctx) {
    let chars: Vec<char> = text.chars().collect();
    let mut line = LineState::of(out);
    for (i, &c) in chars.iter().enumerate() {
        let next = chars.get(i + 1).copied();
        let escaped: Option<&str> = match c {
            // Indentation would make a code block or be dropped: keep it
            // as character references, which render as the same spaces.
            ' ' if line.blank => Some("&#32;"),
            '\t' if line.blank => Some("&#9;"),
            '\\' | '`' | '[' | ']' | '*' | '$' | '^' | '~' => Some(""),
            // Intraword underscores never emphasise.
            '_' => {
                let prev = i.checked_sub(1).map(|j| chars[j]);
                let intraword = prev.is_some_and(char::is_alphanumeric)
                    && next.is_some_and(char::is_alphanumeric);
                (!intraword).then_some("")
            }
            '<' if next
                .is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?')) =>
            {
                Some("")
            }
            '&' if starts_entity(&chars[i + 1..]) => Some(""),
            '|' if ctx == Ctx::TableCell => Some(""),
            // Block markers at the start of a line.
            '#' | '>' | '-' | '+' | '=' if line.blank => Some(""),
            // "12." or "3)" at the start of a line opens an ordered list.
            '.' | ')' if line.digits.is_some_and(|n| (1..=9).contains(&n)) => Some(""),
            _ => None,
        };
        let piece = match escaped {
            // A replacement for the character itself.
            Some(replacement) if !replacement.is_empty() => replacement.to_string(),
            // A backslash escape.
            Some(_) => format!("\\{c}"),
            None => c.to_string(),
        };
        out.push_str(&piece);
        line.feed(&piece);
    }
}

/// What the current output line holds so far, kept up to date as text is
/// appended (rescanning the line per character would be quadratic).
struct LineState {
    /// Only spaces so far.
    blank: bool,
    /// `Some(n)` while the line is exactly `n` ASCII digits.
    digits: Option<usize>,
}

impl LineState {
    fn of(out: &str) -> Self {
        let mut state = LineState {
            blank: true,
            digits: Some(0),
        };
        state.feed(out.rsplit('\n').next().unwrap_or(""));
        state
    }

    fn feed(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\n' => {
                    self.blank = true;
                    self.digits = Some(0);
                }
                ' ' => self.digits = None,
                _ => {
                    self.blank = false;
                    self.digits = match self.digits {
                        Some(n) if c.is_ascii_digit() => Some(n + 1),
                        _ => None,
                    };
                }
            }
        }
    }
}

/// Whether `rest` (the text after an `&`) would be read as an entity or a
/// numeric character reference, e.g. `amp;`, `#169;`, `#x41;`.
fn starts_entity(rest: &[char]) -> bool {
    let Some(end) = rest.iter().take(33).position(|&c| c == ';') else {
        return false;
    };
    let name = &rest[..end];
    match name {
        ['#', 'x' | 'X', hex @ ..] => !hex.is_empty() && hex.iter().all(char::is_ascii_hexdigit),
        ['#', digits @ ..] => !digits.is_empty() && digits.iter().all(char::is_ascii_digit),
        [first, ..] => first.is_ascii_alphabetic() && name.iter().all(char::is_ascii_alphanumeric),
        [] => false,
    }
}

fn code_span(code: &str, ctx: Ctx) -> String {
    let code = single_line(code, ctx);
    let ticks = "`".repeat(longest_run(&code, '`') + 1);
    let pad = if code.starts_with('`') || code.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{code}{pad}{ticks}")
}

/// Code and math spans can't hold newlines (a following line could start a
/// list or end a table row), and in table cells `|` must be escaped even
/// inside them.
fn single_line(text: &str, ctx: Ctx) -> String {
    let text = text.replace(['\r', '\n'], " ");
    if ctx == Ctx::TableCell {
        text.replace('|', "\\|")
    } else {
        text
    }
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        run = if ch == c { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

fn is_autolinkable(url: &str) -> bool {
    (url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:"))
        && !url.contains(|c: char| c.is_whitespace() || c == '<' || c == '>')
}

/// Link destinations with spaces or parentheses go in angle brackets.
fn destination(url: &str) -> String {
    if url.is_empty() || url.contains(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        format!("<{}>", url.replace('<', "%3C").replace('>', "%3E"))
    } else {
        url.to_string()
    }
}

fn link_title(title: &Option<String>) -> String {
    title
        .as_deref()
        .map(|t| format!(" \"{}\"", t.replace('"', "\\\"")))
        .unwrap_or_default()
}

fn quote(text: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                ">".to_string()
            } else {
                format!("> {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::read::{html, markdown, plain};

    fn doc_of(blocks: Vec<Block>) -> Document {
        Document {
            blocks,
            ..Document::default()
        }
    }

    /// Markdown we write must read back as the document we wrote.
    fn assert_round_trips(doc: &Document) {
        let md = write(doc);
        assert_eq!(
            markdown::read(&md).blocks,
            doc.blocks,
            "re-reading changed it:\n{md}"
        );
    }

    #[test]
    fn writes_gfm_from_html() {
        let doc = html::read(
            "<html><head><title>T</title></head><body><h2>Head</h2>\
             <p>Some <em>em</em>, <strong>bold</strong> and <code>x`y</code>.</p>\
             <ul><li>one</li><li>two</li></ul><pre><code class=\"language-js\">a()</code></pre>\
             <table><tr><th>a|b</th></tr><tr><td>1<br>2</td></tr></table></body></html>",
        );
        assert_eq!(
            write(&doc),
            "---\ntitle: T\n---\n\n## Head\n\nSome *em*, **bold** and ``x`y``.\n\n- one\n- two\n\n\
             ```js\na()\n```\n\n| a\\|b |\n| --- |\n| 1<br>2 |\n"
        );
    }

    #[test]
    fn escapes_only_meaningful_characters() {
        let doc =
            markdown::read("snake\\_case\\_name, 3 \\* 4, a\\_ \\[x\\] and \\# not heading\n");
        let md = write(&doc);
        assert_eq!(
            md,
            "snake_case_name, 3 \\* 4, a\\_ \\[x\\] and # not heading\n"
        );
        assert_round_trips(&doc);
    }

    #[test]
    fn escapes_block_markers_at_line_start() {
        let doc = plain::read("# hash\n- dash\n1. one\n> gt\n2) paren\n===");
        assert_eq!(
            write(&doc),
            "\\# hash\\\n\\- dash\\\n1\\. one\\\n\\> gt\\\n2\\) paren\\\n\\===\n"
        );
        assert_round_trips(&doc);
    }

    #[test]
    fn leading_spaces_and_entities_survive() {
        assert_round_trips(&plain::read(
            "    indented\n  - not a list\nAT&amp;T &copy; & so on",
        ));
    }

    #[test]
    fn table_cells_keep_pipes_and_newlines_out() {
        let doc = doc_of(html::read_fragment(
            "<table><tr><th>h</th></tr><tr><td><code>a|b\nc</code> <math alttext=\"x|y\"></math></td></tr></table>",
        ));
        let md = write(&doc);
        assert!(md.contains("| `a\\|b c` $x\\|y$ |"), "{md}");
    }

    #[test]
    fn headings_stay_on_one_line_and_keep_trailing_hash() {
        let doc = doc_of(html::read_fragment(
            "<h1>Main<br>Subtitle</h1><h2>Issue #</h2>",
        ));
        assert_eq!(write(&doc), "# Main Subtitle\n\n## Issue \\#\n");
    }

    #[test]
    fn adjacent_lists_alternate_markers() {
        let doc = doc_of(html::read_fragment(
            "<ul><li>a</li></ul><ul><li>b</li></ul><ol><li>c</li></ol><ol><li>d</li></ol>",
        ));
        assert_eq!(write(&doc), "- a\n\n* b\n\n1. c\n\n1) d\n");
        assert_round_trips(&doc);
    }

    #[test]
    fn nested_and_loose_lists_indent_correctly() {
        let doc = markdown::read("1. one\n\n   para\n2. two\n   - nested\n");
        assert_eq!(write(&doc), "1. one\n\n   para\n\n2. two\n\n   - nested\n");
        assert_round_trips(&doc);
    }

    #[test]
    fn links_images_footnotes_and_math() {
        let doc = markdown::read(
            "See <https://a.org>, [docs](<my file.md> \"Title\"), ![alt](i.png)[^z] and $e=mc^2$.\n\n[^z]: Note\n    more.\n",
        );
        assert_eq!(
            write(&doc),
            "See <https://a.org>, [docs](<my file.md> \"Title\"), ![alt](i.png)[^1] and $e=mc^2$.\n\n[^1]: Note\n    more.\n"
        );
    }

    #[test]
    fn emphasis_that_would_not_flank_is_written_as_html() {
        let doc = doc_of(html::read_fragment(
            "<p>x<em>(y)</em> and a<strong>[b]</strong>c</p>",
        ));
        assert_eq!(
            write(&doc),
            "x<em>(y)</em> and a<strong>\\[b\\]</strong>c\n"
        );
        assert_round_trips(&doc);
    }

    #[test]
    fn emphasis_keeps_delimiters_where_they_flank() {
        let doc = doc_of(html::read_fragment(
            "<p>a<em>b</em>c, (<em>(y)</em>), <strong>x</strong>! and <em>y.</em></p>",
        ));
        assert_eq!(write(&doc), "a*b*c, (*(y)*), **x**! and *y.*\n");
        assert_round_trips(&doc);
    }

    #[test]
    fn emphasis_next_to_punctuation_round_trips() {
        let edges = ["", " ", "a", "(", ")", ".", "\u{00e9}", "\u{2014}"];
        let insides = ["y", "(y)", "y.", ".y", "[y]", "y\u{2014}"];
        for tag in ["em", "strong"] {
            for before in edges {
                for after in edges {
                    for inside in insides {
                        let html = format!("<p>z{before}<{tag}>{inside}</{tag}>{after}z</p>");
                        assert_round_trips(&doc_of(html::read_fragment(&html)));
                    }
                }
            }
        }
    }

    #[test]
    fn nested_emphasis_next_to_punctuation_round_trips() {
        for html in [
            "<p>a<strong><em>(b)</em></strong>c</p>",
            "<p>a<em>x <strong>(b)</strong></em>c</p>",
            "<p>a<em>(b)</em><strong>(c)</strong>d</p>",
            "<p>a<em><code>b</code></em>c</p>",
        ] {
            assert_round_trips(&doc_of(html::read_fragment(html)));
        }
    }

    #[test]
    fn raw_html_blocks_are_written_verbatim() {
        let src = "<details>\n<summary>More</summary>\n</details>\n";
        assert_eq!(write(&markdown::read(src)), src);
    }
}
