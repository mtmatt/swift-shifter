//! Plain text → [`Document`]. Text is taken literally: blank lines separate
//! paragraphs and single newlines are line breaks. (Pandoc has no plain-text
//! reader, so `.txt` used to be read as Markdown, which turned `*`, `_` and
//! leading `#` in ordinary text into formatting.)

use crate::converter::document::native::ast::{Block, Document, Inline};

pub fn read(text: &str) -> Document {
    let mut blocks = Vec::new();
    let mut lines: Vec<&str> = Vec::new();
    // A line holding only whitespace counts as blank.
    for line in text.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !lines.is_empty() {
                blocks.push(paragraph(&lines));
                lines.clear();
            }
        } else {
            lines.push(line.trim_end());
        }
    }
    Document {
        blocks,
        ..Document::default()
    }
}

fn paragraph(lines: &[&str]) -> Block {
    let mut inlines = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            inlines.push(Inline::LineBreak);
        }
        inlines.push(Inline::Text(line.to_string()));
    }
    Block::Paragraph(inlines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_characters_stay_literal() {
        let doc = read("# not a heading\n*not em* and 2 * 3\r\n\r\n   \n\nsecond");
        assert_eq!(
            doc.blocks,
            vec![
                Block::Paragraph(vec![
                    Inline::Text("# not a heading".into()),
                    Inline::LineBreak,
                    Inline::Text("*not em* and 2 * 3".into()),
                ]),
                Block::Paragraph(vec![Inline::Text("second".into())]),
            ]
        );
    }
}
