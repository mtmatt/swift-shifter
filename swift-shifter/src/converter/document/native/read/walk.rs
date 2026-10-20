//! Walks over the blocks a reader built, for the readers that settle links
//! once the whole document is read (EPUB, Typst). Which blocks hold blocks
//! and which hold inlines is listed once here, with no catch-all arm, so a
//! new kind of container doesn't compile until every walk enters it.

use crate::converter::document::native::ast::{Block, Inline, List};

/// Calls `f` with every block in reading order, each before the blocks it
/// holds (in quotes, list items and notes).
pub(super) fn for_each_block(blocks: &mut [Block], f: &mut dyn FnMut(&mut Block)) {
    for block in blocks {
        f(block);
        for_each_child(block, &mut |children| for_each_block(children, f));
    }
}

/// Calls `f` with every paragraph-level run of inlines: each paragraph,
/// heading and table cell, in reading order.
pub(super) fn for_each_run(blocks: &mut [Block], f: &mut dyn FnMut(&mut Vec<Inline>)) {
    for_each_block(blocks, &mut |block| for_each_own_run(block, f));
}

/// Calls `f` with every run of inlines in `blocks`, each before the runs
/// nested in it (emphasis, link text).
pub(super) fn for_each_inlines(blocks: &mut [Block], f: &mut dyn FnMut(&mut Vec<Inline>)) {
    for_each_run(blocks, &mut |run| for_each_nested(run, f));
}

/// Settles the links a reader pointed at placeholders, URLs starting with
/// `marker`, anywhere in `blocks`. `resolve` gets the placeholder without
/// the marker and the link's text, and returns the slug of the heading the
/// link goes to; on `None` the link is replaced by its text, as `resolve`
/// left it.
pub(super) fn resolve_links(
    blocks: &mut [Block],
    marker: char,
    resolve: &mut dyn FnMut(&str, &mut Vec<Inline>) -> Option<String>,
) {
    for_each_inlines(blocks, &mut |inlines| {
        let mut i = 0;
        while i < inlines.len() {
            let Inline::Link { url, content, .. } = &mut inlines[i] else {
                i += 1;
                continue;
            };
            let Some(placeholder) = url.strip_prefix(marker) else {
                i += 1;
                continue;
            };
            if let Some(slug) = resolve(placeholder, content) {
                *url = format!("#{slug}");
                i += 1;
                continue;
            }
            let Inline::Link { content, .. } = inlines.remove(i) else {
                unreachable!("matched a link above");
            };
            // Not stepping past the link's text: it may hold placeholders
            // of its own.
            inlines.splice(i..i, content);
        }
    });
}

/// Calls `f` with `inlines` and then every run nested in it.
pub(super) fn for_each_nested(inlines: &mut Vec<Inline>, f: &mut dyn FnMut(&mut Vec<Inline>)) {
    f(inlines);
    for inline in inlines.iter_mut() {
        if let Some(run) = inner_run(inline) {
            for_each_nested(run, f);
        }
    }
}

/// Calls `f` with each list of blocks `block` holds.
fn for_each_child(block: &mut Block, f: &mut dyn FnMut(&mut [Block])) {
    match block {
        Block::Quote(children)
        | Block::FootnoteDefinition {
            content: children, ..
        } => f(children),
        Block::List(List { items, .. }) => {
            for item in items {
                f(&mut item.content);
            }
        }
        Block::Heading { .. }
        | Block::Paragraph(_)
        | Block::Code { .. }
        | Block::Math(_)
        | Block::Table(_)
        | Block::ThematicBreak
        | Block::Raw { .. } => {}
    }
}

/// Calls `f` with each run of inlines `block` holds itself, outside the
/// blocks it holds.
fn for_each_own_run(block: &mut Block, f: &mut dyn FnMut(&mut Vec<Inline>)) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph(content) => f(content),
        Block::Table(table) => {
            for cell in table
                .header
                .iter_mut()
                .chain(table.rows.iter_mut().flatten())
            {
                f(cell);
            }
        }
        Block::Quote(_)
        | Block::List(_)
        | Block::FootnoteDefinition { .. }
        | Block::Code { .. }
        | Block::Math(_)
        | Block::ThematicBreak
        | Block::Raw { .. } => {}
    }
}

/// The run of inlines `inline` holds, if any.
fn inner_run(inline: &mut Inline) -> Option<&mut Vec<Inline>> {
    match inline {
        Inline::Emph(c)
        | Inline::Strong(c)
        | Inline::Strikethrough(c)
        | Inline::Superscript(c)
        | Inline::Subscript(c)
        | Inline::Link { content: c, .. } => Some(c),
        Inline::Text(_)
        | Inline::Code(_)
        | Inline::Math(_)
        | Inline::Image { .. }
        | Inline::FootnoteRef(_)
        | Inline::LineBreak
        | Inline::SoftBreak
        | Inline::Raw { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::ast::{ListItem, Table};

    fn text(s: &str) -> Inline {
        Inline::Text(s.into())
    }

    fn heading(s: &str) -> Block {
        Block::Heading {
            level: 1,
            content: vec![text(s)],
        }
    }

    fn link(url: &str, content: Vec<Inline>) -> Inline {
        Inline::Link {
            url: url.into(),
            title: None,
            content,
        }
    }

    /// A heading and some text inside each kind of container, the text
    /// inside each kind of inline that holds inlines.
    fn every_container() -> Vec<Block> {
        let styled = vec![
            Inline::Emph(vec![text("emph")]),
            Inline::Strong(vec![text("strong")]),
            Inline::Strikethrough(vec![text("strike")]),
            Inline::Superscript(vec![text("super")]),
            Inline::Subscript(vec![text("sub")]),
            link("\u{1}link", vec![text("link")]),
        ];
        vec![
            Block::Quote(vec![heading("quote"), Block::Paragraph(styled)]),
            Block::List(List {
                start: None,
                tight: true,
                items: vec![ListItem {
                    task: None,
                    content: vec![heading("item")],
                }],
            }),
            Block::Table(Table {
                align: Vec::new(),
                header: vec![vec![text("header")]],
                rows: vec![vec![vec![link("\u{1}cell", vec![text("cell")])]]],
            }),
            Block::FootnoteDefinition {
                label: "n".into(),
                content: vec![heading("note")],
            },
        ]
    }

    /// One of the walks over runs of inlines.
    type RunWalk = fn(&mut [Block], &mut dyn FnMut(&mut Vec<Inline>));

    /// The text directly in each run `walk` visits.
    fn texts(walk: RunWalk) -> Vec<String> {
        let mut out = Vec::new();
        walk(&mut every_container(), &mut |run| {
            out.extend(run.iter().filter_map(|i| match i {
                Inline::Text(t) => Some(t.clone()),
                _ => None,
            }));
        });
        out
    }

    #[test]
    fn walks_reach_into_every_container() {
        assert_eq!(
            texts(for_each_inlines),
            [
                "quote", "emph", "strong", "strike", "super", "sub", "link", "item", "header",
                "cell", "note"
            ]
        );
        assert_eq!(texts(for_each_run), ["quote", "item", "header", "note"]);
        let mut blocks = 0;
        for_each_block(&mut every_container(), &mut |_| blocks += 1);
        assert_eq!(blocks, 8);
    }

    #[test]
    fn resolve_links_settles_placeholders_anywhere() {
        let mut blocks = every_container();
        resolve_links(&mut blocks, '\u{1}', &mut |placeholder, content| {
            if placeholder == "link" {
                return Some("slug".into());
            }
            content.push(text(" (unresolved)"));
            None
        });
        let Block::Quote(quote) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        let Block::Paragraph(styled) = &quote[1] else {
            panic!("{quote:?}");
        };
        assert_eq!(styled.last(), Some(&link("#slug", vec![text("link")])));
        let Block::Table(table) = &blocks[2] else {
            panic!("{blocks:?}");
        };
        assert_eq!(table.rows[0][0], [text("cell"), text(" (unresolved)")]);
    }
}
