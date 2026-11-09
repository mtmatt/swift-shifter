//! How a document's structure changes when it's written as LaTeX and read
//! back. Each document is written as an outline, a line for each part of
//! its structure (see [`outline`]), and the outlines are compared line by
//! line, in order (see [`differences`]). The test
//! [`tests::fixtures_round_trip`] writes the Markdown fixtures as LaTeX,
//! reads them back, and checks their differences against
//! `tests/round-trip/baseline.txt`.

use crate::converter::document::native::ast::{Align, Block, Document, Inline};
use std::collections::HashMap;

/// `doc`'s structure: a line of its title, authors and date, then a line
/// for each block, indented two spaces for each block it's in, with its
/// inlines' text and their markup as Markdown writes it.
///
/// What LaTeX can't tell apart is written alike: whether a list is tight,
/// a soft break and a space, runs of spaces, a column aligned by default
/// and one aligned left (both `l`), and notes' labels, numbered in the
/// order they're first named. Links' and images' titles, which the writer
/// doesn't write, are left out.
pub fn outline(doc: &Document) -> Vec<String> {
    let meta = &doc.meta;
    let mut lines = vec![format!(
        "title {:?} authors {:?} date {:?}",
        meta.title, meta.authors, meta.date
    )];
    let mut notes = Notes::default();
    for block in &doc.blocks {
        block_lines(block, 0, &mut notes, &mut lines);
    }
    lines
}

/// Notes' numbers, by label, in the order they're first named.
#[derive(Default)]
struct Notes(HashMap<String, usize>);

impl Notes {
    fn number(&mut self, label: &str) -> usize {
        let next = self.0.len() + 1;
        *self.0.entry(label.to_string()).or_insert(next)
    }
}

/// Pushes `block`'s lines of an outline (see [`outline`]) to `lines`, it
/// being in `depth` blocks.
fn block_lines(block: &Block, depth: usize, notes: &mut Notes, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    let push = |lines: &mut Vec<String>, text: String| lines.push(format!("{indent}{text}"));
    match block {
        Block::Heading { level, content } => {
            let marks = "#".repeat(usize::from(*level));
            push(lines, format!("{marks} {}", inlines(content, notes)));
        }
        Block::Paragraph(content) => push(lines, inlines(content, notes)),
        Block::Quote(blocks) => {
            push(lines, ">".to_string());
            for block in blocks {
                block_lines(block, depth + 1, notes, lines);
            }
        }
        Block::List(list) => {
            for item in &list.items {
                let marker = match (list.start, item.task) {
                    (_, Some(done)) => format!("- [{}]", if done { "x" } else { " " }),
                    (Some(start), None) => format!("{start}."),
                    (None, None) => "-".to_string(),
                };
                push(lines, marker);
                for block in &item.content {
                    block_lines(block, depth + 1, notes, lines);
                }
            }
        }
        Block::Code { lang, code } => {
            let lang = lang.as_deref().unwrap_or("");
            push(lines, format!("```{lang} {:?}", code.trim_end()));
        }
        Block::Math(tex) => push(lines, format!("$$ {}", collapsed(tex))),
        Block::Table(table) => {
            let columns: String = table
                .align
                .iter()
                .map(|align| match align {
                    Align::Default | Align::Left => 'l',
                    Align::Center => 'c',
                    Align::Right => 'r',
                })
                .collect();
            push(lines, format!("| {columns}"));
            for row in std::iter::once(&table.header).chain(&table.rows) {
                let cells: Vec<String> = row.iter().map(|cell| inlines(cell, notes)).collect();
                push(lines, format!("| {} |", cells.join(" | ")));
            }
        }
        Block::ThematicBreak => push(lines, "---".to_string()),
        Block::FootnoteDefinition { label, content } => {
            push(lines, format!("[^{}]:", notes.number(label)));
            for block in content {
                block_lines(block, depth + 1, notes, lines);
            }
        }
        Block::Raw { text, .. } => push(lines, format!("raw {:?}", text.trim())),
    }
}

/// `content`'s text and markup, on one line (see [`outline`]).
fn inlines(content: &[Inline], notes: &mut Notes) -> String {
    let mut out = String::new();
    for inline in content {
        match inline {
            Inline::Text(text) => out.push_str(text),
            Inline::Emph(inner) => out.push_str(&format!("*{}*", inlines(inner, notes))),
            Inline::Strong(inner) => out.push_str(&format!("**{}**", inlines(inner, notes))),
            Inline::Strikethrough(inner) => out.push_str(&format!("~~{}~~", inlines(inner, notes))),
            Inline::Superscript(inner) => out.push_str(&format!("^{}^", inlines(inner, notes))),
            Inline::Subscript(inner) => out.push_str(&format!("~{}~", inlines(inner, notes))),
            Inline::Code(code) => out.push_str(&format!("`{code}`")),
            Inline::Math(tex) => out.push_str(&format!("${}$", collapsed(tex))),
            Inline::Link { url, content, .. } => {
                out.push_str(&format!("[{}]({url})", inlines(content, notes)));
            }
            Inline::Image { url, alt, .. } => out.push_str(&format!("![{alt}]({url})")),
            Inline::FootnoteRef(label) => out.push_str(&format!("[^{}]", notes.number(label))),
            Inline::LineBreak => out.push_str("\\\n"),
            Inline::SoftBreak => out.push(' '),
            Inline::Raw { text, .. } => out.push_str(&format!("raw{text:?}")),
        }
    }
    collapsed(&out)
}

/// `text` with each run of spaces and line ends one space, trimmed.
fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How `actual` differs from `expected`, in order: the lines of `expected`
/// not in their longest common subsequence prefixed `- `, and those of
/// `actual` prefixed `+ `, each where it is. A line moved, as a table's
/// header swapped with a row, is one of each. It takes time and memory as
/// the product of their lengths, which an outline's few hundred lines
/// keep small.
pub fn differences(expected: &[String], actual: &[String]) -> Vec<String> {
    let (n, m) = (expected.len(), actual.len());
    // `common[i][j]`: how long the longest common subsequence of
    // `expected[i..]` and `actual[j..]` is.
    let mut common = vec![vec![0_usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            common[i][j] = if expected[i] == actual[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n || j < m {
        if i < n && j < m && expected[i] == actual[j] {
            i += 1;
            j += 1;
        } else if j == m || (i < n && common[i + 1][j] >= common[i][j + 1]) {
            out.push(format!("- {}", expected[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", actual[j]));
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::read;
    use super::*;
    use crate::converter::document::native::{read::markdown, write};

    #[test]
    fn outlines_hold_the_structure() {
        let doc = markdown::read(
            "---\ntitle: T\n---\n\n# Title *one*\n\nSome `code`,\n$x$ and[^a] a [link](u).\n\n- [x] done\n- two\n\n3. three\n\n> quoted\n\n| a | b |\n|:-:|--:|\n| 1 | 2 |\n\n[^a]: Note.\n",
        );
        assert_eq!(
            outline(&doc),
            [
                "title Some(\"T\") authors [] date None",
                "# Title *one*",
                "Some `code`, $x$ and[^1] a [link](u).",
                "- [x]",
                "  done",
                "-",
                "  two",
                "3.",
                "  three",
                ">",
                "  quoted",
                "| cr",
                "| a | b |",
                "| 1 | 2 |",
                "[^1]:",
                "  Note.",
            ]
        );
    }

    #[test]
    fn differences_are_in_order() {
        let lines = |text: &str| text.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            differences(&lines("a\nb\nc"), &lines("a\nc\nd")),
            ["- b", "+ d"]
        );
        // A line swapped with another, and one more of a line, differ.
        assert_eq!(differences(&lines("h\nr"), &lines("r\nh")), ["- h", "+ h"]);
        assert_eq!(differences(&lines("a"), &lines("a\na")), ["+ a"]);
        assert!(differences(&lines("a\nb"), &lines("a\nb")).is_empty());
    }

    /// The baseline: each line a fixture's name and a difference its
    /// outline shows now after a LaTeX round trip, in order.
    fn baseline_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/round-trip/baseline.txt")
    }

    /// The Markdown fixtures, written as LaTeX and read back: their
    /// outlines' differences are printed, and must be the baseline's lines,
    /// in order.
    #[test]
    fn fixtures_round_trip() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs");
        let mut now = Vec::new();
        for name in ["kitchen-sink.md", "cjk.md", "pymupdf-output.md"] {
            let doc = markdown::read(&std::fs::read_to_string(fixtures.join(name)).unwrap());
            let (back, _) = read(&write::latex::write(&doc));
            for difference in differences(&outline(&doc), &outline(&back)) {
                now.push(format!("{name} {difference}"));
            }
        }
        for line in &now {
            println!("{line}");
        }
        let baseline = std::fs::read_to_string(baseline_path()).unwrap_or_default();
        let baseline: Vec<String> = baseline.lines().map(str::to_string).collect();
        let changed = differences(&baseline, &now);
        assert!(
            changed.is_empty(),
            "the baseline, {}, should be the lines printed: {changed:#?}",
            baseline_path().display()
        );
    }
}
