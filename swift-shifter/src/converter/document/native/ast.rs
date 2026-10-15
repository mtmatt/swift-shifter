//! The document model every native reader produces and every writer consumes.
//! Deliberately small: only what Swift Shifter's formats can express.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Document {
    pub meta: Meta,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// BCP 47 language tag, e.g. "en" or "zh-Hans".
    pub lang: Option<String>,
    pub date: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    Paragraph(Vec<Inline>),
    Quote(Vec<Block>),
    List(List),
    Code {
        /// Language from the info string or `language-x` class, if any.
        lang: Option<String>,
        code: String,
    },
    /// Display math, as TeX.
    Math(String),
    Table(Table),
    ThematicBreak,
    /// Footnote bodies keep their source label; writers renumber them.
    FootnoteDefinition {
        label: String,
        content: Vec<Block>,
    },
    /// Markup for one output format, kept verbatim (e.g. an HTML block in
    /// Markdown: embeds, `<details>`, attributes the AST can't model).
    Raw {
        format: RawFormat,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct List {
    /// `Some(start)` for an ordered list.
    pub start: Option<u64>,
    /// Tight lists render items without paragraph spacing.
    pub tight: bool,
    pub items: Vec<ListItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    /// `Some(done)` for a task-list item.
    pub task: Option<bool>,
    pub content: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub align: Vec<Align>,
    pub header: Vec<Vec<Inline>>,
    pub rows: Vec<Vec<Vec<Inline>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Default,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFormat {
    Html,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Emph(Vec<Inline>),
    Strong(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    Superscript(Vec<Inline>),
    Subscript(Vec<Inline>),
    Code(String),
    /// Inline math, as TeX.
    Math(String),
    Link {
        url: String,
        title: Option<String>,
        content: Vec<Inline>,
    },
    Image {
        url: String,
        title: Option<String>,
        alt: String,
    },
    FootnoteRef(String),
    /// A hard line break.
    LineBreak,
    /// A break that renders as a space (a newline inside a paragraph).
    SoftBreak,
    Raw {
        format: RawFormat,
        text: String,
    },
}

/// The visible text of a run of inlines, without markup: used for heading ids,
/// image alt text and plain-text output.
pub fn inline_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    push_inline_text(&mut out, inlines);
    out
}

fn push_inline_text(out: &mut String, inlines: &[Inline]) {
    for inline in inlines {
        match inline {
            Inline::Text(t) | Inline::Code(t) | Inline::Math(t) => out.push_str(t),
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strikethrough(c)
            | Inline::Superscript(c)
            | Inline::Subscript(c)
            | Inline::Link { content: c, .. } => push_inline_text(out, c),
            Inline::Image { alt, .. } => out.push_str(alt),
            Inline::LineBreak | Inline::SoftBreak => out.push(' '),
            Inline::FootnoteRef(_) | Inline::Raw { .. } => {}
        }
    }
}

impl Document {
    /// All visible text, for language detection.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            push_block_text(&mut out, block);
        }
        out
    }

    /// The language to declare in output: the document's own, or `zh`/`ja`/`ko`
    /// when its text is mostly CJK, so readers and browsers pick CJK fonts and
    /// line breaking.
    pub fn effective_lang(&self) -> Option<String> {
        if let Some(lang) = &self.meta.lang {
            return Some(lang.clone());
        }
        detect_cjk_lang(&self.plain_text()).map(str::to_string)
    }
}

fn push_block_text(out: &mut String, block: &Block) {
    match block {
        Block::Heading { content, .. } | Block::Paragraph(content) => {
            push_inline_text(out, content);
            out.push('\n');
        }
        Block::Quote(blocks)
        | Block::FootnoteDefinition {
            content: blocks, ..
        } => blocks.iter().for_each(|b| push_block_text(out, b)),
        Block::List(list) => list
            .items
            .iter()
            .flat_map(|item| &item.content)
            .for_each(|b| push_block_text(out, b)),
        Block::Table(table) => {
            for cell in table.header.iter().chain(table.rows.iter().flatten()) {
                push_inline_text(out, cell);
                out.push('\n');
            }
        }
        Block::Code { .. } | Block::Math(_) | Block::ThematicBreak | Block::Raw { .. } => {}
    }
}

/// `zh`, `ja` or `ko` when at least a third of the letters are CJK. Kana means
/// Japanese and Hangul means Korean; Han alone is taken as Chinese.
fn detect_cjk_lang(text: &str) -> Option<&'static str> {
    let (mut letters, mut han, mut kana, mut hangul) = (0usize, 0usize, 0usize, 0usize);
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        letters += 1;
        match c as u32 {
            0x3040..=0x30FF | 0x31F0..=0x31FF => kana += 1,
            0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => hangul += 1,
            0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0xF900..=0xFAFF => han += 1,
            _ => {}
        }
    }
    let cjk = han + kana + hangul;
    if letters == 0 || cjk * 3 < letters {
        return None;
    }
    Some(if kana > 0 {
        "ja"
    } else if hangul > han {
        "ko"
    } else {
        "zh"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_text_flattens_markup() {
        let inlines = vec![
            Inline::Text("a ".into()),
            Inline::Strong(vec![Inline::Emph(vec![Inline::Text("b".into())])]),
            Inline::LineBreak,
            Inline::Code("c".into()),
            Inline::FootnoteRef("1".into()),
        ];
        assert_eq!(inline_text(&inlines), "a b c");
    }

    #[test]
    fn detects_cjk_languages() {
        assert_eq!(detect_cjk_lang("這是一個中文句子"), Some("zh"));
        assert_eq!(detect_cjk_lang("これは日本語の文です"), Some("ja"));
        assert_eq!(detect_cjk_lang("이것은 한국어 문장입니다"), Some("ko"));
        assert_eq!(detect_cjk_lang("Plain English with one 字"), None);
        assert_eq!(detect_cjk_lang(""), None);
    }

    #[test]
    fn declared_lang_wins_over_detection() {
        let doc = Document {
            meta: Meta {
                lang: Some("en".into()),
                ..Meta::default()
            },
            blocks: vec![Block::Paragraph(vec![Inline::Text("中文".into())])],
        };
        assert_eq!(doc.effective_lang().as_deref(), Some("en"));
    }
}
