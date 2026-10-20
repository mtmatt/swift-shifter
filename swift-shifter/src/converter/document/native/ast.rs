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
    /// A cover image (path or URL), for ebook output.
    pub cover: Option<String>,
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
    /// Display math, as TeX (see [`Inline::Math`]).
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
    /// Inline math, as TeX. Its characters may be Unicode (`α`, `→`), as
    /// KaTeX, MathJax and unicode-math take them; the LaTeX writer names
    /// the ones LaTeX's own fonts lack.
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
    /// All visible text, for language detection: raw HTML's too, as the
    /// HTML reader reads it, which the writers show.
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
        // As the HTML reader reads it, which leaves out scripts and styles.
        Block::Raw { text, .. } => {
            for block in super::read::html::read_fragment(text) {
                match block {
                    // What it keeps raw (MathML without TeX): its text.
                    Block::Raw { text, .. } => {
                        out.push_str(&super::read::html::text_of(&text));
                        out.push('\n');
                    }
                    other => push_block_text(out, &other),
                }
            }
        }
        Block::Code { .. } | Block::Math(_) | Block::ThematicBreak => {}
    }
}

/// Appends TeX `tex` to `out`, with a space between them where `out` ends
/// in a control word like `\alpha` and `tex` starts with a letter, which
/// would run into it (`\alphax`). XeTeX and LuaTeX take any letter to be
/// part of a control word, not only an ASCII one (`\alpha中`).
pub fn push_tex(out: &mut String, tex: &str) {
    if ends_with_command_word(out) && tex.starts_with(char::is_alphabetic) {
        out.push(' ');
    }
    out.push_str(tex);
}

/// Whether `c` is a variation selector, which picks a glyph for the
/// character before it (`↪︎` as text, not an emoji, or `∪︀` with serifs).
/// TeX math has no such choice, and pulldown-latex shows it as a character
/// of its own.
pub fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

/// Whether TeX `tex` ends in a control word like `\alpha`, where a letter
/// can't follow without a space (see [`push_tex`]).
fn ends_with_command_word(tex: &str) -> bool {
    tex.rsplit_once('\\')
        .is_some_and(|(_, word)| !word.is_empty() && word.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Whether `text` contains CJK letters (ideographs, kana, Hangul). CJK
/// punctuation and fullwidth forms alone don't count: they turn up in
/// otherwise Latin text.
pub fn contains_cjk(text: &str) -> bool {
    text.chars().any(is_cjk_letter)
}

fn is_cjk_letter(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11FF}'     // Hangul Jamo
        | '\u{2E80}'..='\u{2FDF}'   // CJK and Kangxi radicals
        | '\u{3040}'..='\u{30FF}'   // Hiragana, Katakana
        | '\u{3100}'..='\u{31FF}'   // Bopomofo, Hangul compatibility Jamo, …
        | '\u{3400}'..='\u{4DBF}'   // CJK Unified Ideographs Extension A
        | '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{AC00}'..='\u{D7AF}'   // Hangul syllables
        | '\u{F900}'..='\u{FAFF}'   // CJK compatibility ideographs
        | '\u{20000}'..='\u{3134F}' // CJK Unified Ideographs Extensions B–G
    )
}

/// Common characters written differently in Traditional and Simplified
/// Chinese (paired by position), to tell the two apart.
const TRADITIONAL_ONLY: &str = "這們個來說對時會為國過還發後經點裡麼體與學實現無開關問長樣電話應書見務於當讓間灣臺從邊幾華認頭東車馬鳥魚內";
const SIMPLIFIED_ONLY: &str = "这们个来说对时会为国过还发后经点里么体与学实现无开关问长样电话应书见务于当让间湾台从边几华认头东车马鸟鱼内";

/// `zh-Hant`, `zh-Hans`, `ja` or `ko` when at least a third of the letters
/// are CJK. Japanese mixes kanji with kana, so it needs a real share of
/// kana (a stray Japanese phrase in Chinese text doesn't count); Hangul
/// means Korean; otherwise it's Chinese, told apart by script-specific
/// characters (Simplified when undecided).
pub fn detect_cjk_lang(text: &str) -> Option<&'static str> {
    let (mut letters, mut han, mut kana, mut hangul) = (0usize, 0usize, 0usize, 0usize);
    let (mut traditional, mut simplified) = (0usize, 0usize);
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        letters += 1;
        match c as u32 {
            0x3040..=0x30FF | 0x31F0..=0x31FF => kana += 1,
            0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => hangul += 1,
            0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0x20000..=0x2A6DF | 0xF900..=0xFAFF => {
                han += 1;
                traditional += usize::from(TRADITIONAL_ONLY.contains(c));
                simplified += usize::from(SIMPLIFIED_ONLY.contains(c));
            }
            _ => {}
        }
    }
    let cjk = han + kana + hangul;
    if letters == 0 || cjk * 3 < letters {
        return None;
    }
    Some(if kana * 5 >= cjk {
        "ja"
    } else if hangul > han {
        "ko"
    } else if traditional > simplified {
        "zh-Hant"
    } else {
        "zh-Hans"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_html_counts_toward_the_language() {
        let doc = Document {
            meta: Meta::default(),
            blocks: vec![
                Block::Raw {
                    format: RawFormat::Html,
                    text: "<p align=\"center\">中文標題，這是繁體中文的說明文件。</p>".into(),
                },
                Block::Paragraph(vec![Inline::Text("English body.".into())]),
            ],
        };
        assert_eq!(doc.effective_lang().as_deref(), Some("zh-Hant"));
        // A style, which no writer shows, doesn't count.
        let doc = Document {
            meta: Meta::default(),
            blocks: vec![
                Block::Paragraph(vec![Inline::Text(
                    "這是繁體中文的筆記，說明資料表與圖表的用法。".into(),
                )]),
                Block::Raw {
                    format: RawFormat::Html,
                    text: "<style>\n.dataframe tbody tr th { vertical-align: top; }\n\
                           .dataframe thead th { text-align: right; }\n</style>"
                        .into(),
                },
            ],
        };
        assert_eq!(doc.effective_lang().as_deref(), Some("zh-Hant"));
    }

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
        assert_eq!(detect_cjk_lang("這是一個中文句子"), Some("zh-Hant"));
        assert_eq!(detect_cjk_lang("这是一个中文句子"), Some("zh-Hans"));
        assert_eq!(
            detect_cjk_lang("中文"),
            Some("zh-Hans"),
            "undecided defaults to Simplified"
        );
        // A short Japanese phrase inside Chinese text stays Chinese.
        assert_eq!(
            detect_cjk_lang("這是一段很長的中文內容與說明，還有實際的例子。日本語の文"),
            Some("zh-Hant")
        );
        assert_eq!(detect_cjk_lang("これは日本語の文です"), Some("ja"));
        assert_eq!(detect_cjk_lang("이것은 한국어 문장입니다"), Some("ko"));
        assert_eq!(detect_cjk_lang("Plain English with one 字"), None);
        assert_eq!(detect_cjk_lang(""), None);
    }

    #[test]
    fn contains_cjk_ignores_stray_punctuation() {
        assert!(contains_cjk("Hello 世界"));
        assert!(contains_cjk("ひらがな"));
        assert!(contains_cjk("한국어"));
        assert!(!contains_cjk(
            "Plain text with fullwidth punctuation，and「quotes」"
        ));
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
