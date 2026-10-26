//! LaTeX → [`Document`] (#73): the document's body, as mitex-parser parses
//! it. It's given no commands and expands no macros, so it only splits the
//! source's own tokens and pairs braces and environments: which arguments a
//! command takes is the reader's to say, the parser's own table being
//! math's. `.tex` input still goes to pandoc until the reader reads what
//! pandoc does (see `graph::route_hop`).

use mitex_parser::CommandSpec;
use mitex_parser::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

use crate::converter::document::native::Warning;
use crate::converter::document::native::ast::{Block, Document, Inline};

/// The stack reading takes per level the parser and the reader recurse
/// into, in a debug build, which takes more than a release one: 553 bytes
/// for `{` and `\(`, 570 for `\begin`, 1114 for `^`, `_` and `'`, the
/// costliest (see the test `stack_per_level_is_measured`). This is well
/// over twice that.
const STACK_PER_LEVEL: usize = 4 << 10;

/// The stack reading takes besides, a shallow document's.
const STACK_BASE: usize = 1 << 20;

/// The most stack reading may take, address space it reserves rather than
/// memory it uses. A source that could need more, with more than 250,000
/// characters other than letters, digits and spaces, is read as plain
/// text, with a warning.
const MAX_STACK: usize = 1 << 30;

/// `source`, and, where it might nest too deep to parse, a warning that it
/// was read as plain text instead.
pub fn read(source: &str) -> (Document, Option<Warning>) {
    // The parser and the reader recurse into nested groups, environments,
    // math and scripts, as deep as they go, so reading gets a stack of its
    // own, as deep as `source` could need.
    let stack = STACK_BASE.saturating_add(nesting_bound(source).saturating_mul(STACK_PER_LEVEL));
    if stack <= MAX_STACK {
        let owned = source.to_string();
        let reading = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || read_parsed(&owned));
        if let Ok(reading) = reading {
            match reading.join() {
                Ok(doc) => return (doc, None),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    }
    let warning = Warning {
        one: "document nested too deeply to read was read as plain text",
        many: "documents nested too deeply to read were read as plain text",
        count: 1,
        line: None,
    };
    (super::plain::read(source), Some(warning))
}

/// How deep the parser and the reader might recurse into `source` at most:
/// its characters other than letters, digits and spaces. Each level they
/// nest, whatever opens it (`{`, `\begin`, `\(`, `^`, `'`), takes at least
/// one of them, so however the parser splits `source`, it can't nest
/// deeper.
fn nesting_bound(source: &str) -> usize {
    source
        .chars()
        .filter(|c| !c.is_alphanumeric() && !c.is_whitespace())
        .count()
}

fn read_parsed(source: &str) -> Document {
    // Expanding macros, which may never end, would also parse more than
    // `source` holds, past `nesting_bound`.
    let root = mitex_parser::parse_without_macro(source, CommandSpec::new(Default::default()));
    let mut reader = Reader::default();
    reader.flow(pieces(&document_body(&root).unwrap_or(root)));
    reader.end_paragraph();
    Document {
        blocks: reader.blocks,
        ..Document::default()
    }
}

/// The `document` environment, past the preamble; a fragment has none.
fn document_body(root: &SyntaxNode) -> Option<SyntaxNode> {
    root.descendants().find(|node| {
        node.kind() == SyntaxKind::ItemEnv
            && node
                .first_child()
                .and_then(|begin| begin.first_token())
                .is_some_and(|name| name.text() == "document")
    })
}

/// A node's child, its text split into its tokens, which an argument may be
/// one of.
#[derive(Clone)]
enum Piece {
    Token(SyntaxKind, String),
    Node(SyntaxNode),
}

fn pieces(node: &SyntaxNode) -> Vec<Piece> {
    let mut out = Vec::new();
    for item in node.children_with_tokens() {
        match item {
            SyntaxElement::Node(text) if text.kind() == SyntaxKind::ItemText => {
                out.extend(text.children_with_tokens().filter_map(|token| {
                    let token = token.into_token()?;
                    Some(Piece::Token(token.kind(), token.text().to_string()))
                }));
            }
            SyntaxElement::Node(node) => out.push(Piece::Node(node)),
            SyntaxElement::Token(token) => {
                out.push(Piece::Token(token.kind(), token.text().to_string()));
            }
        }
    }
    out
}

#[derive(Default)]
struct Reader {
    blocks: Vec<Block>,
    /// The paragraph being read.
    inlines: Vec<Inline>,
    /// What the line breaks since the last text make: nothing (0), a space
    /// (1) or, after an empty line, a new paragraph (2).
    breaks: usize,
    /// A comment runs to its line break, which then breaks nothing.
    in_comment: bool,
    /// At a line's start, whose spaces TeX skips: a line break there ends
    /// an empty line.
    line_start: bool,
    /// After a control word, whose following spaces TeX skips.
    skip_spaces: bool,
    /// Reading a command's argument, where an empty line is only a space.
    in_argument: bool,
    /// Whether the text being read is emphasized: it goes in a run of
    /// [`Inline::Emph`], which nests none.
    emphasized: bool,
}

impl Reader {
    fn flow(&mut self, mut items: Vec<Piece>) {
        let mut at = 0;
        while let Some(item) = items.get(at).cloned() {
            at += 1;
            match item {
                Piece::Token(kind, text) => self.token(kind, &text),
                Piece::Node(node) => match node.kind() {
                    SyntaxKind::ItemCmd => {
                        let name = command_name(&node);
                        let args = match signature(&name) {
                            Some(signature) => take_arguments(&mut items, &mut at, signature),
                            // Unknown: the groups right after it.
                            None => {
                                let mut groups = Vec::new();
                                while let Some(Piece::Node(group)) = items.get(at)
                                    && group.kind() == SyntaxKind::ItemCurly
                                {
                                    groups.push(pieces(group));
                                    at += 1;
                                }
                                groups
                            }
                        };
                        self.non_space();
                        let letters = &name[1..];
                        let control_word =
                            !letters.is_empty() && letters.chars().all(|c| c.is_ascii_alphabetic());
                        self.skip_spaces = control_word && args.is_empty();
                        self.command(&name, args);
                    }
                    // Begin and end name the environment, which is read for
                    // its content.
                    SyntaxKind::ItemBegin | SyntaxKind::ItemEnd => self.non_space(),
                    // `\iffalse … \fi` shows nothing, and `\fi` is a control
                    // word; so does mitex's own `\iftypst … \fi`.
                    SyntaxKind::ItemBlockComment | SyntaxKind::ItemTypstCode => {
                        self.non_space();
                        self.skip_spaces = true;
                    }
                    _ => self.flow(pieces(&node)),
                },
            }
        }
    }

    fn token(&mut self, kind: SyntaxKind, text: &str) {
        match kind {
            SyntaxKind::TokenLineBreak => {
                let mut lines = line_ends(text);
                if std::mem::take(&mut self.in_comment) {
                    lines -= 1;
                    self.line_start = true;
                }
                if lines > 0 {
                    self.breaks = if self.line_start || lines > 1 {
                        2
                    } else {
                        self.breaks.max(1)
                    };
                }
                self.line_start = true;
            }
            SyntaxKind::TokenComment => self.in_comment = true,
            SyntaxKind::TokenWhiteSpace if !self.line_start && !self.skip_spaces => self.space(),
            SyntaxKind::TokenWhiteSpace => {}
            // A brace shows nothing, but it's no space.
            SyntaxKind::TokenLBrace | SyntaxKind::TokenRBrace => self.non_space(),
            SyntaxKind::TokenTilde => self.text("\u{a0}"),
            _ => self.text(text),
        }
    }

    /// The command `name`, given its required arguments (see [`signature`]),
    /// or, unknown, the groups right after it.
    fn command(&mut self, name: &str, args: Vec<Vec<Piece>>) {
        match name {
            // `\emph` switches between italic and upright, so inside another
            // it's upright: the reader nests no emphasis, which writers
            // can't set apart from the one around it (#126). What's after
            // it reads on as after its argument: the spaces after a control
            // space there go.
            "\\emph" => {
                if let Some(arg) = args.into_iter().next() {
                    let in_argument = std::mem::replace(&mut self.in_argument, true);
                    self.emphasized = !self.emphasized;
                    self.flow(arg);
                    self.emphasized = !self.emphasized;
                    self.in_argument = in_argument;
                }
            }
            // TeX stops at a paragraph's end in an argument; it's a space,
            // and the spaces after it go as after any control word.
            "\\par" if self.in_argument => {
                self.space();
                self.skip_spaces = true;
            }
            "\\par" => self.end_paragraph(),
            // A control space (`\ `, or `\` at a line's end) is a space of
            // its own, and the spaces after it go.
            "\\" => {
                self.text(" ");
                self.skip_spaces = true;
            }
            // Characters LaTeX gives meanings, escaped.
            _ if ESCAPED.contains(&name) => self.text(&name[1..]),
            // Unknown commands keep their groups' text, as pandoc keeps it.
            _ => {
                for group in args {
                    self.flow(group);
                }
            }
        }
    }

    /// Before anything that isn't a space, shown or not (text, a brace, a
    /// command, an environment's begin or end): the spaces and line breaks
    /// before it settle, its line isn't empty, and a control word's spaces
    /// end. Every such thing comes through here.
    fn non_space(&mut self) {
        self.settle_breaks();
        self.line_start = false;
        self.skip_spaces = false;
    }

    /// `text`, emphasized in the run of emphasis at the end, or a new one.
    fn text(&mut self, text: &str) {
        self.non_space();
        if !self.emphasized {
            push_text(&mut self.inlines, text);
        } else if let Some(Inline::Emph(run)) = self.inlines.last_mut() {
            push_text(run, text);
        } else {
            self.inlines
                .push(Inline::Emph(vec![Inline::Text(text.into())]));
        }
    }

    /// A space, unless one is already there: TeX makes one of a run. One
    /// starting a paragraph goes, but not one starting an argument.
    fn space(&mut self) {
        self.settle_breaks();
        let last = match self.inlines.last() {
            Some(Inline::Emph(run)) => run.last(),
            last => last,
        };
        match last {
            Some(Inline::Text(last)) if last.ends_with(' ') => {}
            None if !self.in_argument => {}
            _ => self.text(" "),
        }
    }

    /// The line breaks before text: a space, or a new paragraph.
    fn settle_breaks(&mut self) {
        let breaks = std::mem::take(&mut self.breaks);
        let skip_spaces = std::mem::take(&mut self.skip_spaces);
        if breaks >= 2 && !self.in_argument {
            self.end_paragraph();
        } else if breaks > 0 && !skip_spaces {
            self.space();
        }
    }

    fn end_paragraph(&mut self) {
        self.breaks = 0;
        let inlines = trimmed(std::mem::take(&mut self.inlines));
        if !inlines.is_empty() {
            self.blocks.push(Block::Paragraph(inlines));
        }
    }
}

/// Characters LaTeX gives meanings, escaped: each shows its character.
const ESCAPED: [&str; 7] = ["\\%", "\\&", "\\#", "\\$", "\\_", "\\{", "\\}"];

/// What a command takes after its name, as LaTeX defines it: its required
/// arguments.
#[derive(Clone, Copy, Default)]
struct Signature {
    required: usize,
}

/// What a command the reader knows takes; `None` for one it doesn't, which
/// keeps the text of the groups right after it (see [`Reader::command`]).
/// Those that take nothing leave the groups after them to be read on.
fn signature(name: &str) -> Option<Signature> {
    match name {
        "\\emph" => Some(Signature { required: 1 }),
        "\\par" | "\\" => Some(Signature::default()),
        _ if ESCAPED.contains(&name) => Some(Signature::default()),
        _ => None,
    }
}

/// Takes from `items[*at..]` the arguments a command with `signature`
/// takes, as TeX takes them: each a group or a single token (a word's first
/// letter), looked for past spaces (see [`past_spaces`]). A missing one,
/// before an empty line or a group's end, ends the taking.
fn take_arguments(items: &mut Vec<Piece>, at: &mut usize, signature: Signature) -> Vec<Vec<Piece>> {
    let mut required = Vec::new();
    while required.len() < signature.required {
        let next = past_spaces(items, *at);
        // The parser joins a prime to what's before it (`It's`, `{B}'`),
        // wrapped as its argument: the argument here is the first of what
        // it joins, the rest read on.
        if let Some(Piece::Node(joined)) = items.get(next)
            && matches!(
                joined.kind(),
                SyntaxKind::ItemAttachComponent | SyntaxKind::ClauseArgument
            )
        {
            let parts = pieces(joined);
            items.splice(next..=next, parts);
            continue;
        }
        match items.get_mut(next) {
            Some(Piece::Node(group)) if group.kind() == SyntaxKind::ItemCurly => {
                required.push(pieces(group));
                *at = next + 1;
            }
            // A word's first letter, the rest of it left.
            Some(Piece::Token(SyntaxKind::TokenWord, word)) if word.chars().count() > 1 => {
                let first = word.remove(0);
                required.push(vec![Piece::Token(SyntaxKind::TokenWord, first.into())]);
                *at = next;
            }
            // None before an empty line or a group's end, where TeX would stop.
            None
            | Some(Piece::Token(
                SyntaxKind::TokenLineBreak
                | SyntaxKind::TokenWhiteSpace
                | SyntaxKind::TokenComment
                | SyntaxKind::TokenLBrace
                | SyntaxKind::TokenRBrace,
                _,
            )) => break,
            // Any other token, a character (`(`) or a command.
            Some(piece) => {
                required.push(vec![piece.clone()]);
                *at = next + 1;
            }
        }
    }
    required
}

/// Where what's after `items[from..]`'s spaces starts: past spaces, a
/// line's end and comments with the line ends they take, but not an empty
/// line, which ends a paragraph and which TeX looks no further past. Left
/// before one, its line breaks are read as they are.
fn past_spaces(items: &[Piece], from: usize) -> usize {
    let mut at = from;
    // Where the line that ended before `at` did, if one did.
    let mut line_end = None;
    loop {
        match items.get(at) {
            Some(Piece::Token(SyntaxKind::TokenWhiteSpace, _)) => at += 1,
            // A comment takes its line's end, so a line of one isn't empty.
            Some(Piece::Token(SyntaxKind::TokenComment, _)) => match items.get(at + 1) {
                Some(Piece::Token(SyntaxKind::TokenLineBreak, breaks))
                    if line_ends(breaks) == 1 =>
                {
                    line_end.get_or_insert(at);
                    at += 2;
                }
                Some(Piece::Token(SyntaxKind::TokenLineBreak, _)) => return line_end.unwrap_or(at),
                // At a group's end.
                _ => at += 1,
            },
            Some(Piece::Token(SyntaxKind::TokenLineBreak, breaks))
                if line_end.is_none() && line_ends(breaks) == 1 =>
            {
                line_end = Some(at);
                at += 1;
            }
            Some(Piece::Token(SyntaxKind::TokenLineBreak, _)) => return line_end.unwrap_or(at),
            _ => return at,
        }
    }
}

/// How many lines a run of line breaks ends: the parser gives one as a
/// token, and CR, LF and CRLF each end a line.
fn line_ends(breaks: &str) -> usize {
    breaks
        .replace("\r\n", "\n")
        .matches(['\n', '\r'])
        .count()
        .max(1)
}

fn command_name(command: &SyntaxNode) -> String {
    command
        .first_token()
        .map(|name| name.text().to_string())
        .unwrap_or_default()
}

/// Appends `text` to `inlines`, to the text at their end if there is one.
fn push_text(inlines: &mut Vec<Inline>, text: &str) {
    match inlines.last_mut() {
        Some(Inline::Text(last)) => last.push_str(text),
        _ => inlines.push(Inline::Text(text.to_string())),
    }
}

/// `inlines` without the spaces at their ends, a block's (`~` is kept).
fn trimmed(mut inlines: Vec<Inline>) -> Vec<Inline> {
    if let Some(Inline::Text(first)) = inlines.first_mut() {
        *first = first.trim_start_matches(' ').to_string();
    }
    if let Some(Inline::Text(last)) = inlines.last_mut() {
        *last = last.trim_end_matches(' ').to_string();
    }
    inlines.retain(|inline| !matches!(inline, Inline::Text(text) if text.is_empty()));
    inlines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Inline {
        Inline::Text(s.into())
    }

    /// The paragraphs `source` reads as, their text.
    fn paragraphs(source: &str) -> Vec<Vec<Inline>> {
        read(source)
            .0
            .blocks
            .into_iter()
            .map(|block| match block {
                Block::Paragraph(inlines) => inlines,
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_body_is_read_in_paragraphs() {
        let source = "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\
                      Some\n  text. % a comment\n\nSecond.\\par Third.\n\\end{document}\n";
        assert_eq!(
            paragraphs(source),
            [
                vec![text("Some text.")],
                vec![text("Second.")],
                vec![text("Third.")],
            ]
        );
        assert!(read(source).1.is_none());
    }

    /// Each row: what TeX makes of the source, as the reader reads it.
    #[test]
    fn spaces_comments_and_control_words_are_read_as_tex_reads_them() {
        let cases: &[(&str, &[&str])] = &[
            // A line's end is a space; an empty one, a comment line's
            // included, ends the paragraph.
            ("a\nb", &["a b"]),
            ("a\n  \nb", &["a", "b"]),
            ("a %c\n\nb", &["a", "b"]),
            ("a\n%c\nb", &["a b"]),
            ("a\r\n\r\nb\rc", &["a", "b c"]),
            // A run of spaces, a line's end among them, is one.
            ("a  \n  b", &["a b"]),
            // A group is its content, paragraphs and all.
            ("a{}b {x\n\ny}", &["ab x", "y"]),
            // A comment takes its line's end; the next line's spaces go, not
            // the space before it.
            ("a%comment\n  b %comment\n  c", &["ab c"]),
            ("a %c\nb", &["a b"]),
            ("before \\iffalse hidden\\fi after", &["before after"]),
            ("a\\iffalse x\\fi b", &["ab"]),
            // A control word takes the spaces after it, not a control symbol.
            ("a \\relax b", &["a b"]),
            ("a\\relax  b", &["ab"]),
            ("a\\relax\n\nb", &["a", "b"]),
            ("a\n\\relax b", &["a b"]),
            ("a\n\\relax\nb", &["a b"]),
            ("a\n\\begin{x}\nb\n\\end{x}\nc", &["a b c"]),
            ("a\\relax\\begin{x} b\\end{x}", &["a b"]),
            ("a\\begin{x}b\\relax\\end{x} c", &["ab c"]),
            // A group after it, `{}` too, ends it.
            ("a\\relax{} b", &["a b"]),
            ("a\\relax {} b", &["a b"]),
            ("a\n{}\nb", &["a b"]),
            // `\par` takes no group: it's the next paragraph's. Nor does an
            // escaped character or a control space.
            ("a\\par{b}", &["a", "b"]),
            ("a\\%{kept}b\\ {too}", &["a%keptb too"]),
            ("a~b", &["a\u{a0}b"]),
            ("a\\ b", &["a b"]),
            ("Dr.\\  Who", &["Dr. Who"]),
            ("a\\\nb", &["a b"]),
            // An escaped character is itself, and takes no spaces.
            (
                "50\\% off \\& \\#1, \\$2 \\{a\\_b\\}",
                &["50% off & #1, $2 {a_b}"],
            ),
            // An unknown command keeps its groups' text.
            ("Just \\unknown{kept}{too} text.", &["Just kepttoo text."]),
        ];
        for &(source, expected) in cases {
            let expected: Vec<Vec<Inline>> = expected.iter().map(|p| vec![text(p)]).collect();
            assert_eq!(paragraphs(source), expected, "{source}");
        }
    }

    fn emph(content: &[Inline]) -> Inline {
        Inline::Emph(content.to_vec())
    }

    /// Each row: what TeX makes of the source, as the reader reads it.
    #[test]
    fn arguments_are_taken_as_tex_takes_them() {
        let x = || emph(&[text("x")]);
        let cases: &[(&str, &[&[Inline]])] = &[
            // A group or one token, a word's first letter, after spaces, a
            // line's end or comments.
            ("A \\emph x B", &[&[text("A "), x(), text(" B")]]),
            ("\\emph xyz", &[&[x(), text("yz")]]),
            ("\\emph {x}", &[&[x()]]),
            ("\\emph\n  {x}", &[&[x()]]),
            ("\\emph%c\n%d\n {x}", &[&[x()]]),
            ("\\emph~", &[&[emph(&[text("\u{a0}")])]]),
            ("\\emph\\relax x", &[&[text("x")]]),
            ("\\emph(x)", &[&[emph(&[text("(")]), text("x)")]]),
            ("\\emph It's", &[&[emph(&[text("I")]), text("t's")]]),
            // Two side by side are one; one in another is upright.
            (
                "\\emph{x}\\emph{y} z",
                &[&[emph(&[text("xy")]), text(" z")]],
            ),
            (
                "\\emph{a \\emph{b} c}",
                &[&[emph(&[text("a ")]), text("b"), emph(&[text(" c")])]],
            ),
            ("\\emph{\\emph{x}}", &[&[text("x")]]),
            (
                "A\\emph{B}'s",
                &[&[text("A"), emph(&[text("B")]), text("'s")]],
            ),
            // The spaces after a control space in it go after it.
            (
                "A\\emph\\  B",
                &[&[text("A"), emph(&[text(" ")]), text("B")]],
            ),
            // One, the rest read on.
            ("\\emph{one}{two}", &[&[emph(&[text("one")]), text("two")]]),
            // Not past an empty line, a comment's line among them, or a
            // group's end.
            ("a\\emph\n\n{x}", &[&[text("a")], &[text("x")]]),
            ("a\\emph\n  \n{x}", &[&[text("a")], &[text("x")]]),
            ("a\\emph%c\n\n{x}", &[&[text("a")], &[text("x")]]),
            ("{a\\emph} b", &[&[text("a b")]]),
            // Inside one, spaces, `~` and a line's end are kept, and an
            // empty line or `\par`, which TeX stops at, is a space.
            (
                "a\\emph{ b }c",
                &[&[text("a"), emph(&[text(" b ")]), text("c")]],
            ),
            (
                "a\\emph{b\n}c",
                &[&[text("a"), emph(&[text("b ")]), text("c")]],
            ),
            (
                "a\\emph{b\n\nc}d",
                &[&[text("a"), emph(&[text("b c")]), text("d")]],
            ),
            (
                "a\\emph{b\\par  c}d",
                &[&[text("a"), emph(&[text("b c")]), text("d")]],
            ),
        ];
        for &(source, expected) in cases {
            let expected: Vec<Vec<Inline>> = expected.iter().map(|p| p.to_vec()).collect();
            assert_eq!(paragraphs(source), expected, "{source}");
        }
    }

    /// Emphases side by side and one in another come back from Markdown as
    /// they're read, the text and only the text.
    #[test]
    fn emphases_survive_markdown() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "\\emph{x}\\emph{y} z",
            "\\emph{a \\emph{b} c}",
            "\\emph{\\emph{x}}",
            "a\\emph{b}c",
        ] {
            let (doc, _) = read(source);
            let written = write::markdown::write(&doc);
            assert_eq!(
                markdown::read(&written).blocks,
                doc.blocks,
                "{source}: {written}"
            );
        }
    }

    /// Emphases nested 20,000 deep read flat, so the document is written
    /// and dropped on the stack conversions run on.
    #[test]
    fn deep_emphases_read_flat() {
        let source = |depth: usize| format!("{}x{}", "\\emph{".repeat(depth), "}".repeat(depth));
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                for (depth, expected) in [(20_000, text("x")), (20_001, emph(&[text("x")]))] {
                    // Dropped here, each in turn.
                    let (doc, warning) = read(&source(depth));
                    assert_eq!(doc.blocks, [Block::Paragraph(vec![expected])]);
                    assert!(warning.is_none());
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// Macros aren't expanded: one that expands forever doesn't hang it.
    #[test]
    fn a_macro_that_never_ends_is_read_as_written() {
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (doc, _) = read("\\newcommand{\\again}{\\again}\\again done");
            sent.send(doc).unwrap();
        });
        let doc = received
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("reading never ended");
        assert!(format!("{:?}", doc.blocks).contains("done"));
    }

    /// Punctuation counts, Unicode's too; letters, digits and spaces don't.
    #[test]
    fn the_nesting_bound_counts_what_is_no_word() {
        assert_eq!(nesting_bound("Ab 12 中文 é"), 0);
        // `{ } ^ ' _ \ ( … \ ) ，`
        assert_eq!(nesting_bound("{x}^'_\\(…\\)，"), 11);
    }

    #[test]
    fn deep_nesting_is_read_within_its_stack() {
        // A stack overflow aborts the process, so each runs in a child: as
        // `read` sizes its stack, and with half that, which keeps
        // `STACK_PER_LEVEL` twice what it takes. This deep, each needs more
        // than the 2 MiB conversions run on.
        const DEPTH: usize = 5000;
        let half = (STACK_BASE + DEPTH * STACK_PER_LEVEL) / 2;
        for construct in CONSTRUCTS {
            assert!(in_child(construct, DEPTH, None), "{construct}");
            assert!(in_child(construct, DEPTH, Some(half)), "{construct}");
        }
        // Past the most stack reading may take: plain text, with a warning.
        let source = format!("{}x", "{".repeat(MAX_STACK / STACK_PER_LEVEL));
        let (doc, warning) = read(&source);
        assert!(format!("{:?}", doc.blocks).contains('x'));
        assert_eq!(
            warning.unwrap().message(),
            "1 document nested too deeply to read was read as plain text"
        );
    }

    /// What the parser and the reader recurse into: groups, environments,
    /// `\(` math, and runs of scripts and primes.
    const CONSTRUCTS: [&str; 6] = ["{", "\\begin", "\\(", "^", "_", "'"];

    /// `construct` nested `depth` deep around an `x`.
    fn nested(construct: &str, depth: usize) -> String {
        let (open, close) = match construct {
            "{" => ("{", "}"),
            "\\begin" => ("\\begin{q}", "\\end{q}"),
            "\\(" => ("\\(", "\\)"),
            script => return format!("x{}", script.repeat(depth)),
        };
        format!("{}x{}", open.repeat(depth), close.repeat(depth))
    }

    /// Whether reading `construct` nested `depth` deep, on a thread with
    /// `stack` or as [`read`] sizes it, succeeds: in a child process, this
    /// test binary running [`nesting_child`] alone.
    fn in_child(construct: &str, depth: usize, stack: Option<usize>) -> bool {
        let stack = stack.map_or("read".to_string(), |stack| stack.to_string());
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "converter::document::native::read::latex::tests::nesting_child",
                "--exact",
                "-q",
            ])
            .env(CHILD, format!("{construct} {depth} {stack}"))
            .output()
            .unwrap()
            .status
            .success()
    }

    const CHILD: &str = "SWIFT_SHIFTER_LATEX_NESTING_CHILD";

    /// In a child of [`in_child`], reads what it's told to; otherwise
    /// nothing.
    #[test]
    fn nesting_child() {
        let Ok(task) = std::env::var(CHILD) else {
            return;
        };
        let [construct, depth, stack] = task.split(' ').collect::<Vec<_>>()[..] else {
            panic!("{task}");
        };
        let source = nested(construct, depth.parse().unwrap());
        let doc = match stack.parse::<usize>() {
            Ok(stack) => std::thread::Builder::new()
                .stack_size(stack)
                .spawn(move || read_parsed(&source))
                .unwrap()
                .join()
                .unwrap(),
            Err(_) => read(&source).0,
        };
        assert!(format!("{:?}", doc.blocks).contains('x'));
    }

    /// How [`STACK_PER_LEVEL`] is measured, in a debug build: for each
    /// construct, the least stack that reads it 5000 deep, found by halving
    /// in child processes, less what a shallow read takes, per level. The
    /// costliest, doubled, must fit. It takes a minute and a half, so it's
    /// run by hand, after mitex-parser or the reader's recursion changes:
    /// `cargo test stack_per_level -- --ignored --nocapture`.
    #[test]
    #[ignore = "slow: re-measures by hand"]
    fn stack_per_level_is_measured() {
        const DEPTH: usize = 5000;
        let least = |construct: &str, depth: usize| {
            let (mut fails, mut works) = (16 << 10, 1 << 30);
            while works - fails > 4 << 10 {
                let mid = (fails + works) / 2;
                if in_child(construct, depth, Some(mid)) {
                    works = mid;
                } else {
                    fails = mid;
                }
            }
            works
        };
        for construct in CONSTRUCTS {
            let per_level = (least(construct, DEPTH) - least(construct, 1)) / DEPTH;
            println!("{construct}: {per_level} bytes a level");
            assert!(2 * per_level <= STACK_PER_LEVEL, "{construct}: {per_level}");
        }
    }
}
