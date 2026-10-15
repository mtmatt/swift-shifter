//! Markdown (CommonMark + GFM) → [`Document`], parsed by comrak.

use comrak::nodes::{ListType, NodeValue, TableAlignment};
use comrak::{Arena, Node, Options, parse_document};

use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, ListItem, Meta, RawFormat, Table,
};

pub fn read(markdown: &str) -> Document {
    let markdown = strip_empty_front_matter(markdown);
    let arena = Arena::new();
    let root = parse_document(&arena, markdown, &options());
    let mut doc = Document::default();
    for node in root.children() {
        let front_matter = match &node.data().value {
            NodeValue::FrontMatter(raw) => Some(raw.clone()),
            _ => None,
        };
        match front_matter {
            Some(raw) => match parse_front_matter(&raw) {
                Some(meta) => doc.meta = meta,
                // `---` around ordinary text isn't metadata: keep the text,
                // with the rules it was written between.
                None => {
                    doc.blocks.push(Block::ThematicBreak);
                    doc.blocks.extend(read(front_matter_body(&raw)).blocks);
                    doc.blocks.push(Block::ThematicBreak);
                }
            },
            None => doc.blocks.extend(block(node)),
        }
    }
    doc
}

fn options() -> Options<'static> {
    let mut options = Options::default();
    let ext = &mut options.extension;
    ext.strikethrough = true;
    ext.table = true;
    ext.autolink = true;
    ext.tasklist = true;
    ext.superscript = true;
    ext.subscript = true;
    ext.footnotes = true;
    ext.math_dollars = true;
    ext.front_matter_delimiter = Some("---".to_string());
    // Emphasis next to CJK punctuation (e.g. **「強調」**) works as writers expect.
    ext.cjk_friendly_emphasis = true;
    options
}

/// Jekyll-style empty front matter (`---` then `---`): comrak would read it
/// as two thematic breaks.
fn strip_empty_front_matter(markdown: &str) -> &str {
    let mut lines = markdown.split_inclusive('\n');
    match (lines.next(), lines.next()) {
        (Some(first), Some(second)) if first.trim_end() == "---" && second.trim_end() == "---" => {
            &markdown[first.len() + second.len()..]
        }
        _ => markdown,
    }
}

fn front_matter_body(raw: &str) -> &str {
    raw.trim()
        .trim_start_matches("---")
        .trim_end_matches("---")
        .trim_end_matches("...")
}

/// Title, authors, lang, date and description from YAML front matter, or
/// `None` when it isn't a YAML mapping (then it's ordinary text).
fn parse_front_matter(raw: &str) -> Option<Meta> {
    let map = match serde_yaml::from_str(front_matter_body(raw)) {
        Ok(serde_yaml::Value::Mapping(map)) => map,
        // Empty front matter (`---` then `---`) is still front matter.
        Ok(serde_yaml::Value::Null) => return Some(Meta::default()),
        _ => return None,
    };
    let text = |key: &str| match map.get(key) {
        Some(serde_yaml::Value::String(s)) => Some(s.clone()),
        Some(serde_yaml::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let authors = match map.get("author").or_else(|| map.get("authors")) {
        Some(serde_yaml::Value::String(s)) => vec![s.clone()],
        Some(serde_yaml::Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    Some(Meta {
        title: text("title"),
        authors,
        lang: text("lang").or_else(|| text("language")),
        date: text("date"),
        description: text("description").or_else(|| text("abstract")),
    })
}

fn block<'a>(node: Node<'a>) -> Vec<Block> {
    let value = node.data().value.clone();
    match value {
        NodeValue::Paragraph => paragraph(node),
        NodeValue::Heading(h) => vec![Block::Heading {
            level: h.level,
            content: inlines(node),
        }],
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
            vec![Block::Quote(blocks(node))]
        }
        NodeValue::List(l) => vec![Block::List(List {
            start: (l.list_type == ListType::Ordered).then_some(l.start as u64),
            tight: l.tight,
            items: node.children().map(list_item).collect(),
        })],
        NodeValue::CodeBlock(c) => {
            let lang = c.info.split_whitespace().next().map(str::to_string);
            vec![Block::Code {
                lang,
                code: c.literal.trim_end_matches('\n').to_string(),
            }]
        }
        // Kept verbatim, as pandoc does: embeds, <details>, attributes and
        // the like have no place in the AST. Writers that can't emit HTML
        // read it for its text.
        NodeValue::HtmlBlock(h) => vec![Block::Raw {
            format: RawFormat::Html,
            text: h.literal.trim_end().to_string(),
        }],
        NodeValue::ThematicBreak => vec![Block::ThematicBreak],
        NodeValue::Table(t) => vec![Block::Table(table(node, &t.alignments))],
        NodeValue::FootnoteDefinition(f) => vec![Block::FootnoteDefinition {
            label: f.name,
            content: blocks(node),
        }],
        // Extensions we don't enable, or containers without their own meaning:
        // keep their content.
        _ => blocks(node),
    }
}

fn blocks<'a>(node: Node<'a>) -> Vec<Block> {
    node.children().flat_map(block).collect()
}

fn list_item<'a>(node: Node<'a>) -> ListItem {
    let task = match &node.data().value {
        NodeValue::TaskItem(t) => Some(t.symbol.is_some()),
        _ => None,
    };
    ListItem {
        task,
        content: blocks(node),
    }
}

fn table<'a>(node: Node<'a>, alignments: &[TableAlignment]) -> Table {
    let mut table = Table {
        align: alignments
            .iter()
            .map(|a| match a {
                TableAlignment::None => Align::Default,
                TableAlignment::Left => Align::Left,
                TableAlignment::Center => Align::Center,
                TableAlignment::Right => Align::Right,
            })
            .collect(),
        header: Vec::new(),
        rows: Vec::new(),
    };
    for row in node.children() {
        let is_header = matches!(row.data().value, NodeValue::TableRow(true));
        let cells: Vec<Vec<Inline>> = row.children().map(inlines).collect();
        if is_header {
            table.header = cells;
        } else {
            table.rows.push(cells);
        }
    }
    table
}

/// A paragraph, split around any `$$…$$` it contains: display math is a
/// block of its own even when written inside a paragraph.
fn paragraph<'a>(node: Node<'a>) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    for child in node.children() {
        let display_tex = match &child.data().value {
            NodeValue::Math(m) if m.display_math => Some(m.literal.clone()),
            _ => None,
        };
        match display_tex {
            Some(tex) => {
                push_paragraph(&mut blocks, std::mem::take(&mut current));
                blocks.push(Block::Math(tex));
            }
            None => push_inline(&mut current, child),
        }
    }
    push_paragraph(&mut blocks, current);
    blocks
}

/// Push a paragraph unless it holds only whitespace and breaks (what's left
/// around display math).
fn push_paragraph(blocks: &mut Vec<Block>, mut content: Vec<Inline>) {
    while matches!(content.first(), Some(Inline::SoftBreak | Inline::LineBreak)) {
        content.remove(0);
    }
    while matches!(content.last(), Some(Inline::SoftBreak | Inline::LineBreak)) {
        content.pop();
    }
    let blank = content
        .iter()
        .all(|i| matches!(i, Inline::Text(t) if t.trim().is_empty()));
    if !blank {
        blocks.push(Block::Paragraph(content));
    }
}

fn inlines<'a>(node: Node<'a>) -> Vec<Inline> {
    let mut out = Vec::new();
    for child in node.children() {
        push_inline(&mut out, child);
    }
    out
}

fn push_inline<'a>(out: &mut Vec<Inline>, node: Node<'a>) {
    let value = node.data().value.clone();
    let inline = match value {
        NodeValue::Text(t) => return push_text(out, &t),
        NodeValue::SoftBreak => Inline::SoftBreak,
        NodeValue::LineBreak => Inline::LineBreak,
        NodeValue::Code(c) => Inline::Code(c.literal),
        NodeValue::Math(m) => Inline::Math(m.literal),
        NodeValue::Emph => Inline::Emph(inlines(node)),
        NodeValue::Strong => Inline::Strong(inlines(node)),
        NodeValue::Strikethrough => Inline::Strikethrough(inlines(node)),
        NodeValue::Superscript => Inline::Superscript(inlines(node)),
        NodeValue::Subscript => Inline::Subscript(inlines(node)),
        NodeValue::Link(l) => Inline::Link {
            url: l.url,
            title: (!l.title.is_empty()).then_some(l.title),
            content: inlines(node),
        },
        NodeValue::Image(l) => Inline::Image {
            url: l.url,
            title: (!l.title.is_empty()).then_some(l.title),
            alt: crate::converter::document::native::ast::inline_text(&inlines(node)),
        },
        NodeValue::FootnoteReference(f) => Inline::FootnoteRef(f.name),
        NodeValue::HtmlInline(html) => match html_inline(&html) {
            Some(inline) => inline,
            None => return,
        },
        NodeValue::Raw(text) => return push_text(out, &text),
        _ => {
            for child in node.children() {
                push_inline(out, child);
            }
            return;
        }
    };
    out.push(inline);
}

/// `<br>` is a line break; HTML comments are dropped; any other inline tag is
/// kept as raw HTML for the writers that can use it.
fn html_inline(html: &str) -> Option<Inline> {
    let tag = html.trim().to_ascii_lowercase();
    if matches!(tag.as_str(), "<br>" | "<br/>" | "<br />") {
        return Some(Inline::LineBreak);
    }
    if tag.starts_with("<!--") {
        return None;
    }
    Some(Inline::Raw {
        format: RawFormat::Html,
        text: html.to_string(),
    })
}

/// Append text, merging with a preceding text node (comrak splits text at
/// every potential delimiter).
pub(crate) fn push_text(out: &mut Vec<Inline>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Inline::Text(prev)) = out.last_mut() {
        prev.push_str(text);
    } else {
        out.push(Inline::Text(text.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.to_string())
    }

    #[test]
    fn reads_headings_paragraphs_and_emphasis() {
        let doc = read("# Title\n\nSome *em* and **strong** text.\n");
        assert_eq!(
            doc.blocks,
            vec![
                Block::Heading {
                    level: 1,
                    content: vec![text("Title")]
                },
                Block::Paragraph(vec![
                    text("Some "),
                    Inline::Emph(vec![text("em")]),
                    text(" and "),
                    Inline::Strong(vec![text("strong")]),
                    text(" text."),
                ]),
            ]
        );
    }

    #[test]
    fn reads_front_matter_into_meta() {
        let doc = read("---\ntitle: My Doc\nauthor: [Ann, Bo]\nlang: zh-Hant\n---\n\nBody\n");
        assert_eq!(doc.meta.title.as_deref(), Some("My Doc"));
        assert_eq!(doc.meta.authors, ["Ann", "Bo"]);
        assert_eq!(doc.meta.lang.as_deref(), Some("zh-Hant"));
        assert_eq!(doc.blocks, vec![Block::Paragraph(vec![text("Body")])]);
    }

    #[test]
    fn reads_lists_and_tasks() {
        let doc = read("3. three\n4. four\n\n- [x] done\n- [ ] todo\n");
        let [Block::List(ordered), Block::List(tasks)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(ordered.start, Some(3));
        assert!(ordered.tight);
        assert_eq!(ordered.items.len(), 2);
        assert_eq!(tasks.start, None);
        assert_eq!(
            tasks.items.iter().map(|i| i.task).collect::<Vec<_>>(),
            [Some(true), Some(false)]
        );
    }

    #[test]
    fn reads_tables_with_alignment() {
        let doc = read("| a | b |\n|:--|--:|\n| 1 | 2 |\n");
        let [Block::Table(t)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(t.align, [Align::Left, Align::Right]);
        assert_eq!(t.header, vec![vec![text("a")], vec![text("b")]]);
        assert_eq!(t.rows, vec![vec![vec![text("1")], vec![text("2")]]]);
    }

    #[test]
    fn reads_math_code_and_footnotes() {
        let doc = read(
            "Inline $x^2$ here[^n].\n\n$$\\int f$$\n\n```rust\nfn main() {}\n```\n\n[^n]: A note.\n",
        );
        assert_eq!(
            doc.blocks,
            vec![
                Block::Paragraph(vec![
                    text("Inline "),
                    Inline::Math("x^2".into()),
                    text(" here"),
                    Inline::FootnoteRef("n".into()),
                    text("."),
                ]),
                Block::Math("\\int f".into()),
                Block::Code {
                    lang: Some("rust".into()),
                    code: "fn main() {}".into()
                },
                Block::FootnoteDefinition {
                    label: "n".into(),
                    content: vec![Block::Paragraph(vec![text("A note.")])]
                },
            ]
        );
    }

    #[test]
    fn br_tags_become_line_breaks() {
        // pymupdf4llm emits <br> inside table cells and paragraphs.
        let doc = read("one<br>two\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Paragraph(vec![
                text("one"),
                Inline::LineBreak,
                text("two")
            ])]
        );
    }

    #[test]
    fn html_blocks_are_kept_verbatim() {
        let doc = read("<div align=\"center\">\n<p>Hello</p>\n</div>\n");
        assert_eq!(
            doc.blocks,
            vec![Block::Raw {
                format: RawFormat::Html,
                text: "<div align=\"center\">\n<p>Hello</p>\n</div>".into()
            }]
        );
    }

    #[test]
    fn non_yaml_front_matter_stays_content() {
        let doc = read("---\nJust some text\n---\n\nAfter\n");
        assert_eq!(doc.meta, Meta::default());
        assert_eq!(
            doc.blocks,
            vec![
                Block::ThematicBreak,
                Block::Paragraph(vec![text("Just some text")]),
                Block::ThematicBreak,
                Block::Paragraph(vec![text("After")]),
            ]
        );
    }

    #[test]
    fn empty_front_matter_is_dropped() {
        let doc = read("---\n---\n\nBody\n");
        assert_eq!(doc.blocks, vec![Block::Paragraph(vec![text("Body")])]);
    }

    #[test]
    fn display_math_inside_a_paragraph_becomes_a_block() {
        let doc = read("Before\n$$x$$\nafter\n");
        assert_eq!(
            doc.blocks,
            vec![
                Block::Paragraph(vec![text("Before")]),
                Block::Math("x".into()),
                Block::Paragraph(vec![text("after")]),
            ]
        );
    }
}
