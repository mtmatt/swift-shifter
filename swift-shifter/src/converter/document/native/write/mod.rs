pub mod html;
pub mod markdown;
pub mod plain;

use std::collections::{HashMap, HashSet};

use crate::converter::document::native::ast::{Block, Document, Inline, inline_text};

/// Footnote numbers, 1, 2, … in reading order: notes referenced from the main
/// text first, then notes referenced only from inside earlier notes, each in
/// the order its reference is met.
pub(crate) struct Footnotes {
    numbers: HashMap<String, usize>,
}

impl Footnotes {
    /// The numbering, and each numbered note's body (body `i` is note `i + 1`).
    /// Definitions nobody references are dropped, like pandoc does.
    pub fn collect(doc: &Document) -> (Self, Vec<Vec<Block>>) {
        let mut definitions: HashMap<String, Vec<Block>> = HashMap::new();
        collect_definitions(&doc.blocks, &mut definitions);

        let mut numbers = HashMap::new();
        let mut bodies: Vec<Vec<Block>> = Vec::new();
        let mut number_refs = |refs: Vec<String>, bodies: &mut Vec<Vec<Block>>| {
            for label in refs {
                if numbers.contains_key(&label) {
                    continue;
                }
                if let Some(body) = definitions.remove(&label) {
                    numbers.insert(label, bodies.len() + 1);
                    bodies.push(body);
                }
            }
        };

        let mut refs = Vec::new();
        collect_refs(&doc.blocks, &mut refs);
        number_refs(refs, &mut bodies);
        // Worklist: each numbered note's body can number further notes,
        // which are appended and visited in turn.
        let mut next = 0;
        while next < bodies.len() {
            let mut refs = Vec::new();
            collect_refs(&bodies[next], &mut refs);
            number_refs(refs, &mut bodies);
            next += 1;
        }
        (Footnotes { numbers }, bodies)
    }

    /// The display number of a reference, or `None` for a reference with no
    /// definition (writers render it as literal text).
    pub fn number(&self, label: &str) -> Option<usize> {
        self.numbers.get(label).copied()
    }
}

fn collect_definitions(blocks: &[Block], out: &mut HashMap<String, Vec<Block>>) {
    for block in blocks {
        if let Block::FootnoteDefinition { label, content } = block {
            out.entry(label.clone()).or_insert_with(|| content.clone());
        }
        for_each_child_blocks(block, |children| collect_definitions(children, out));
    }
}

/// References in `blocks`, in reading order, skipping footnote definitions
/// (their references are collected when the note itself is numbered).
fn collect_refs(blocks: &[Block], out: &mut Vec<String>) {
    for block in blocks {
        if matches!(block, Block::FootnoteDefinition { .. }) {
            continue;
        }
        for_each_inlines(block, &mut |inlines| collect_inline_refs(inlines, out));
        for_each_child_blocks(block, |children| collect_refs(children, out));
    }
}

fn collect_inline_refs(inlines: &[Inline], out: &mut Vec<String>) {
    for inline in inlines {
        match inline {
            Inline::FootnoteRef(label) => out.push(label.clone()),
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strikethrough(c)
            | Inline::Superscript(c)
            | Inline::Subscript(c)
            | Inline::Link { content: c, .. } => collect_inline_refs(c, out),
            _ => {}
        }
    }
}

/// Calls `f` with each run of inlines held directly by `block`.
fn for_each_inlines(block: &Block, f: &mut dyn FnMut(&[Inline])) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph(content) => f(content),
        Block::Table(table) => table
            .header
            .iter()
            .chain(table.rows.iter().flatten())
            .for_each(|cell| f(cell)),
        _ => {}
    }
}

/// Calls `f` with each list of child blocks held by `block`.
fn for_each_child_blocks(block: &Block, mut f: impl FnMut(&[Block])) {
    match block {
        Block::Quote(children)
        | Block::FootnoteDefinition {
            content: children, ..
        } => f(children),
        Block::List(list) => list.items.iter().for_each(|item| f(&item.content)),
        _ => {}
    }
}

/// Rendering of block sequences, shared by the text writers.
pub(crate) trait BlockWriter {
    /// A block's text, or `None` for blocks written elsewhere (footnotes).
    fn block(&self, block: &Block) -> Option<String>;

    fn blocks(&self, blocks: &[Block]) -> String {
        self.blocks_joined(blocks, "\n\n")
    }

    /// Blocks separated by `sep`: a blank line normally, a single newline
    /// inside a tight list item (so a nested list doesn't make it loose).
    fn blocks_joined(&self, blocks: &[Block], sep: &str) -> String {
        blocks
            .iter()
            .filter_map(|b| self.block(b))
            .collect::<Vec<_>>()
            .join(sep)
    }
}

/// Indent every line after the first (list items, footnote bodies).
pub(crate) fn indent_rest(text: &str, indent: &str) -> String {
    let mut lines = text.lines();
    let mut out = lines.next().unwrap_or("").to_string();
    for line in lines {
        out.push('\n');
        if !line.is_empty() {
            out.push_str(indent);
            out.push_str(line);
        }
    }
    out
}

/// Unique, URL-friendly heading ids: lowercase letters and digits (any
/// script, so CJK headings keep their text), words joined by `-`.
#[derive(Default)]
pub(crate) struct HeadingIds {
    used: HashSet<String>,
}

impl HeadingIds {
    pub fn next(&mut self, content: &[Inline]) -> String {
        let mut slug = String::new();
        for c in inline_text(content).trim().chars() {
            if c.is_alphanumeric() {
                slug.extend(c.to_lowercase());
            } else if (c.is_whitespace() || c == '-' || c == '_') && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        let slug = slug.trim_matches('-');
        let base = if slug.is_empty() { "section" } else { slug };
        let mut id = base.to_string();
        let mut n = 1;
        while !self.used.insert(id.clone()) {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::read::markdown;

    #[test]
    fn footnotes_number_in_reference_order() {
        let doc = markdown::read(
            "B[^b] then A[^a] and B again[^b] and missing[^x].\n\n[^a]: A.\n[^b]: B.\n",
        );
        let (notes, bodies) = Footnotes::collect(&doc);
        assert_eq!(notes.number("b"), Some(1));
        assert_eq!(notes.number("a"), Some(2));
        assert_eq!(notes.number("x"), None);
        assert_eq!(bodies.len(), 2);
    }

    #[test]
    fn notes_referenced_inside_notes_number_in_reading_order() {
        // a's body refers to c, b's to d; definitions appear as b, a, c, d.
        let doc = markdown::read(
            "Refs[^a][^b].\n\n[^b]: B[^d].\n\n[^a]: A[^c].\n\n[^c]: C.\n\n[^d]: D.\n",
        );
        let (notes, bodies) = Footnotes::collect(&doc);
        let order: Vec<_> = ["a", "b", "c", "d"]
            .iter()
            .map(|l| notes.number(l))
            .collect();
        assert_eq!(order, [Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(bodies.len(), 4);
    }

    #[test]
    fn heading_ids_are_unique_and_keep_cjk() {
        let mut ids = HeadingIds::default();
        let text = |s: &str| vec![Inline::Text(s.into())];
        assert_eq!(ids.next(&text("Hello, World!")), "hello-world");
        assert_eq!(ids.next(&text("Hello world")), "hello-world-1");
        assert_eq!(ids.next(&text("中文 標題")), "中文-標題");
        assert_eq!(ids.next(&text("!!!")), "section");
    }
}
