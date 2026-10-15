//! [`Document`] → plain text. Lines aren't hard-wrapped (pandoc wraps at 72
//! columns, which breaks pasting into anything that wraps on its own).

use unicode_width::UnicodeWidthStr;

use super::{BlockWriter, Footnotes, indent_rest};
use crate::converter::document::native::ast::{
    Block, Document, Inline, List, RawFormat, Table, inline_text,
};
use crate::converter::document::native::read;

pub fn write(doc: &Document) -> String {
    let (notes, note_bodies) = Footnotes::collect(doc);
    let w = Writer { notes };
    let mut parts: Vec<String> = Vec::new();
    // The declared title, unless the document already opens with it.
    let opens_with_title = matches!(
        doc.blocks.first(),
        Some(Block::Heading { content, .. }) if Some(inline_text(content)) == doc.meta.title
    );
    if let (Some(title), false) = (&doc.meta.title, opens_with_title) {
        parts.push(underline(title, '='));
    }
    parts.extend(doc.blocks.iter().filter_map(|b| w.block(b)));
    if !note_bodies.is_empty() {
        let notes: Vec<String> = note_bodies
            .iter()
            .enumerate()
            .map(|(i, body)| format!("[{}] {}", i + 1, indent_rest(&w.blocks(body), "    ")))
            .collect();
        parts.push(format!("---\n\n{}", notes.join("\n\n")));
    }
    let mut out = parts.join("\n\n");
    out.push('\n');
    out
}

struct Writer {
    notes: Footnotes,
}

impl BlockWriter for Writer {
    fn block(&self, block: &Block) -> Option<String> {
        let text = match block {
            Block::Heading { level, content } => {
                let text = self.inlines(content);
                match level {
                    1 => underline(&text, '='),
                    2 => underline(&text, '-'),
                    _ => text,
                }
            }
            Block::Paragraph(content) => self.inlines(content),
            Block::Quote(children) => indent_all(&self.blocks(children), "  "),
            Block::List(list) => self.list(list),
            Block::Code { code, .. } => indent_all(code, "    "),
            Block::Math(tex) => indent_all(tex.trim(), "    "),
            Block::Table(table) => self.table(table),
            Block::ThematicBreak => "* * *".to_string(),
            Block::FootnoteDefinition { .. } => return None,
            // Raw HTML contributes its structure where the AST models it,
            // and its text where it doesn't. (Re-reading a nested Raw block
            // would return it unchanged, forever.)
            Block::Raw {
                format: RawFormat::Html,
                text,
            } => {
                let blocks: Vec<Block> = read::html::read_fragment(text)
                    .into_iter()
                    .map(|block| match block {
                        Block::Raw { text, .. } => {
                            Block::Paragraph(vec![Inline::Text(read::html::text_of(&text))])
                        }
                        other => other,
                    })
                    .collect();
                self.blocks(&blocks)
            }
        };
        (!text.trim().is_empty()).then_some(text)
    }
}

impl Writer {
    fn list(&self, list: &List) -> String {
        let mut number = list.start.unwrap_or(1);
        let items: Vec<String> = list
            .items
            .iter()
            .map(|item| {
                let marker = match list.start {
                    Some(_) => {
                        let m = format!("{number}. ");
                        number += 1;
                        m
                    }
                    None => "- ".to_string(),
                };
                let task = match item.task {
                    Some(true) => "[x] ",
                    Some(false) => "[ ] ",
                    None => "",
                };
                let body =
                    self.blocks_joined(&item.content, if list.tight { "\n" } else { "\n\n" });
                format!(
                    "{marker}{task}{}",
                    indent_rest(&body, &" ".repeat(marker.len()))
                )
            })
            .collect();
        items.join(if list.tight { "\n" } else { "\n\n" })
    }

    /// Columns padded to their widest cell, measured in display columns so
    /// CJK text lines up.
    fn table(&self, table: &Table) -> String {
        let cell = |c: &[Inline]| self.inlines(c).replace('\n', " ");
        let header: Vec<String> = table.header.iter().map(|c| cell(c)).collect();
        let rows: Vec<Vec<String>> = table
            .rows
            .iter()
            .map(|r| r.iter().map(|c| cell(c)).collect())
            .collect();
        let cols = header
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
        let mut widths = vec![0usize; cols];
        for row in std::iter::once(&header).chain(rows.iter()) {
            for (i, c) in row.iter().enumerate() {
                widths[i] = widths[i].max(c.width());
            }
        }
        let line = |row: &[String]| {
            let cells: Vec<String> = (0..cols)
                .map(|i| {
                    let c = row.get(i).map(String::as_str).unwrap_or("");
                    format!("{c}{}", " ".repeat(widths[i] - c.width()))
                })
                .collect();
            cells.join("  ").trim_end().to_string()
        };
        let mut lines = Vec::new();
        if !header.is_empty() {
            lines.push(line(&header));
            lines.push(
                widths
                    .iter()
                    .map(|w| "-".repeat((*w).max(1)))
                    .collect::<Vec<_>>()
                    .join("  "),
            );
        }
        lines.extend(rows.iter().map(|r| line(r)));
        lines.join("\n")
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
            Inline::Text(t) | Inline::Code(t) | Inline::Math(t) => out.push_str(t),
            Inline::Emph(c) | Inline::Strong(c) | Inline::Strikethrough(c) => {
                out.push_str(&self.inlines(c))
            }
            Inline::Superscript(c) => out.push_str(&scripted(&self.inlines(c), SUPERSCRIPTS, '^')),
            Inline::Subscript(c) => out.push_str(&scripted(&self.inlines(c), SUBSCRIPTS, '_')),
            Inline::Link { url, content, .. } => {
                let text = self.inlines(content);
                let bare = url.strip_prefix("mailto:").unwrap_or(url);
                if text == *url || text == bare || url.starts_with('#') {
                    out.push_str(&text);
                } else {
                    out.push_str(&format!("{text} ({url})"));
                }
            }
            Inline::Image { alt, .. } if !alt.is_empty() => out.push_str(&format!("[{alt}]")),
            Inline::Image { .. } => out.push_str("[image]"),
            Inline::FootnoteRef(label) => match self.notes.number(label) {
                Some(n) => out.push_str(&format!("[{n}]")),
                None => out.push_str(&format!("[^{label}]")),
            },
            Inline::LineBreak => out.push('\n'),
            Inline::SoftBreak => out.push(' '),
            Inline::Raw { text, .. } => out.push_str(&read::html::text_of(text)),
        }
    }
}

const SUPERSCRIPTS: &[(char, char)] = &[
    ('0', '⁰'),
    ('1', '¹'),
    ('2', '²'),
    ('3', '³'),
    ('4', '⁴'),
    ('5', '⁵'),
    ('6', '⁶'),
    ('7', '⁷'),
    ('8', '⁸'),
    ('9', '⁹'),
    ('+', '⁺'),
    ('-', '⁻'),
    ('=', '⁼'),
    ('(', '⁽'),
    (')', '⁾'),
    ('n', 'ⁿ'),
    ('i', 'ⁱ'),
];
const SUBSCRIPTS: &[(char, char)] = &[
    ('0', '₀'),
    ('1', '₁'),
    ('2', '₂'),
    ('3', '₃'),
    ('4', '₄'),
    ('5', '₅'),
    ('6', '₆'),
    ('7', '₇'),
    ('8', '₈'),
    ('9', '₉'),
    ('+', '₊'),
    ('-', '₋'),
    ('=', '₌'),
    ('(', '₍'),
    (')', '₎'),
];

/// Super/subscript text in Unicode (x², H₂O) when every character has a
/// Unicode form, else marked up as `^(…)` / `_(…)` so the meaning survives.
fn scripted(text: &str, table: &[(char, char)], marker: char) -> String {
    let mapped: Option<String> = text
        .chars()
        .map(|c| table.iter().find(|(from, _)| *from == c).map(|(_, to)| *to))
        .collect();
    match mapped {
        Some(m) if !m.is_empty() => m,
        _ if text.chars().count() == 1 => format!("{marker}{text}"),
        _ => format!("{marker}({text})"),
    }
}

fn underline(text: &str, c: char) -> String {
    let width = text
        .lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
        .max(1);
    format!("{text}\n{}", c.to_string().repeat(width))
}

fn indent_all(text: &str, indent: &str) -> String {
    text.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{indent}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::read::markdown;

    #[test]
    fn writes_readable_plain_text() {
        let doc = markdown::read(
            "# Title\n\nSome **bold** text with a [link](https://a.org) and <https://b.org>[^1].\n\n\
             ## Sub\n\n- one\n- two\n  1. nested\n\n```\ncode\n```\n\n[^1]: The note.\n",
        );
        assert_eq!(
            write(&doc),
            "Title\n=====\n\nSome bold text with a link (https://a.org) and https://b.org[1].\n\n\
             Sub\n---\n\n- one\n- two\n  1. nested\n\n    code\n\n---\n\n[1] The note.\n"
        );
    }

    #[test]
    fn tables_align_by_display_width() {
        let doc = markdown::read("| 名稱 | n |\n|---|---|\n| 中文字 | 1 |\n| a | 22 |\n");
        assert_eq!(
            write(&doc),
            "名稱    n\n------  --\n中文字  1\na       22\n"
        );
    }

    #[test]
    fn scripts_use_unicode_when_possible() {
        let doc = markdown::read("H~2~O, x^2^, e^iπ^ and a~k~.\n");
        assert_eq!(write(&doc), "H₂O, x², e^(iπ) and a_k.\n");
    }

    #[test]
    fn title_is_not_repeated_when_the_document_opens_with_it() {
        let doc = markdown::read("---\ntitle: Same\n---\n\n# Same\n\nBody\n");
        assert_eq!(write(&doc), "Same\n====\n\nBody\n");
        let doc = markdown::read("---\ntitle: Report\n---\n\nBody\n");
        assert_eq!(write(&doc), "Report\n======\n\nBody\n");
    }

    #[test]
    fn raw_html_blocks_contribute_their_text() {
        let doc =
            markdown::read("<details><summary>More</summary><p>Hidden <b>text</b></p></details>\n");
        assert_eq!(write(&doc), "More\n\nHidden text\n");
    }

    #[test]
    fn mathml_without_tex_is_written_as_its_text() {
        let doc = Document {
            blocks: read::html::read_fragment(
                r#"<p>Before</p><math display="block"><mi>x</mi><mo>+</mo><mn>1</mn></math><p>y <math><mi>z</mi></math></p>"#,
            ),
            ..Document::default()
        };
        assert_eq!(write(&doc), "Before\n\nx+1\n\ny z\n");
        let doc = markdown::read("<div><math display=\"block\"><mi>q</mi></math></div>\n");
        assert_eq!(write(&doc), "q\n");
    }

    #[test]
    fn raw_html_is_dropped_but_its_text_kept() {
        let doc = markdown::read("H<sub>2</sub>O\n");
        assert_eq!(write(&doc), "H2O\n");
    }
}
