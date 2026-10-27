//! Whether a list the readers build is loose, for the readers that read
//! lists without blank lines to say so (LaTeX, Typst).

use crate::converter::document::native::ast::Block;

/// Whether a list item holding `content` makes its list loose: when one of
/// its blocks, written on the line after the one before it, as the writers
/// join a tight item's blocks, would run into it (see [`runs_into`]). Any
/// other blocks keep the list tight, as typst draws it. Blocks no writer
/// writes there (a note's, display math showing nothing) are skipped.
pub(super) fn loosens(content: &[Block]) -> bool {
    let written: Vec<&Block> = content
        .iter()
        .filter(|block| match block {
            Block::FootnoteDefinition { .. } => false,
            Block::Math(tex) => !tex.trim().is_empty(),
            _ => true,
        })
        .collect();
    written.windows(2).any(|pair| runs_into(pair[0], pair[1]))
}

/// Whether `next`, written on the line after `block`, runs into it, in
/// Markdown, the strictest of the writers. A paragraph takes a paragraph
/// after it as more of itself; it and display math, which is paragraph
/// text there, take a rule as a heading underline, and a list they can't
/// be interrupted by (an ordered one not from 1, or one opening with an
/// empty item) as text. A paragraph, display math or a table goes on the
/// table, quote or list item it follows, if that ends in a paragraph, and
/// a quote goes on a quote. Raw markup runs to the next blank line.
fn runs_into(block: &Block, next: &Block) -> bool {
    let goes_on = matches!(next, Block::Paragraph(_) | Block::Math(_) | Block::Table(_));
    match block {
        Block::Paragraph(_) | Block::Math(_) => match next {
            // Read back, display math is a block again.
            Block::Paragraph(_) => matches!(block, Block::Paragraph(_)),
            Block::ThematicBreak | Block::Raw { .. } => true,
            Block::List(list) => {
                list.start.is_some_and(|start| start != 1)
                    || list
                        .items
                        .first()
                        .is_none_or(|item| item.content.is_empty())
            }
            _ => false,
        },
        Block::Table(_) => goes_on,
        Block::Quote(children) => {
            matches!(next, Block::Quote(_)) || (goes_on && ends_in_paragraph(children))
        }
        Block::List(list) => {
            goes_on
                && list
                    .items
                    .last()
                    .is_some_and(|item| ends_in_paragraph(&item.content))
        }
        Block::Raw { .. } => true,
        Block::Heading { .. }
        | Block::Code { .. }
        | Block::ThematicBreak
        | Block::FootnoteDefinition { .. } => false,
    }
}

/// Whether `blocks` end in a paragraph, display math included, that the
/// lines after them may go on.
fn ends_in_paragraph(blocks: &[Block]) -> bool {
    match blocks.last() {
        Some(Block::Paragraph(_) | Block::Math(_)) => true,
        Some(Block::Quote(children)) => ends_in_paragraph(children),
        Some(Block::List(list)) => list
            .items
            .last()
            .is_some_and(|item| ends_in_paragraph(&item.content)),
        _ => false,
    }
}
