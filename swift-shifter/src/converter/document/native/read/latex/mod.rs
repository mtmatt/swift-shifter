//! LaTeX → [`Document`] (#73): the document's body, as mitex-parser parses
//! it. It's given no commands and expands no macros, so it only splits the
//! source's own tokens and pairs braces and environments: which arguments a
//! command takes is the reader's to say, the parser's own table being
//! math's. Verbatim text is cut out before it's parsed (see [`verbatim`]).
//! `.tex` input still goes to pandoc until the reader reads what pandoc
//! does (see `graph::route_hop`).

mod conditionals;
pub mod files;
mod formulas;
pub mod paths;
pub mod stage;
mod verbatim;

use mitex_parser::CommandSpec;
use mitex_parser::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use verbatim::Verbatim;

use super::lists::loosens;
use crate::converter::document::native::Warning;
use crate::converter::document::native::ast::{Block, Document, Inline, List, ListItem};

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
/// was read as plain text instead, or one of what of it was (see
/// [`read_parsed`]).
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
                Ok(read) => return read,
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

/// `source`, parsed, and a warning of the lists nested too deep to keep
/// (see [`MAX_LIST_NESTING`]).
fn read_parsed(source: &str) -> (Document, Option<Warning>) {
    let source = conditionals::keep_else(source);
    let cut = verbatim::cut(&source);
    // Expanding macros, which may never end, would also parse more than
    // `source` holds, past `nesting_bound`.
    let closed = formulas::close_scripts(&cut.source);
    let separated = formulas::separate_formulas(&closed);
    let root = mitex_parser::parse_without_macro(&separated, CommandSpec::new(Default::default()));
    let mut reader = Reader {
        verbatims: cut.verbatims,
        verbatim_mark: cut.mark,
        ..Reader::default()
    };
    reader.flow(pieces(&document_body(&root).unwrap_or(root)));
    reader.end_paragraph();
    level_headings(&mut reader.blocks);
    let warning = (reader.unlisted > 0).then_some(Warning {
        one: "list or quote nested too deeply was read as its text",
        many: "lists or quotes nested too deeply were read as their text",
        count: reader.unlisted,
        line: None,
    });
    let doc = Document {
        blocks: reader.blocks,
        ..Document::default()
    };
    (doc, warning)
}

/// The `document` environment, past the preamble; a fragment has none.
fn document_body(root: &SyntaxNode) -> Option<SyntaxNode> {
    root.descendants()
        .find(|node| node.kind() == SyntaxKind::ItemEnv && environment_name(node) == "document")
}

/// What a list's environment holds (see [`gather`]).
#[derive(Default)]
struct Gathered {
    /// Whether `\tightlist` makes it tight.
    tight: bool,
    /// The number `start=` in its options gives its first item (enumitem's
    /// `\begin{enumerate}[start=3]`).
    start: Option<i64>,
    /// What's before the first `\item`; with none, all it holds.
    before: Vec<Piece>,
    items: Vec<Item>,
}

/// An `\item` of a list's own (see [`gather`]).
struct Item {
    /// Its task mark (see [`task`]).
    task: Option<bool>,
    /// Whether a label takes the place of its number.
    labelled: bool,
    /// Its label, which isn't a task's: it leads its text.
    label: Option<Vec<Piece>>,
    /// What's up to the next.
    content: Vec<Piece>,
}

/// The environments that are lists.
#[derive(Clone, Copy, PartialEq)]
enum ListKind {
    Itemize,
    Enumerate,
    Description,
}

/// The `\item`s of `items`, a list's environment's, each what's up to the
/// next. Only those of the environment's own: one in a group or another
/// environment (`{\item a}`, `samepage`) is in its item's text, the group
/// or environment left whole, so nothing in it is lost and a command keeps
/// its argument. `\item[$\boxtimes$]` and `\item[$\square$]` make tasks,
/// done or not; any other label leads its item's text. `\tightlist` before
/// the first item makes it tight, as the LaTeX writer writes it. Options
/// after `\begin` are enumitem's: of those, only `start=` shows, in a
/// writer's numbers, as no writer takes a list's label or spacing. Those
/// that run code are left to be read, so that what they do isn't lost.
fn gather(mut items: Vec<Piece>) -> Gathered {
    let mut gathered = Gathered::default();
    let (mut at, mut unbracketed) = (0, 0);
    if matches!(items.first(), Some(Piece::Node(node)) if node.kind() == SyntaxKind::ItemBegin) {
        at = 1;
        let options = take_arguments(&mut items, &mut at, &mut unbracketed, ITEM).optional;
        let text = options.as_deref().map(options_text).unwrap_or_default();
        if options_run_code(&text) {
            at = 1;
        } else {
            gathered.start = list_start(&text);
        }
    }
    while let Some(piece) = items.get(at).cloned() {
        at += 1;
        let name = match &piece {
            Piece::Node(node) if node.kind() == SyntaxKind::ItemCmd => command_name(node),
            _ => String::new(),
        };
        match name.as_str() {
            "\\item" => {
                let label = take_arguments(&mut items, &mut at, &mut unbracketed, ITEM).optional;
                let labelled = label.is_some();
                let task = label.as_deref().and_then(task);
                gathered.items.push(Item {
                    task,
                    labelled,
                    label: label.filter(|_| task.is_none()),
                    content: Vec::new(),
                });
            }
            "\\tightlist" if gathered.items.is_empty() => gathered.tight = true,
            _ => match gathered.items.last_mut() {
                Some(item) => item.content.push(piece),
                None => gathered.before.push(piece),
            },
        }
    }
    gathered
}

/// The text of `options`, a list's, without what's in a group, which is an
/// option's value and may hold anything.
fn options_text(options: &[Piece]) -> String {
    let outside_groups: Vec<Piece> = options
        .iter()
        .filter(|piece| !matches!(piece, Piece::Node(node) if node.kind() == SyntaxKind::ItemCurly))
        .cloned()
        .collect();
    source_text(&outside_groups)
}

/// Whether `options`, as [`options_text`] gives them, run code when the
/// list starts or ends: `before`, `first` and `after`, with their `*` and
/// `!` forms.
fn options_run_code(options: &str) -> bool {
    options.split(',').any(|option| {
        let key = option.split('=').next().unwrap_or_default().trim();
        ["before", "first", "after"]
            .iter()
            .any(|hook| key.starts_with(hook))
    })
}

/// The value of the last `start=` in `options`, as [`options_text`] gives
/// them, which overrides any before it. With no value, it's 1.
fn list_start(options: &str) -> Option<i64> {
    let start = options
        .rsplit(',')
        .find(|option| option.split('=').next().unwrap_or_default().trim() == "start")?;
    match start.split_once('=') {
        Some((_, value)) => value.trim().parse().ok(),
        None => Some(1),
    }
}

/// Whether `pieces`, an environment's, hold a `\rule` and nothing else.
fn is_a_rule(pieces: &[Piece]) -> bool {
    let mut content: Vec<Piece> = pieces
        .iter()
        .filter(|piece| match piece {
            Piece::Token(kind, _) => !matches!(
                kind,
                SyntaxKind::TokenWhiteSpace | SyntaxKind::TokenLineBreak | SyntaxKind::TokenComment
            ),
            Piece::Node(node) => {
                !matches!(node.kind(), SyntaxKind::ItemBegin | SyntaxKind::ItemEnd)
            }
        })
        .cloned()
        .collect();
    let is_rule = matches!(content.first(), Some(Piece::Node(node))
        if node.kind() == SyntaxKind::ItemCmd && command_name(node) == "\\rule");
    let (mut at, mut unbracketed) = (1, 0);
    is_rule && {
        take_arguments(&mut content, &mut at, &mut unbracketed, RULE);
        at == content.len()
    }
}

/// Takes the box leading `content`, a numbered item's, as the LaTeX writer
/// writes a numbered task, and gives whether it's done.
fn leading_box(content: &mut Vec<Piece>) -> Option<bool> {
    let done = match content.first()? {
        Piece::Node(node) if node.kind() == SyntaxKind::ItemFormula => task(&content[..1])?,
        _ => return None,
    };
    let text = content
        .iter()
        .skip(1)
        .position(|piece| !matches!(piece, Piece::Token(SyntaxKind::TokenWhiteSpace, _)))
        .map_or(content.len(), |spaces| spaces + 1);
    content.drain(..text);
    Some(done)
}

/// Whether `label`, an `\item`'s, marks a task done or not: as the LaTeX
/// writer marks them (`$\boxtimes$`, `$\square$`), or as their characters
/// (`☒`, `☐`).
fn task(label: &[Piece]) -> Option<bool> {
    let label: String = source_text(label)
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '$')
        .collect();
    match label.as_str() {
        "\\boxtimes" | "☒" => Some(true),
        "\\square" | "☐" => Some(false),
        _ => None,
    }
}

/// The source `pieces` were parsed from, without the braces of the group
/// they're an argument's (see [`take_arguments`]), or comments, with the
/// line's end and the next line's spaces each one takes: the letters after
/// a comment stay apart from a control word before it.
fn source_text(pieces: &[Piece]) -> String {
    let mut tokens = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Token(SyntaxKind::TokenLBrace | SyntaxKind::TokenRBrace, _) => {}
            Piece::Token(kind, token) => tokens.push((*kind, token.clone())),
            Piece::Node(node) => push_source_tokens(node, &mut tokens),
        }
    }
    let mut text = String::new();
    let mut after_comment = false;
    let mut after_control_word = false;
    for (kind, token) in tokens {
        match kind {
            SyntaxKind::TokenComment => after_comment = true,
            SyntaxKind::TokenLineBreak | SyntaxKind::TokenWhiteSpace if after_comment => {}
            _ => {
                let letter = token.starts_with(|c: char| c.is_ascii_alphabetic());
                if std::mem::take(&mut after_comment) && letter && after_control_word {
                    text.push(' ');
                }
                // `\langle` after `\left` is a symbol to the parser.
                after_control_word = matches!(
                    kind,
                    SyntaxKind::ClauseCommandName | SyntaxKind::TokenCommandSym
                ) && token.ends_with(|c: char| c.is_ascii_alphabetic());
                text.push_str(&token);
            }
        }
    }
    text
}

/// Appends the tokens of `node`'s source to `tokens`. The tree keeps only
/// the name of an environment's `\begin` and `\end`. What `\iffalse` hides
/// shows nothing, and takes the spaces after its `\fi`, as a comment does.
fn push_source_tokens(node: &SyntaxNode, tokens: &mut Vec<(SyntaxKind, String)>) {
    let delimiter = match node.kind() {
        SyntaxKind::ItemBegin => Some("begin"),
        SyntaxKind::ItemEnd => Some("end"),
        SyntaxKind::ItemBlockComment | SyntaxKind::ItemTypstCode => {
            tokens.push((SyntaxKind::TokenComment, String::new()));
            return;
        }
        _ => None,
    };
    if let Some(delimiter) = delimiter {
        let name = node.text();
        tokens.push((SyntaxKind::TokenWord, format!("\\{delimiter}{{{name}}}")));
        return;
    }
    for element in node.children_with_tokens() {
        match element {
            SyntaxElement::Node(node) => push_source_tokens(&node, tokens),
            SyntaxElement::Token(token) => {
                tokens.push((token.kind(), token.text().to_string()));
            }
        }
    }
}

/// Takes from `items[*at..]` what the definition command `name` defines:
/// what it holds is only read where it's called, which the reader doesn't
/// do yet (#73), and its text doesn't show here.
fn skip_definition(items: &mut [Piece], at: &mut usize, unbracketed: &mut usize, name: &str) {
    let required = Signature {
        required: 1,
        ..Signature::default()
    };
    let name_of_it = Signature {
        star: true,
        ..required
    };
    take_arguments(items, at, unbracketed, name_of_it);
    if name.ends_with("command") {
        // Its count of arguments, and the default of the first.
        for _ in 0..2 {
            let optional = Signature {
                optional: true,
                ..Signature::default()
            };
            take_arguments(items, at, unbracketed, optional);
        }
    } else {
        // `\def`'s arguments, up to its group, as TeX takes them. With none
        // in sight, past a `$` that opened math in the parser, say, what's
        // after it is read on, so that nothing is lost.
        let text = items[*at..]
            .iter()
            .take(MAX_DEFINITION_PARAMETERS)
            .position(|piece| match piece {
                Piece::Token(..) => false,
                Piece::Node(node) => node.kind() != SyntaxKind::ItemCmd,
            });
        match text.map(|text| (text, &items[*at + text])) {
            Some((text, Piece::Node(node))) if node.kind() == SyntaxKind::ItemCurly => *at += text,
            _ => return,
        }
    }
    take_arguments(items, at, unbracketed, required);
}

/// The name of `env`, an environment: what its `\begin` names.
fn environment_name(env: &SyntaxNode) -> String {
    env.first_child()
        .and_then(|begin| begin.first_token())
        .map(|name| name.text().to_string())
        .unwrap_or_default()
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
    push_pieces(node, &mut out);
    out
}

/// Appends `node`'s children to `out`, those that only group what they
/// hold taken apart: text into its tokens, and a prime joined to what's
/// before it (`It's`, `{B}'`, `]'`) into what it joins, so that an argument
/// is only the first of them.
fn push_pieces(node: &SyntaxNode, out: &mut Vec<Piece>) {
    for item in node.children_with_tokens() {
        match item {
            SyntaxElement::Node(node)
                if matches!(
                    node.kind(),
                    SyntaxKind::ItemText
                        | SyntaxKind::ItemAttachComponent
                        | SyntaxKind::ClauseArgument
                ) =>
            {
                push_pieces(&node, out);
            }
            SyntaxElement::Node(node) => out.push(Piece::Node(node)),
            SyntaxElement::Token(token) => {
                out.push(Piece::Token(token.kind(), token.text().to_string()));
            }
        }
    }
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
    /// Whether it's bold: it goes in a run of [`Inline::Strong`], which
    /// nests none either and holds the runs of emphasis.
    bold: bool,
    /// The verbatim text cut out of the source, which placeholders stand
    /// for (see [`verbatim::cut`]).
    verbatims: Vec<Verbatim>,
    /// The command a placeholder is (see [`verbatim::Cut::mark`]).
    verbatim_mark: String,
    /// How many lists the text being read is in.
    lists: usize,
    /// The counters of the `enumerate`s the text being read is in, the
    /// outermost's first: their last item's number. One `\setcounter` sets
    /// them wherever it's read, as LaTeX sets them for the whole document.
    enumerations: Vec<i64>,
    /// The lists past [`MAX_LIST_NESTING`], read as their text.
    unlisted: usize,
}

/// How deep lists nest, past which one is read as its items' text: as deep
/// as documents nest them, and no deeper than writing the document, or
/// dropping it, recurses on the stack conversions run on.
const MAX_LIST_NESTING: usize = 128;

impl Reader {
    fn flow(&mut self, mut items: Vec<Piece>) {
        let mut at = 0;
        // How long a tail of `items` holds no `]` (see `take_arguments`).
        let mut unbracketed = 0;
        while let Some(item) = items.get(at).cloned() {
            at += 1;
            match item {
                // `\\`, a line break: its `*` and `[…]` say how, which the
                // document has no say in, and the spaces after it go.
                Piece::Token(SyntaxKind::ItemNewLine, _) => {
                    take_arguments(&mut items, &mut at, &mut unbracketed, LINE_BREAK);
                    self.inline(Inline::LineBreak);
                    self.skip_spaces = true;
                }
                Piece::Token(kind, text) => self.token(kind, &text),
                Piece::Node(node) => match node.kind() {
                    SyntaxKind::ItemCmd if DEFINITIONS.contains(&command_name(&node).as_str()) => {
                        let name = command_name(&node);
                        skip_definition(&mut items, &mut at, &mut unbracketed, &name);
                        self.non_space();
                    }
                    SyntaxKind::ItemCmd => {
                        let name = command_name(&node);
                        let signature = if name == self.verbatim_mark {
                            Some(PLACEHOLDER)
                        } else {
                            signature(&name)
                        };
                        let args = match signature {
                            Some(signature) => {
                                take_arguments(&mut items, &mut at, &mut unbracketed, signature)
                                    .required
                            }
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
                    SyntaxKind::ItemEnv => match environment_name(&node).as_str() {
                        "itemize" => self.list(&node, ListKind::Itemize),
                        "enumerate" => self.list(&node, ListKind::Enumerate),
                        "description" => self.list(&node, ListKind::Description),
                        "quote" | "quotation" | "verse" => self.quote(&node),
                        "center" | "flushleft" | "flushright" => self.aligned(&node),
                        name @ ("math" | "displaymath") => {
                            let mut inside = pieces(&node);
                            inside.retain(|piece| {
                                !matches!(piece, Piece::Node(node)
                                    if matches!(node.kind(), SyntaxKind::ItemBegin | SyntaxKind::ItemEnd))
                            });
                            self.math(&source_text(&inside), name == "displaymath");
                        }
                        name if MATH_ENVIRONMENTS.contains(&name.trim_end_matches('*')) => {
                            self.math(&source_text(&[Piece::Node(node.clone())]), true);
                        }
                        _ => self.flow(pieces(&node)),
                    },
                    SyntaxKind::ItemFormula => self.formula(&node),
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
        if let Some(level) = SECTIONS.iter().position(|section| *section == name) {
            return self.heading(level, args);
        }
        if name == self.verbatim_mark {
            return self.verbatim(args);
        }
        if let Some((bold, emphasized)) = font(name, self.bold, self.emphasized) {
            // What's after it reads on as after its argument: the spaces
            // after a control space there go.
            if let Some(arg) = args.into_iter().next() {
                self.in_font(arg, bold, emphasized);
            }
            return;
        }
        match name {
            // TeX stops at a paragraph's end in an argument; it's a space,
            // and the spaces after it go as after any control word.
            "\\par" if self.in_argument => {
                self.space();
                self.skip_spaces = true;
            }
            "\\par" => self.end_paragraph(),
            // What a list starts with (see `Reader::list`): elsewhere, it
            // shows nothing.
            // A line, which no writer's text can hold, outside of a block
            // of its own (see `Reader::aligned`).
            "\\tightlist" | "\\rule" => {}
            "\\ensuremath" => {
                if let Some(arg) = args.first() {
                    self.math(&source_text(arg), false);
                }
            }
            "\\setcounter" => self.set_counter(args),
            // An item that isn't a list's own (see `gather`) is a space in
            // its text, and its groups' text follows.
            "\\item" => {
                self.space();
                for group in args {
                    self.flow(group);
                }
            }
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

    /// Sets the counter of an `enumerate` the text is in to `args`' value.
    /// That of one it isn't in does nothing: the next one starts over.
    fn set_counter(&mut self, args: Vec<Vec<Piece>>) {
        let [name, value] = &args[..] else {
            return;
        };
        let depth = ENUMERATION_COUNTERS
            .iter()
            .position(|counter| *counter == source_text(name).trim());
        let value = source_text(value).trim().parse();
        if let (Some(counter), Ok(value)) =
            (depth.and_then(|d| self.enumerations.get_mut(d)), value)
        {
            *counter = value;
        }
    }

    /// `$…$`, `\(…\)`, `$$…$$` or `\[…\]`: its TeX, as it's written. One
    /// that doesn't end, a stray `$`, say, isn't one: it's read as text, so
    /// the sections and lists the parser took into it stay what they are.
    fn formula(&mut self, formula: &SyntaxNode) {
        let mut pieces = pieces(formula);
        let ended = pieces.len() > 1
            && matches!(
                pieces.last(),
                Some(Piece::Token(
                    SyntaxKind::TokenDollar | SyntaxKind::TokenEndMath,
                    _
                ))
            );
        if !ended {
            return self.flow(pieces);
        }
        let display = matches!(pieces.first(),
            Some(Piece::Token(_, open)) if open == "$$" || open == "\\[");
        pieces.remove(0);
        pieces.pop();
        self.math(&source_text(&pieces), display);
    }

    /// Math, as TeX: a block of its own if it's `display`, but in an
    /// argument, which no block goes in. Empty, it shows nothing.
    fn math(&mut self, tex: &str, display: bool) {
        self.non_space();
        let trimmed = tex.trim_end();
        // A space that a backslash makes a control space is the formula's.
        let backslashes = trimmed.len() - trimmed.trim_end_matches('\\').len();
        let space = tex[trimmed.len()..].chars().next();
        let tex = match space {
            Some(space) if backslashes % 2 == 1 => &tex[..trimmed.len() + space.len_utf8()],
            _ => trimmed,
        };
        let tex = self.with_verbatims(tex.trim_start());
        if tex.is_empty() {
            return;
        }
        if display && !self.in_argument {
            self.end_paragraph();
            self.blocks.push(Block::Math(tex));
        } else {
            self.inline(Inline::Math(tex));
        }
    }

    /// `tex` with the verbatim text its placeholders stand for (see
    /// [`verbatim::cut`]) back in it, each a `\verb`.
    fn with_verbatims(&self, tex: &str) -> String {
        let mut out = String::new();
        let mut rest = tex;
        while let Some(at) = rest.find(&self.verbatim_mark) {
            let after = &rest[at + self.verbatim_mark.len()..];
            let placeholder = after.strip_prefix('{').and_then(|inside| {
                let (number, _) = inside.split_once('}')?;
                let index: usize = number.trim_end().parse().ok()?;
                let code = self.verbatims.get(index)?;
                Some((code, number.len() + 2))
            });
            out.push_str(&rest[..at]);
            let Some((code, length)) = placeholder else {
                // A command of the source's, whose name only starts as the
                // mark does.
                out.push_str(&self.verbatim_mark);
                rest = after;
                continue;
            };
            let (Verbatim::Inline(code) | Verbatim::Block(code)) = code;
            match ('!'..='~').find(|c| !c.is_alphanumeric() && *c != '*' && !code.contains(*c)) {
                Some(delimiter) => out.push_str(&format!("\\verb{delimiter}{code}{delimiter}")),
                // No `\verb` can hold it.
                None => out.push_str(code),
            }
            rest = &after[length..];
        }
        out.push_str(rest);
        out
    }

    /// A `quote`-like environment: a quote of its blocks, if it has any.
    /// Past [`MAX_LIST_NESTING`], it's read as its text.
    fn quote(&mut self, env: &SyntaxNode) {
        self.end_paragraph();
        let pieces = pieces(env);
        if self.lists >= MAX_LIST_NESTING {
            self.unlisted += 1;
            return self.flow(pieces);
        }
        self.lists += 1;
        let outer = std::mem::take(&mut self.blocks);
        self.flow(pieces);
        self.end_paragraph();
        let content = std::mem::replace(&mut self.blocks, outer);
        self.lists -= 1;
        if !content.is_empty() {
            self.blocks.push(Block::Quote(content));
        }
    }

    /// A `center`-like environment: blocks of their own, its alignment
    /// shown by no writer, or, with only a `\rule` in it, as the LaTeX
    /// writer writes one, a thematic break.
    fn aligned(&mut self, env: &SyntaxNode) {
        self.end_paragraph();
        let pieces = pieces(env);
        if is_a_rule(&pieces) {
            self.blocks.push(Block::ThematicBreak);
        } else {
            self.flow(pieces);
            self.end_paragraph();
        }
    }

    /// `arg`, an argument, in the font `bold` and `emphasized` say.
    fn in_font(&mut self, arg: Vec<Piece>, bold: bool, emphasized: bool) {
        let outer = (self.bold, self.emphasized, self.in_argument);
        (self.bold, self.emphasized, self.in_argument) = (bold, emphasized, true);
        self.flow(arg);
        (self.bold, self.emphasized, self.in_argument) = outer;
    }

    /// An `itemize`, `enumerate` or `description` environment: a list of
    /// its `\item`s (see [`gather`]), tight where `\tightlist` makes it so
    /// and none of its items' blocks would run together written that way
    /// (see [`loosens`]). An `enumerate` splits into lists where its
    /// numbers don't follow on: at a `\setcounter`, and around an item a
    /// label takes the number of, or numbered below 0, which no list
    /// writes. A `description`'s labels are its terms, in bold. Past
    /// [`MAX_LIST_NESTING`], it's read as its items' text.
    fn list(&mut self, env: &SyntaxNode, kind: ListKind) {
        let numbered = kind == ListKind::Enumerate;
        self.end_paragraph();
        let pieces = pieces(env);
        if self.lists >= MAX_LIST_NESTING {
            self.unlisted += 1;
            return self.flow(pieces);
        }
        let gathered = gather(pieces);
        // With no item of its own, it's its text.
        if gathered.items.is_empty() {
            return self.flow(gathered.before);
        }
        self.lists += 1;
        if numbered {
            let start = gathered.start.unwrap_or(1);
            self.enumerations.push(start.saturating_sub(1));
        }
        let outer = std::mem::take(&mut self.blocks);
        // What's before its first item is that item's, so nothing is lost,
        // and read before it's numbered, as it may set its number.
        self.flow(gathered.before);
        let mut lists: Vec<List> = Vec::new();
        for mut item in gathered.items {
            let start = self
                .enumerations
                .last_mut()
                .filter(|_| numbered && !item.labelled)
                .and_then(|counter| {
                    *counter = counter.saturating_add(1);
                    u64::try_from(*counter).ok()
                });
            let boxed = if numbered && !item.labelled {
                leading_box(&mut item.content)
            } else {
                None
            };
            if let Some(label) = item.label {
                if kind == ListKind::Description {
                    self.in_font(label, true, self.emphasized);
                } else {
                    self.flow(label);
                }
                // A control word ending the label takes no space that's
                // not in the source.
                self.skip_spaces = false;
                self.token(SyntaxKind::TokenWhiteSpace, " ");
            }
            self.flow(item.content);
            self.end_paragraph();
            let follows_on = lists.last().is_some_and(|list| match (list.start, start) {
                (Some(first), Some(number)) => first + list.items.len() as u64 == number,
                (None, None) => true,
                _ => false,
            });
            if !follows_on {
                lists.push(List {
                    start,
                    tight: gathered.tight,
                    items: Vec::new(),
                });
            }
            lists.last_mut().unwrap().items.push(ListItem {
                task: item.task.or(boxed),
                content: std::mem::take(&mut self.blocks),
            });
        }
        self.blocks = outer;
        if numbered {
            self.enumerations.pop();
        }
        self.lists -= 1;
        for mut list in lists {
            list.tight &= !list.items.iter().any(|item| loosens(&item.content));
            self.blocks.push(Block::List(list));
        }
    }

    /// A sectioning command's heading, at `level` in [`SECTIONS`] (see
    /// [`level_headings`]), its argument its text: a block of its own, after
    /// the paragraph before it. Its star (unnumbered) and short title (the
    /// table of contents') show nothing; one showing nothing is none.
    fn heading(&mut self, level: usize, args: Vec<Vec<Piece>>) {
        self.end_paragraph();
        let Some(arg) = args.into_iter().next() else {
            return;
        };
        // Set in its own font, emphasized or not: the emphasis around it
        // holds again after it.
        let emphasized = std::mem::take(&mut self.emphasized);
        let in_argument = std::mem::replace(&mut self.in_argument, true);
        self.flow(arg);
        self.in_argument = in_argument;
        self.emphasized = emphasized;
        let content = trimmed(std::mem::take(&mut self.inlines));
        if !content.is_empty() {
            // Seven of them, so an index fits.
            let level = level as u8 + 1;
            self.blocks.push(Block::Heading { level, content });
        }
    }

    /// The verbatim text a placeholder stands for, its index the number in
    /// its argument (see [`verbatim::cut`]): `\verb`'s inline code, the
    /// `verbatim` environment's a code block of its own.
    fn verbatim(&mut self, args: Vec<Vec<Piece>>) {
        let index = args.iter().flatten().find_map(|piece| match piece {
            Piece::Token(SyntaxKind::TokenWord, number) => number.parse::<usize>().ok(),
            _ => None,
        });
        match index.and_then(|index| self.verbatims.get(index)).cloned() {
            // Right after another, it's one with it, as TeX sets them, which
            // writers can't set apart.
            Some(Verbatim::Inline(code)) => {
                self.non_space();
                // Empty, it shows nothing.
                if code.is_empty() {
                    return;
                }
                let run = self.run();
                match run.last_mut() {
                    Some(Inline::Code(before)) => before.push_str(&code),
                    _ => run.push(Inline::Code(code)),
                }
            }
            Some(Verbatim::Block(code)) => {
                self.end_paragraph();
                self.blocks.push(Block::Code { lang: None, code });
            }
            None => {}
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

    fn text(&mut self, text: &str) {
        self.non_space();
        push_text(self.run(), text);
    }

    fn inline(&mut self, inline: Inline) {
        self.non_space();
        self.run().push(inline);
    }

    /// Where what's read goes: the runs of bold and emphasis at the end, or
    /// new ones, as far as it's in them.
    fn run(&mut self) -> &mut Vec<Inline> {
        let mut run = &mut self.inlines;
        if self.bold {
            if !matches!(run.last(), Some(Inline::Strong(_))) {
                run.push(Inline::Strong(Vec::new()));
            }
            let Some(Inline::Strong(bold)) = run.last_mut() else {
                unreachable!("a run of bold was just pushed");
            };
            run = bold;
        }
        if self.emphasized {
            if !matches!(run.last(), Some(Inline::Emph(_))) {
                run.push(Inline::Emph(Vec::new()));
            }
            let Some(Inline::Emph(emphasis)) = run.last_mut() else {
                unreachable!("a run of emphasis was just pushed");
            };
            run = emphasis;
        }
        run
    }

    /// A space, unless one is already there: TeX makes one of a run. One
    /// starting a paragraph goes, but not one starting an argument.
    fn space(&mut self) {
        self.settle_breaks();
        let mut last = self.inlines.last();
        while let Some(Inline::Strong(run) | Inline::Emph(run)) = last {
            last = run.last();
        }
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

/// What a command takes after its name, as LaTeX defines it: a star
/// (`\section*`), an optional argument (`[…]`) and required ones.
#[derive(Clone, Copy, Default)]
struct Signature {
    star: bool,
    optional: bool,
    /// Whether it takes its optional argument only without its star, as
    /// `\section` its short title.
    unstarred_optional: bool,
    required: usize,
}

#[cfg(test)]
thread_local! {
    /// How many pieces the reader has looked at for an optional argument's
    /// `]` on this thread, for tests to bound it without a clock.
    static BRACKET_SCAN_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A verbatim placeholder's: its index (see [`verbatim::Cut::mark`]).
const PLACEHOLDER: Signature = Signature {
    star: false,
    optional: false,
    unstarred_optional: false,
    required: 1,
};

/// `\item`'s: its label (`\item[a)]`).
const ITEM: Signature = Signature {
    star: false,
    optional: true,
    unstarred_optional: false,
    required: 0,
};

/// `\rule[raise]{width}{height}`'s.
const RULE: Signature = Signature {
    star: false,
    optional: true,
    unstarred_optional: false,
    required: 2,
};

/// `\setcounter{counter}{value}`'s.
const SET_COUNTER: Signature = Signature {
    star: false,
    optional: false,
    unstarred_optional: false,
    required: 2,
};

/// The counters of `enumerate`s, from the outermost in.
const ENUMERATION_COUNTERS: [&str; 4] = ["enumi", "enumii", "enumiii", "enumiv"];

/// The environments that are a formula, with their `\begin` and `\end`, as
/// writers take them; each has a starred form too.
const MATH_ENVIRONMENTS: [&str; 7] = [
    "equation", "align", "alignat", "flalign", "gather", "multline", "eqnarray",
];

/// The commands that define others (see [`skip_definition`]).
const DEFINITIONS: [&str; 7] = [
    "\\newcommand",
    "\\renewcommand",
    "\\providecommand",
    "\\def",
    "\\gdef",
    "\\edef",
    "\\xdef",
];

/// How many tokens a `\def`'s arguments are looked for its group past: more
/// than nine arguments and their delimiters take, and few enough that
/// definitions with no group don't each read the rest of the document.
const MAX_DEFINITION_PARAMETERS: usize = 64;

/// `\\`'s: `\\*[1em]`.
const LINE_BREAK: Signature = Signature {
    star: true,
    optional: true,
    unstarred_optional: false,
    required: 0,
};

/// The font a command sets its argument in, as `(bold, emphasized)`, in text
/// that's `bold` and `emphasized`; `None` for a command that sets none.
/// `\emph` switches between italic and upright, so inside another it's
/// upright: the reader nests no emphasis, which writers can't set apart from
/// the one around it (#126). No writer shows the other families and shapes,
/// which leave the font as it is.
fn font(name: &str, bold: bool, emphasized: bool) -> Option<(bool, bool)> {
    Some(match name {
        "\\emph" => (bold, !emphasized),
        "\\textbf" => (true, emphasized),
        "\\textmd" => (false, emphasized),
        "\\textit" | "\\textsl" => (bold, true),
        "\\textup" => (bold, false),
        "\\textnormal" => (false, false),
        "\\textrm" | "\\textsf" | "\\textsc" | "\\underline" => (bold, emphasized),
        _ => return None,
    })
}

/// What a command the reader knows takes; `None` for one it doesn't, which
/// keeps the text of the groups right after it (see [`Reader::command`]).
/// Those that take nothing leave the groups after them to be read on.
fn signature(name: &str) -> Option<Signature> {
    match name {
        "\\ensuremath" => Some(Signature {
            required: 1,
            ..Signature::default()
        }),
        _ if font(name, false, false).is_some() => Some(Signature {
            required: 1,
            ..Signature::default()
        }),
        "\\par" | "\\" | "\\tightlist" => Some(Signature::default()),
        "\\setcounter" => Some(SET_COUNTER),
        "\\rule" => Some(RULE),
        _ if ESCAPED.contains(&name) => Some(Signature::default()),
        _ if SECTIONS.contains(&name) => Some(Signature {
            star: true,
            optional: true,
            unstarred_optional: true,
            required: 1,
        }),
        _ => None,
    }
}

/// The sectioning commands, from the top level down.
const SECTIONS: [&str; 7] = [
    "\\part",
    "\\chapter",
    "\\section",
    "\\subsection",
    "\\subsubsection",
    "\\paragraph",
    "\\subparagraph",
];

/// `blocks`' headings, read at their command's level in [`SECTIONS`],
/// risen so that the top one a document uses is level 1, as `\section` is
/// in an article and `\chapter` in a book; past 6, they're 6.
/// Those in lists too.
fn level_headings(blocks: &mut [Block]) {
    let mut top: Option<u8> = None;
    super::walk::for_each_block(blocks, &mut |block| {
        if let Block::Heading { level, .. } = block {
            top = Some(top.map_or(*level, |top| top.min(*level)));
        }
    });
    let Some(top) = top else {
        return;
    };
    super::walk::for_each_block(blocks, &mut |block| {
        if let Block::Heading { level, .. } = block {
            *level = (*level - top + 1).min(6);
        }
    });
}

/// The arguments a command was given (see [`take_arguments`]).
struct Arguments {
    /// What's in its `[…]`, if it's given one.
    optional: Option<Vec<Piece>>,
    required: Vec<Vec<Piece>>,
}

/// Takes from `items[*at..]` what a command with `signature` takes, as TeX
/// and LaTeX take it: its required arguments each a group or a single
/// token (a word's first letter), looked for past spaces (see
/// [`past_spaces`]). A missing one, before an empty line or a group's end,
/// ends the taking. Its star is taken but not given, as no command the
/// reader knows shows it; LaTeX's `\@ifnextchar`, looking for it and the
/// optional argument, takes the spaces before them even where they're not
/// there.
///
/// `unbracketed` is how long a tail of `items` is known to hold no `]`:
/// an unclosed `[` scans it once, not again for every `[` before it.
fn take_arguments(
    items: &mut [Piece],
    at: &mut usize,
    unbracketed: &mut usize,
    signature: Signature,
) -> Arguments {
    let mut optional = None;
    let mut starred = false;
    if signature.star {
        *at = past_spaces(items, *at);
        if let Some(Piece::Token(SyntaxKind::TokenAsterisk, _)) = items.get(*at) {
            *at += 1;
            starred = true;
        }
    }
    if signature.optional && !(starred && signature.unstarred_optional) {
        *at = past_spaces(items, *at);
        // The first `]` past it closes it, a primed one (`]'`) too (see
        // `push_pieces`), but not one in a group: groups are nodes of their
        // own. Unclosed, `[` is text.
        if let Some(Piece::Token(SyntaxKind::TokenLBracket, _)) = items.get(*at) {
            // Up to the tail known to hold none, which may start before it.
            let after = *at + 1;
            let end = (items.len() - *unbracketed).max(after);
            let close = items[after..end].iter().position(|piece| {
                #[cfg(test)]
                BRACKET_SCAN_STEPS.with(|n| n.set(n.get() + 1));
                matches!(piece, Piece::Token(SyntaxKind::TokenRBracket, _))
            });
            match close {
                Some(close) => {
                    optional = Some(items[after..after + close].to_vec());
                    *at = after + close + 1;
                }
                None => *unbracketed = (*unbracketed).max(items.len() - after),
            }
        }
    }
    let mut required = Vec::new();
    while required.len() < signature.required {
        let next = past_spaces(items, *at);
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
    Arguments { optional, required }
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

/// `inlines` with the spaces and line breaks at the edges of their runs of
/// bold and emphasis (see [`Reader::run`]) moved out of them, where Markdown
/// wouldn't read them as runs, and those left empty gone.
fn hoisted(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out = Vec::with_capacity(inlines.len());
    for inline in inlines {
        let (run, bold) = match inline {
            Inline::Strong(run) => (run, true),
            Inline::Emph(run) => (run, false),
            other => {
                push_hoisted(&mut out, other);
                continue;
            }
        };
        let mut run = hoisted(run);
        let start = run.iter().position(|inline| !at_run_edge(inline));
        let Some(start) = start else {
            run.into_iter()
                .for_each(|inline| push_hoisted(&mut out, inline));
            continue;
        };
        for inline in run.drain(..start) {
            push_hoisted(&mut out, inline);
        }
        if let Some(Inline::Text(text)) = run.first_mut() {
            let spaces = text.len() - text.trim_start_matches(' ').len();
            let spaces: String = text.drain(..spaces).collect();
            push_hoisted(&mut out, Inline::Text(spaces));
        }
        let end = run.iter().rposition(|inline| !at_run_edge(inline));
        let after = run.split_off(end.map_or(0, |end| end + 1));
        let spaces = match run.last_mut() {
            Some(Inline::Text(text)) => text.split_off(text.trim_end_matches(' ').len()),
            _ => String::new(),
        };
        out.push(if bold {
            Inline::Strong(run)
        } else {
            Inline::Emph(run)
        });
        push_hoisted(&mut out, Inline::Text(spaces));
        for inline in after {
            push_hoisted(&mut out, inline);
        }
    }
    out
}

/// Whether `inline`, at the edge of a run, moves out of it whole.
fn at_run_edge(inline: &Inline) -> bool {
    match inline {
        Inline::LineBreak => true,
        Inline::Text(text) => text.trim_matches(' ').is_empty(),
        _ => false,
    }
}

/// Appends `inline` to `out`, where a run's edge moved it. Spaces stay as
/// many as they are: [`Reader::space`] made one of a run, so more are control
/// spaces, each a space of its own.
fn push_hoisted(out: &mut Vec<Inline>, inline: Inline) {
    match inline {
        Inline::Text(text) if text.is_empty() => {}
        Inline::Text(text) => push_text(out, &text),
        other => out.push(other),
    }
}

/// `inlines` without the spaces at their ends, a block's (`~` is kept).
/// Line breaks at the ends go too: LaTeX ends no line there, and Markdown
/// can't.
fn trimmed(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut inlines = hoisted(inlines);
    trim_start(&mut inlines);
    trim_end(&mut inlines);
    inlines.retain(|inline| !matches!(inline, Inline::Text(text) if text.is_empty()));
    inlines
}

fn trim_start(inlines: &mut Vec<Inline>) {
    let mut gone = 0;
    for inline in inlines.iter_mut() {
        match inline {
            Inline::LineBreak => {}
            Inline::Text(text) => {
                *text = text.trim_start_matches(' ').to_string();
                if !text.is_empty() {
                    break;
                }
            }
            Inline::Emph(run) => {
                trim_start(run);
                if !run.is_empty() {
                    break;
                }
            }
            _ => break,
        }
        gone += 1;
    }
    inlines.drain(..gone);
}

fn trim_end(inlines: &mut Vec<Inline>) {
    while let Some(last) = inlines.last_mut() {
        match last {
            Inline::LineBreak => {}
            Inline::Text(text) => {
                text.truncate(text.trim_end_matches(' ').len());
                if !text.is_empty() {
                    return;
                }
            }
            Inline::Emph(run) => {
                trim_end(run);
                if !run.is_empty() {
                    return;
                }
            }
            _ => return,
        }
        inlines.pop();
    }
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

    /// Each row: the blocks the source reads as. The spaces after each
    /// control word go (#185).
    #[test]
    fn the_else_of_an_iffalse_is_read() {
        let cases: Vec<(&str, Vec<Block>)> = vec![
            ("\\iffalse A\\else B\\fi C", vec![para(&[text("BC")])]),
            ("a\\iffalse x\\else b\\fi c", vec![para(&[text("abc")])]),
            (
                "\\iffalse\nA\n\\else\nB\n\\fi\nC",
                vec![para(&[text("B C")])],
            ),
            ("\\iffalse A\\fi B", vec![para(&[text("B")])]),
            (
                "\\iftrue A\\else\\iffalse X\\else B\\fi\\fi",
                vec![para(&[text("A")])],
            ),
            (
                "\\iffalse A\\else \\verb|x|\\fi",
                vec![para(&[Inline::Code("x".to_string())])],
            ),
            (
                "\\iffalse A\\else\\section{B}\\fi",
                vec![Block::Heading {
                    level: 1,
                    content: vec![text("B")],
                }],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
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
                &[&[emph(&[text("a")]), text(" b "), emph(&[text("c")])]],
            ),
            ("\\emph{\\emph{x}}", &[&[text("x")]]),
            (
                "A\\emph{B}'s",
                &[&[text("A"), emph(&[text("B")]), text("'s")]],
            ),
            // The spaces after a control space in it go after it.
            ("A\\emph\\  B", &[&[text("A B")]]),
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
                &[&[text("a "), emph(&[text("b")]), text(" c")]],
            ),
            (
                "a\\emph{b\n}c",
                &[&[text("a"), emph(&[text("b")]), text(" c")]],
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

    /// `\\` takes a star and `[…]` past spaces, and the spaces after them;
    /// at a paragraph's ends, where LaTeX ends no line, it's dropped.
    #[test]
    fn line_breaks_take_their_star_and_length() {
        let a_b = || vec![text("a"), Inline::LineBreak, text("b")];
        let cases: &[(&str, &[Vec<Inline>])] = &[
            ("a\\\\b", &[a_b()]),
            ("a\\\\ *  [2pt]  b", &[a_b()]),
            ("a\\\\*\n[2pt]\nb", &[a_b()]),
            // The first `]` closes it, but not one in a group.
            ("a\\\\[{]}] b", &[a_b()]),
            (
                "a\\\\[2pt]'b ] c",
                &[vec![text("a"), Inline::LineBreak, text("'b ] c")]],
            ),
            (
                "a\\\\[x] y] b",
                &[vec![text("a"), Inline::LineBreak, text("y] b")]],
            ),
            // Unclosed, `[` is text; past an empty line, a new paragraph.
            (
                "a\\\\ [b",
                &[vec![text("a"), Inline::LineBreak, text("[b")]],
            ),
            ("a\\\\\n\n[2pt]b", &[vec![text("a")], vec![text("[2pt]b")]]),
            ("\\\\a\\\\ ", &[vec![text("a")]]),
            // Emphasized, it's in the emphasis, at its ends too.
            (
                "\\emph{a\\\\b}",
                &[vec![emph(&[text("a"), Inline::LineBreak, text("b")])]],
            ),
            ("\\emph{ a\\\\}", &[vec![emph(&[text("a")])]]),
        ];
        for &(source, expected) in cases {
            assert_eq!(paragraphs(source), expected, "{source}");
        }
    }

    /// Unclosed `[`s, one after another, look for their `]` in what's after
    /// them once, not once each.
    #[test]
    fn unclosed_optional_arguments_are_scanned_once() {
        const COUNT: usize = 2000;
        // Each `a\\[` is three pieces, and each `It's ` four, its prime
        // taken apart beforehand: the first `[` scans past them all.
        for (source, pieces) in [
            ("a\\\\[".repeat(COUNT), 3 * COUNT),
            (format!("a\\\\[{}", "It's ".repeat(COUNT)), 4 * COUNT),
        ] {
            BRACKET_SCAN_STEPS.with(|n| n.set(0));
            // Read here, not on `read`'s thread, to count.
            read_parsed(&source);
            let steps = BRACKET_SCAN_STEPS.with(std::cell::Cell::get);
            assert!((COUNT..=pieces).contains(&steps), "{steps}");
        }
    }

    /// Arguments taken from primed words (`\emph It's`) again and again
    /// are each its first letter, the parser's joining undone beforehand
    /// rather than for each.
    #[test]
    fn arguments_from_primed_words_are_taken_in_turn() {
        const COUNT: usize = 20_000;
        let (doc, _) = read(&"\\emph It's ".repeat(COUNT));
        let [Block::Paragraph(inlines)] = &doc.blocks[..] else {
            panic!("{:?}", doc.blocks);
        };
        let emphases = inlines
            .iter()
            .filter(|inline| **inline == emph(&[text("I")]))
            .count();
        assert_eq!(emphases, COUNT);
    }

    /// Emphases side by side and one in another come back from Markdown as
    /// they're read, the text and only the text, and so do line breaks.
    #[test]
    fn emphases_survive_markdown() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "\\emph{x}\\emph{y} z",
            "\\emph{a \\emph{b} c}",
            "\\emph{\\emph{x}}",
            "a\\emph{b}c",
            "a\\\\b \\emph{c\\\\d} e\\\\",
            "\\emph{ a\\\\}",
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

    fn heading(level: u8, content: &[Inline]) -> Block {
        Block::Heading {
            level,
            content: content.to_vec(),
        }
    }

    fn para(content: &[Inline]) -> Block {
        Block::Paragraph(content.to_vec())
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn sectioning_commands_are_headings() {
        let cases: &[(&str, &[Block])] = &[
            // A block of its own, between paragraphs.
            (
                "a \\section{B} c",
                &[
                    para(&[text("a")]),
                    heading(1, &[text("B")]),
                    para(&[text("c")]),
                ],
            ),
            // Its short title shows nothing; its text is inline.
            (
                "\\section [Short] {Long \\emph{one}}",
                &[heading(1, &[text("Long "), emph(&[text("one")])])],
            ),
            // Nor does its star, after which it takes no short title.
            ("\\section* {A}", &[heading(1, &[text("A")])]),
            (
                "\\section*[] body",
                &[heading(1, &[text("[")]), para(&[text("] body")])],
            ),
            // An empty line in it is a space; one showing nothing is none.
            (
                "\\section{a\n\nb}\\section{ }",
                &[heading(1, &[text("a b")])],
            ),
            // Set in its own font, inside emphasis too.
            (
                "\\emph{a \\section{B} c}",
                &[
                    para(&[emph(&[text("a")])]),
                    heading(1, &[text("B")]),
                    para(&[emph(&[text("c")])]),
                ],
            ),
        ];
        for &(source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// `\verb` is inline code and the `verbatim` environment a code block,
    /// their text as it is, LaTeX's own characters included.
    #[test]
    fn verbatim_text_is_code() {
        let code = |text: &str| Inline::Code(text.into());
        let block = |text: &str| Block::Code {
            lang: None,
            code: text.into(),
        };
        let cases: &[(&str, &[Block])] = &[
            (
                "a \\verb|x{y}%z| b",
                &[para(&[text("a "), code("x{y}%z"), text(" b")])],
            ),
            // A command of the placeholders' name in the source is no
            // placeholder (see `verbatim::Cut::mark`).
            (
                "\\SwiftShifterVerbatim{0} and \\verb|x|",
                &[para(&[text("0 and "), code("x")])],
            ),
            // What `\iffalse` skips isn't cut, so its `\fi` ends it.
            ("\\iffalse\\verb|\\fi| shown", &[para(&[text("| shown")])]),
            (
                "\\emph{see \\verb!\\emph!}",
                &[para(&[emph(&[text("see "), code("\\emph")])])],
            ),
            (
                "a\n\\begin{verbatim}\n  {%x}\n\n\\end{y}\n\\end{verbatim}\nb",
                &[
                    para(&[text("a")]),
                    block("  {%x}\n\n\\end{y}"),
                    para(&[text("b")]),
                ],
            ),
            // With a space before its name, as LaTeX allows (#167).
            ("\\begin {verbatim}\n$x$\n\\end{verbatim}", &[block("$x$")]),
        ];
        for &(source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
        // Side by side, it's one; its spaces stay.
        assert_eq!(
            read("\\verb|a|\\verb|b| \\verb| c |").0.blocks,
            [para(&[code("ab"), text(" "), code(" c ")])]
        );
        // Back from Markdown as it's read.
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            cases[0].0,
            cases[3].0,
            cases[4].0,
            "\\verb|a|\\verb|b| \\verb| c | \\verb|`|",
            "a\\verb!!b \\verb|`x`|",
        ] {
            let (doc, _) = read(source);
            let written = write::markdown::write(&doc);
            assert_eq!(markdown::read(&written).blocks, doc.blocks, "{written}");
        }
    }

    fn list(tight: bool, items: &[(Option<bool>, &[Block])]) -> Block {
        Block::List(List {
            start: None,
            tight,
            items: items
                .iter()
                .map(|&(task, content)| ListItem {
                    task,
                    content: content.to_vec(),
                })
                .collect(),
        })
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn itemize_is_a_list() {
        let a = [para(&[text("a")])];
        let b = [para(&[text("b")])];
        let cases: &[(&str, &[Block])] = &[
            // Each item the blocks up to the next, between paragraphs.
            (
                "x\n\\begin{itemize}\n\\item a\n\\item b\n\n  c\n\\end{itemize}\ny",
                &[
                    para(&[text("x")]),
                    list(
                        false,
                        &[
                            (None, &a),
                            (None, &[para(&[text("b")]), para(&[text("c")])]),
                        ],
                    ),
                    para(&[text("y")]),
                ],
            ),
            // What's before the first item sets its spacing, and its text
            // is the first item's.
            (
                "\\begin{itemize}\\tightlist kept \\item a\\item b\\end{itemize}",
                &[list(
                    true,
                    &[(None, &[para(&[text("kept a")])]), (None, &b)],
                )],
            ),
            // Tasks, and labels that lead their item.
            (
                "\\begin{itemize}\\item[$\\boxtimes$] a\\item [ $\\square$ ] b\\item[(i)] c\\end{itemize}",
                &[list(
                    false,
                    &[
                        (Some(true), &a),
                        (Some(false), &b),
                        (None, &[para(&[text("(i) c")])]),
                    ],
                )],
            ),
            // Nested, in its item.
            (
                "\\begin{itemize}\\item a\\begin{itemize}\\item b\\end{itemize}\\end{itemize}",
                &[list(
                    false,
                    &[(None, &[para(&[text("a")]), list(false, &[(None, &b)])])],
                )],
            ),
            // Only its own items split it: one in a group or another
            // environment is a space in its item's text, the group or
            // environment whole, and nothing is lost. With none of its own,
            // it's its text; what's before its first is that item's.
            (
                "\\begin{itemize}{\\item a}\\item \\emph{b\\item c} d\\end{itemize}",
                &[list(
                    false,
                    &[(
                        None,
                        // Its own item takes the spaces after it, as
                        // LaTeX's looks for a label past them.
                        &[para(&[text("a"), emph(&[text("b c")]), text(" d")])],
                    )],
                )],
            ),
            (
                "\\begin{itemize}\\begin{samepage}\\item alpha\\end{samepage}\\end{itemize}",
                &[para(&[text("alpha")])],
            ),
            // Elsewhere, `\tightlist` shows nothing.
            (
                "\\begin{itemize}x\\end{itemize}\\tightlist y",
                &[para(&[text("xy")])],
            ),
        ];
        for &(source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    fn numbered(start: u64, tight: bool, items: &[(Option<bool>, &[Block])]) -> Block {
        let Block::List(list) = list(tight, items) else {
            unreachable!()
        };
        Block::List(List {
            start: Some(start),
            ..list
        })
    }

    /// Where an `enumerate` split, its lists stay apart in Markdown, with
    /// their numbers.
    #[test]
    fn split_enumerates_keep_their_numbers_in_markdown() {
        use crate::converter::document::native::write;
        let doc = read(
            "\\begin{enumerate}\\item a\\setcounter{enumi}{6}\\item b\\item c\
             \\item[(x)] d\\item e\\end{enumerate}",
        )
        .0;
        assert_eq!(
            write::markdown::write(&doc),
            "1. a\n\n7) b\n\n8) c\n\n- (x) d\n\n9. e\n"
        );
    }

    /// Each row: the blocks the source reads as, numbered as LaTeX numbers
    /// them (#160).
    #[test]
    fn enumerate_is_numbered_as_latex_numbers_it() {
        let a = [para(&[text("a")])];
        let b = [para(&[text("b")])];
        let c = [para(&[text("c")])];
        let cases: &[(&str, &[Block])] = &[
            (
                "\\begin{enumerate}\\item a\\item b\\end{enumerate}",
                &[numbered(1, false, &[(None, &a), (None, &b)])],
            ),
            // What's before the first item sets its number and spacing, and
            // its text is the first item's.
            (
                "\\begin{enumerate}\\setcounter{enumi}{2}\\tightlist kept \\item a\\item b\\end{enumerate}",
                &[numbered(
                    3,
                    true,
                    &[(None, &[para(&[text("kept a")])]), (None, &b)],
                )],
            ),
            // A label takes the place of an item's number, which the next
            // takes: a list without numbers, between those with them.
            (
                "\\begin{enumerate}\\item a\\item[(x)] b\\item c\\end{enumerate}",
                &[
                    numbered(1, false, &[(None, &a)]),
                    list(false, &[(None, &[para(&[text("(x) b")])])]),
                    numbered(2, false, &[(None, &c)]),
                ],
            ),
            // So does a task's label; a box in the text keeps its number,
            // as the LaTeX writer writes a numbered task.
            (
                "\\begin{enumerate}\\item[$\\square$] a\\item b\\end{enumerate}",
                &[
                    list(false, &[(Some(false), &a)]),
                    numbered(1, false, &[(None, &b)]),
                ],
            ),
            (
                "\\begin{enumerate}\\item $\\square$ a\\item $\\boxtimes$ b\\item c\\end{enumerate}",
                &[numbered(
                    1,
                    false,
                    &[(Some(false), &a), (Some(true), &b), (None, &c)],
                )],
            ),
            // `\setcounter` numbers the items after it, wherever it's read:
            // in a group, in another environment, in a list inside.
            (
                "\\begin{enumerate}\\item a{\\setcounter{enumi}{6}}\\item b\
                 \\setcounter{enumi}{2}\\item c\\end{enumerate}",
                &[
                    numbered(1, false, &[(None, &a)]),
                    numbered(7, false, &[(None, &b)]),
                    numbered(3, false, &[(None, &c)]),
                ],
            ),
            (
                "\\begin{enumerate}\\item a\\begin{samepage}\\setcounter{enumi}{5}\\end{samepage}\
                 \\item b\\end{enumerate}",
                &[
                    numbered(1, false, &[(None, &a)]),
                    numbered(6, false, &[(None, &b)]),
                ],
            ),
            (
                "\\begin{enumerate}\\item a\\begin{itemize}\\item b\\setcounter{enumi}{5}\\end{itemize}\
                 \\item c\\end{enumerate}",
                &[
                    numbered(
                        1,
                        false,
                        &[(None, &[para(&[text("a")]), list(false, &[(None, &b)])])],
                    ),
                    numbered(6, false, &[(None, &c)]),
                ],
            ),
            // Back to the start, it's a list of its own, or the two would
            // read as one going on.
            (
                "\\begin{enumerate}\\item a\\setcounter{enumi}{0}\\item b\\end{enumerate}",
                &[
                    numbered(1, false, &[(None, &a)]),
                    numbered(1, false, &[(None, &b)]),
                ],
            ),
            // Each `enumerate` has its counter, by how deep it is, and an
            // outer one's can be set from inside; one that isn't running
            // does nothing, as the next starts over.
            (
                "\\begin{enumerate}\\item a\\begin{enumerate}\\setcounter{enumii}{3}\\item b\
                 \\setcounter{enumi}{7}\\end{enumerate}\\item c\\end{enumerate}",
                &[
                    numbered(
                        1,
                        false,
                        &[(
                            None,
                            &[para(&[text("a")]), numbered(4, false, &[(None, &b)])],
                        )],
                    ),
                    numbered(8, false, &[(None, &c)]),
                ],
            ),
            (
                "\\setcounter{enumi}{4}\\begin{enumerate}\\setcounter{enumii}{4}\\item a\\end{enumerate}",
                &[numbered(1, false, &[(None, &a)])],
            ),
            // enumitem's options show as nothing but their start, which
            // `\setcounter` still sets over; a label is still an item's.
            (
                "\\begin{enumerate}[(a)]\\item a\\end{enumerate}",
                &[numbered(1, false, &[(None, &a)])],
            ),
            (
                "\\begin{enumerate} [label=(\\alph*), start=4 ,leftmargin=*]\n\\item a\\item b\\end{enumerate}",
                &[numbered(4, false, &[(None, &a), (None, &b)])],
            ),
            (
                "\\begin{enumerate}[start=4]\\setcounter{enumi}{1}\\item a\\end{enumerate}",
                &[numbered(2, false, &[(None, &a)])],
            ),
            (
                "\\begin{enumerate}[start=x]\\item a\\end{enumerate}",
                &[numbered(1, false, &[(None, &a)])],
            ),
            // Options that run code are read as text, and what they do holds,
            // where it's read rather than when they'd run.
            (
                "\\begin{enumerate}[before={\\setcounter{enumi}{4}}]\\item a\\end{enumerate}",
                &[numbered(
                    5,
                    false,
                    &[(None, &[para(&[text("[before=]a")])])],
                )],
            ),
            (
                "\\begin{enumerate}[after={\\setcounter{enumi}{4}}]\\item a\\end{enumerate}",
                &[numbered(5, false, &[(None, &[para(&[text("[after=]a")])])])],
            ),
            // The last one holds, and one in a group is another's value.
            (
                "\\begin{enumerate}[start=2,start=5]\\item a\\end{enumerate}",
                &[numbered(5, false, &[(None, &a)])],
            ),
            (
                "\\begin{enumerate}[start=4,start]\\item a\\end{enumerate}",
                &[numbered(1, false, &[(None, &a)])],
            ),
            (
                "\\begin{enumerate}[label={x,start=7,y}]\\item a\\end{enumerate}",
                &[numbered(1, false, &[(None, &a)])],
            ),
            (
                "\\begin{itemize}[noitemsep]\\item a\\item[x] b\\end{itemize}",
                &[list(false, &[(None, &a), (None, &[para(&[text("x b")])])])],
            ),
            (
                "\\begin{enumerate}\\item [a]\\end{enumerate}",
                &[list(false, &[(None, &[para(&[text("a")])])])],
            ),
            // Comments in its arguments are none of them.
            (
                "\\begin{enumerate}\\setcounter{enu% name\n  mi}{2% value\n}\\item a\\end{enumerate}",
                &[numbered(3, false, &[(None, &a)])],
            ),
            // Other counters, and values that aren't numbers, set nothing.
            (
                "\\begin{enumerate}\\setcounter{page}{4}\\setcounter{enumi}{\\value{x}}\\item a\\end{enumerate}",
                &[numbered(1, false, &[(None, &a)])],
            ),
            // At 0 too; below it, no list can say so.
            (
                "\\begin{enumerate}\\setcounter{enumi}{-1}\\item a\\end{enumerate}",
                &[numbered(0, false, &[(None, &a)])],
            ),
            (
                "\\begin{enumerate}\\setcounter{enumi}{-5}\\item a\\end{enumerate}",
                &[list(false, &[(None, &a)])],
            ),
            // Elsewhere, it shows nothing.
            ("x\\setcounter{enumi}{2}y", &[para(&[text("xy")])]),
        ];
        for &(source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// A definition only runs where it's called.
    #[test]
    fn definitions_set_no_counters() {
        let a = [para(&[text("a")])];
        let b = [para(&[text("b")])];
        for definition in [
            "\\newcommand{\\f}{\\setcounter{enumi}{7}}",
            "\\newcommand*{\\f}[1][x]{\\setcounter{enumi}{7}}",
            "\\renewcommand\\f[2]{\\setcounter{enumi}{7}}",
            "\\providecommand{\\f}{\\setcounter{enumi}{7}}",
            "\\def\\f{\\setcounter{enumi}{7}}",
            "\\def\\f#1.{\\setcounter{enumi}{7}}",
            "\\gdef\\f{\\setcounter{enumi}{7}}",
        ] {
            let source =
                format!("\\begin{{enumerate}}\\item a{definition}\\item b\\end{{enumerate}}");
            assert_eq!(
                read(&source).0.blocks,
                [numbered(1, false, &[(None, &a), (None, &b)])],
                "{source}"
            );
        }
    }

    /// The lists the LaTeX writer writes, tasks and starts among them, read
    /// back as they were.
    #[test]
    fn our_latex_lists_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        let source = "- [x] done\n- [ ] todo\n\n* one\n\n  more\n* two\n  - in\n\n\
                      3. a\n4. b\n   1. in\n\n1) [ ] todo\n2) [x] done\n3) next\n";
        let doc = markdown::read(source);
        let tex = write::latex::write(&doc);
        assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
    }

    /// `\tightlist` makes a list tight only where its items' blocks don't
    /// run together written that way: they read back from Markdown as
    /// they were.
    #[test]
    fn tight_lists_keep_their_items_blocks_apart() {
        use crate::converter::document::native::{read::markdown, write};
        let cases = [
            (
                "\\begin{itemize}\\tightlist\\item a\\item b\\end{itemize}",
                true,
            ),
            // Two paragraphs in an item.
            (
                "\\begin{itemize}\\tightlist\\item Alpha.\\par Beta.\\end{itemize}",
                false,
            ),
            // Text after a nested list, which would go on its last item.
            (
                "\\begin{itemize}\\tightlist\\item a\
                 \\begin{itemize}\\tightlist\\item b\\end{itemize}\n\nc\\end{itemize}",
                false,
            ),
        ];
        for (source, tight) in cases {
            let doc = read(source).0;
            assert!(
                matches!(&doc.blocks[..], [Block::List(list)] if list.tight == tight),
                "{source}"
            );
            let written = write::markdown::write(&doc);
            assert_eq!(markdown::read(&written).blocks, doc.blocks, "{written}");
        }
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn math_is_kept_as_tex() {
        let inline = |tex: &str| Inline::Math(tex.to_string());
        let display = |tex: &str| Block::Math(tex.to_string());
        let a = para(&[text("a")]);
        let b = para(&[text("b")]);
        let cases: Vec<(&str, Vec<Block>)> = vec![
            (
                "a $x_1$ b \\(y\\) c",
                vec![para(&[
                    text("a "),
                    inline("x_1"),
                    text(" b "),
                    inline("y"),
                    text(" c"),
                ])],
            ),
            // Display math is a block of its own, the text around it two.
            ("a\\[x\\]b", vec![a.clone(), display("x"), b.clone()]),
            ("a$$ x $$b", vec![a.clone(), display("x"), b.clone()]),
            ("\\[\nx % why\n  y\n\\]", vec![display("x y")]),
            // Environments keep their `\begin` and `\end`, inside formulas
            // too, which the tree doesn't hold.
            (
                "a\\begin{align*}\nx &= y \\\\\nz\n\\end{align*}b",
                vec![
                    a.clone(),
                    display("\\begin{align*}\nx &= y \\\\\nz\n\\end{align*}"),
                    b.clone(),
                ],
            ),
            (
                "\\begin{equation}\\begin{split}x\\end{split}\\end{equation}",
                vec![display(
                    "\\begin{equation}\\begin{split}x\\end{split}\\end{equation}",
                )],
            ),
            (
                "$\\begin{aligned}[t]x\\end{aligned}$",
                vec![para(&[inline("\\begin{aligned}[t]x\\end{aligned}")])],
            ),
            (
                "\\begin{gather}x\\end{gather}",
                vec![display("\\begin{gather}x\\end{gather}")],
            ),
            (
                "\\begin{multline*}x\\end{multline*}",
                vec![display("\\begin{multline*}x\\end{multline*}")],
            ),
            // These two say no more than their delimiters do.
            (
                "\\begin{displaymath}x\\end{displaymath}",
                vec![display("x")],
            ),
            ("\\begin{math}x\\end{math}", vec![para(&[inline("x")])]),
            ("\\ensuremath{x^2}", vec![para(&[inline("x^2")])]),
            // What's hidden stays so, verbatim text is what it was, and a
            // comment doesn't join what's around it.
            ("$a\\iffalse hidden\\fi b$", vec![para(&[inline("ab")])]),
            (
                "$\\text{a\\iffalse hidden\\fi b}$",
                vec![para(&[inline("\\text{ab}")])],
            ),
            (
                "$\\alpha\\iffalse x\\fi b$",
                vec![para(&[inline("\\alpha b")])],
            ),
            // A control space at the end is the formula's.
            ("$x\\ $", vec![para(&[inline("x\\ ")])]),
            ("$x\\\\ $", vec![para(&[inline("x\\\\")])]),
            // Of however many bytes.
            ("$x\\\u{3000}$", vec![para(&[inline("x\\\u{3000}")])]),
            ("$x\\\u{a0} $", vec![para(&[inline("x\\\u{a0}")])]),
            // Side by side, each is a formula, and text after a display
            // formula starts a paragraph of its own.
            ("$x$$y$", vec![para(&[inline("x"), inline("y")])]),
            ("$x$$$y$$", vec![para(&[inline("x")]), display("y")]),
            ("\\[x\\] b", vec![display("x"), b.clone()]),
            ("$\\verb|x%y|$", vec![para(&[inline("\\verb!x%y!")])]),
            ("$\\verb!x|y!z$", vec![para(&[inline("\\verb!x|y!z")])]),
            ("$\\verb;x|!y;$", vec![para(&[inline("\\verb\"x|!y\"")])]),
            (
                "$\\SwiftShifterVerbatimOther{x}$",
                vec![para(&[inline("\\SwiftShifterVerbatimOther{x}")])],
            ),
            (
                "\\begin{equation}\\verb|x|\\end{equation}",
                vec![display("\\begin{equation}\\verb!x!\\end{equation}")],
            ),
            ("$\\alpha% why\n b$", vec![para(&[inline("\\alpha b")])]),
            (
                "$\\left\\langle% why\n b\\right\\rangle$",
                vec![para(&[inline("\\left\\langle b\\right\\rangle")])],
            ),
            ("$x% why\n b$", vec![para(&[inline("xb")])]),
            ("$\\\\a% why\n b$", vec![para(&[inline("\\\\ab")])]),
            // A formula in text in a formula is of it.
            ("$\\text{a $b$}$", vec![para(&[inline("\\text{a $b$}")])]),
            // In an argument, no block goes.
            (
                "\\section{a \\[x\\]}",
                vec![heading(1, &[text("a "), inline("x")])],
            ),
            ("\\emph{$x$}", vec![para(&[emph(&[inline("x")])])]),
            // Empty, it's none; unended, it's text, and what's after it is
            // read as it would be.
            ("a$$$$b", vec![para(&[text("ab")])]),
            ("a$x", vec![para(&[text("a$x")])]),
            ("$", vec![para(&[text("$")])]),
            ("\\[x", vec![para(&[text("\\[x")])]),
            (
                "\\emph{a $b} c",
                vec![para(&[emph(&[text("a $b")]), text(" c")])],
            ),
            (
                "Costs $5.\n\n\\section{Next}\n\nEnd",
                vec![
                    para(&[text("Costs $5.")]),
                    heading(1, &[text("Next")]),
                    para(&[text("End")]),
                ],
            ),
            // Escaped, it's a dollar.
            ("\\$5", vec![para(&[text("$5")])]),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// The formulas the LaTeX writer writes read back as they were.
    #[test]
    fn our_latex_math_reads_back() {
        use crate::converter::document::native::{read::markdown, write};
        let doc = markdown::read(
            "An $x^2$ here.\n\n$$\ny = 1\n$$\n\n$$\n\\begin{align}\na &= b\n\\end{align}\n$$\n",
        );
        let tex = write::latex::write(&doc);
        // Markdown keeps a display formula's line ends; LaTeX has none.
        let expected: Vec<Block> = doc
            .blocks
            .into_iter()
            .map(|block| match block {
                Block::Math(tex) => Block::Math(tex.trim().to_string()),
                other => other,
            })
            .collect();
        assert_eq!(read(&tex).0.blocks, expected, "{tex}");
    }

    /// Each row: the inlines the source reads as, in a paragraph.
    #[test]
    fn fonts_are_read_as_tex_sets_them() {
        let strong = |inlines: &[Inline]| Inline::Strong(inlines.to_vec());
        let a = || text("a");
        let b = || text("b");
        let x = || text("x");
        let y = || text("y");
        let cases: Vec<(&str, Vec<Inline>)> = vec![
            ("\\textbf{a}", vec![strong(&[a()])]),
            ("\\textbf a b", vec![strong(&[a()]), text(" b")]),
            ("\\textit{a}", vec![emph(&[a()])]),
            ("\\textsl{a}", vec![emph(&[a()])]),
            // Fonts no writer shows keep their text, and take one argument.
            ("\\textsc{a} b", vec![text("a b")]),
            ("\\textrm{a}\\textsf{b}", vec![text("ab")]),
            ("\\underline a b", vec![text("a b")]),
            ("\\textbf{\\textsc{a}}", vec![strong(&[a()])]),
            // Empty, they show nothing; a space stays a space.
            ("x\\textbf{}y", vec![text("xy")]),
            ("x\\textbf{ }y", vec![text("x y")]),
            ("x \\textbf{ } y", vec![text("x y")]),
            // Spaces and line breaks at their edges are outside them.
            (
                "x\\textbf{ a }y",
                vec![text("x "), strong(&[a()]), text(" y")],
            ),
            (
                "x \\textbf{ a } y",
                vec![text("x "), strong(&[a()]), text(" y")],
            ),
            ("x\\emph{ a }y", vec![text("x "), emph(&[a()]), text(" y")]),
            (
                "x\\emph{\\textbf{ a }}y",
                vec![text("x "), strong(&[emph(&[a()])]), text(" y")],
            ),
            // Control spaces there are each a space of their own.
            ("\\emph{a\\ \\ }b", vec![emph(&[a()]), text("  b")]),
            ("x\\emph{\\ \\ a}", vec![text("x  "), emph(&[a()])]),
            // A control word's spaces end with its group.
            ("\\textbf{a\\relax} b", vec![strong(&[a()]), text(" b")]),
            (
                "x\\textbf{a\\\\}y",
                vec![x(), strong(&[a()]), Inline::LineBreak, y()],
            ),
            (
                "x\\textbf{\\\\a}y",
                vec![x(), Inline::LineBreak, strong(&[a()]), y()],
            ),
            ("x\\textbf{\\\\}y", vec![x(), Inline::LineBreak, y()]),
            (
                "x\\emph{\\textbf{a\\\\}}y",
                vec![x(), strong(&[emph(&[a()])]), Inline::LineBreak, y()],
            ),
            // In the middle, they stay; an empty line is a space.
            (
                "\\textbf{a\\\\b}",
                vec![strong(&[a(), Inline::LineBreak, b()])],
            ),
            ("\\textbf{a\n\nb}", vec![strong(&[text("a b")])]),
            // Bold and emphasis, either way round, and upright in both.
            ("\\textbf{\\emph{a}}", vec![strong(&[emph(&[a()])])]),
            ("\\emph{\\textbf{a}}", vec![strong(&[emph(&[a()])])]),
            ("\\emph{\\textbf{\\emph{a}}}", vec![strong(&[a()])]),
            (
                "\\emph{x\\textbf{a}y}",
                vec![emph(&[x()]), strong(&[emph(&[a()])]), emph(&[y()])],
            ),
            (
                "\\textbf{x\\emph{a}y}",
                vec![strong(&[x(), emph(&[a()]), y()])],
            ),
            // `\textit` is italic in italic, where `\emph` is upright.
            ("\\emph{\\textit{a}}", vec![emph(&[a()])]),
            ("\\emph{\\textup{a}}", vec![a()]),
            // Bold in bold is bold, and what's alike side by side is one.
            ("\\textbf{x\\textbf{a}}", vec![strong(&[text("xa")])]),
            ("\\textbf{a}\\textbf{b}", vec![strong(&[text("ab")])]),
            (
                "\\textbf{\\emph{a}}\\textbf{\\emph{b}}",
                vec![strong(&[emph(&[text("ab")])])],
            ),
            (
                "\\textbf{a} \\textbf{b}",
                vec![strong(&[a()]), text(" "), strong(&[b()])],
            ),
            // What takes a font off holds in its argument alone.
            (
                "\\textbf{x\\textmd{a}y}",
                vec![strong(&[x()]), a(), strong(&[y()])],
            ),
            (
                "\\textbf{x\\textnormal{a}y}",
                vec![strong(&[x()]), a(), strong(&[y()])],
            ),
            (
                "\\textit{x\\textnormal{a}y}",
                vec![emph(&[x()]), a(), emph(&[y()])],
            ),
            (
                "\\textbf{\\textit{x\\textnormal{a}}}",
                vec![strong(&[emph(&[x()])]), a()],
            ),
            (
                "\\textbf{\\textit{x\\textmd{a}}}",
                vec![strong(&[emph(&[x()])]), emph(&[a()])],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(
                read(source).0.blocks,
                [Block::Paragraph(expected)],
                "{source}"
            );
        }
        // In a heading, a term, an empty one, and alone.
        assert_eq!(
            read("\\section{\\textbf{a}}").0.blocks,
            [heading(1, &[strong(&[text("a")])])]
        );
        assert_eq!(
            read("\\begin{description}\\item[\\textbf{a}] b\\end{description}")
                .0
                .blocks,
            [list(
                false,
                &[(None, &[para(&[strong(&[text("a")]), text(" b")])])]
            )]
        );
        assert_eq!(read("\\textbf{ }").0.blocks, []);
    }

    /// The fonts the LaTeX writer writes read back as they were.
    #[test]
    fn our_latex_fonts_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        let doc = markdown::read("**a** and *b*, **c *d* e** f\n");
        let tex = write::latex::write(&doc);
        assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
    }

    /// What Markdown can write reads back as the same text, in the same
    /// fonts. `***a*b*c***`, bold starting and ending in emphasis, it can't
    /// (#126).
    #[test]
    fn fonts_survive_markdown() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "x\\textbf{a} y",
            "x\\textbf{ a }y",
            "x\\emph{ a }y",
            "\\emph{x\\textbf{a}y}",
            "\\textbf{\\emph{a}}",
            "\\textbf{a}\\textbf{b}",
            "\\textbf{\\emph{a}}\\textbf{\\emph{b}}",
            "\\emph{\\textbf{a}\\textbf{b}}",
            "\\textbf{a}\\emph{b}",
            "\\emph{a}\\textbf{b}",
            "\\textbf{x\\textmd{a}y}",
            "\\textbf{a\\\\b}",
            "x\\textbf{a\\\\}y",
        ] {
            let (doc, _) = read(source);
            let written = write::markdown::write(&doc);
            let text = |blocks: &[Block]| match blocks {
                [Block::Paragraph(content)] => {
                    crate::converter::document::native::ast::inline_text(content)
                }
                _ => panic!("{blocks:?}"),
            };
            assert_eq!(
                text(&markdown::read(&written).blocks),
                text(&doc.blocks),
                "{source}: {written}"
            );
        }
    }

    /// Bold nested 20,000 deep reads flat, like emphasis.
    #[test]
    fn deep_bold_reads_flat() {
        let source = format!("{}x{}", "\\textbf{".repeat(20_000), "}".repeat(20_000));
        let blocks = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || read(&source).0.blocks)
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(blocks, [para(&[Inline::Strong(vec![text("x")])])]);
    }

    /// A `description`'s terms are bold; there's no list of terms to put
    /// them in.
    #[test]
    fn description_lists_have_their_terms_in_bold() {
        let strong = |inlines: &[Inline]| Inline::Strong(inlines.to_vec());
        assert_eq!(
            read("\\begin{description}\\item[Term] means\\item[\\emph{Other}] else\\item plain\\end{description}")
                .0
                .blocks,
            [list(
                false,
                &[
                    (None, &[para(&[strong(&[text("Term")]), text(" means")])]),
                    (
                        None,
                        &[para(&[strong(&[emph(&[text("Other")])]), text(" else")])]
                    ),
                    (None, &[para(&[text("plain")])]),
                ],
            )]
        );
        // A control word ending it leaves the space after it.
        assert_eq!(
            read("\\begin{description}\\item[Term\\relax]means\\end{description}")
                .0
                .blocks,
            [list(
                false,
                &[(None, &[para(&[strong(&[text("Term")]), text(" means")])])],
            )]
        );
        // An empty one is no term.
        assert_eq!(
            read("\\begin{description}\\item[] a\\end{description}")
                .0
                .blocks,
            [list(false, &[(None, &[para(&[text("a")])])])]
        );
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn quotes_and_alignments_are_blocks_of_their_own() {
        let a = para(&[text("a")]);
        let b = para(&[text("b")]);
        let c = para(&[text("c")]);
        let cases: Vec<(&str, Vec<Block>)> = vec![
            (
                "a\\begin{quote}b\\end{quote}c",
                vec![a.clone(), Block::Quote(vec![b.clone()]), c.clone()],
            ),
            (
                "\\begin{quotation}a\n\nb\\end{quotation}",
                vec![Block::Quote(vec![a.clone(), b.clone()])],
            ),
            (
                "\\begin{verse}a\\end{verse}",
                vec![Block::Quote(vec![a.clone()])],
            ),
            // Nested, and with blocks in it.
            (
                "\\begin{quote}\\begin{quote}a\\end{quote}\\section{b}\\end{quote}",
                vec![Block::Quote(vec![
                    Block::Quote(vec![a.clone()]),
                    heading(1, &[text("b")]),
                ])],
            ),
            // Empty, it's none.
            ("\\begin{quote}  \\end{quote}", vec![]),
            // Alignment shows in no writer: the blocks stay apart.
            (
                "a\\begin{center}b\\end{center}c",
                vec![a.clone(), b.clone(), c.clone()],
            ),
            (
                "a\\begin{flushleft}b\\end{flushleft}\\begin{flushright}c\\end{flushright}",
                vec![a.clone(), b.clone(), c.clone()],
            ),
            // A rule on its own is a break.
            (
                "a\\begin{center}\n\\rule{0.5\\linewidth}{0.5pt}\n\\end{center}b",
                vec![a.clone(), Block::ThematicBreak, b.clone()],
            ),
            (
                "\\begin{center}\\rule[1pt]{1cm}{2pt}\\end{center}",
                vec![Block::ThematicBreak],
            ),
            // Among text, it shows nothing, its arguments none of the text.
            (
                "\\begin{center}a\\rule{1cm}{2pt}\\end{center}",
                vec![a.clone()],
            ),
            ("a\\rule{1cm}{2pt}", vec![a.clone()]),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// The quotes and rules the LaTeX writer writes read back as they were.
    #[test]
    fn our_latex_quotes_and_rules_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        let doc = markdown::read("> a\n>\n> > b\n\n---\n\n> c\n");
        let tex = write::latex::write(&doc);
        assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
    }

    /// Past `MAX_LIST_NESTING`, quotes keep their text, as lists do.
    #[test]
    fn deep_quotes_are_read_as_text_past_the_nesting_bound() {
        const DEPTH: usize = 20_000;
        let source = format!(
            "{}x{}",
            "\\begin{quote}".repeat(DEPTH),
            "\\end{quote}".repeat(DEPTH)
        );
        let warning = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let (doc, warning) = read(&source);
                assert!(format!("{:?}", doc.blocks).contains('x'));
                warning
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(
            warning.unwrap().message(),
            format!(
                "{} lists or quotes nested too deeply were read as their text",
                DEPTH - MAX_LIST_NESTING
            )
        );
    }

    /// Lists nested 20,000 deep keep their text, nested no deeper than
    /// `MAX_LIST_NESTING`, so the document drops on the stack conversions
    /// run on.
    #[test]
    fn deep_lists_are_read_as_text_past_the_nesting_bound() {
        const DEPTH: usize = 20_000;
        let source = format!(
            "{}x{}",
            "\\begin{itemize}\\item ".repeat(DEPTH),
            "\\end{itemize}".repeat(DEPTH)
        );
        let (depth, warning) = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                // Dropped here, at the thread's end.
                let (doc, warning) = read(&source);
                let mut blocks = doc.blocks;
                let mut depth = 0;
                while let [Block::List(List { items, .. })] = &blocks[..] {
                    depth += 1;
                    blocks = items[0].content.clone();
                }
                assert_eq!(blocks, [para(&[text("x")])]);
                (depth, warning)
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(depth, MAX_LIST_NESTING);
        assert_eq!(
            warning.unwrap().message(),
            format!(
                "{} lists or quotes nested too deeply were read as their text",
                DEPTH - MAX_LIST_NESTING
            )
        );
    }

    /// A script at its end leaves a group's end where it is, so what follows
    /// stays outside it (#202).
    #[test]
    fn what_follows_a_group_ending_in_a_script_is_read() {
        for (source, blocks) in [
            ("\\section{a^}b", 2),
            ("\\section{a_ }b", 2),
            ("\\section{a^% c\n}b", 2),
            (
                "\\begin{equation}\\label{a^}x\\end{equation}\\section{b}\n\nc",
                3,
            ),
        ] {
            assert_eq!(read(source).0.blocks.len(), blocks, "{source}");
        }
    }

    /// The top level a document uses is level 1, and the levels under it
    /// follow, as far as 6.
    #[test]
    fn heading_levels_start_at_the_top_one_used() {
        let levels = |source: &str| -> Vec<u8> {
            read(source)
                .0
                .blocks
                .into_iter()
                .filter_map(|block| match block {
                    Block::Heading { level, .. } => Some(level),
                    _ => None,
                })
                .collect()
        };
        let below = "\\section{x}\\subsection{x}\\subsubsection{x}\\paragraph{x}\\subparagraph{x}";
        assert_eq!(levels(below), [1, 2, 3, 4, 5]);
        assert_eq!(
            levels(&format!("\\chapter{{x}}{below}")),
            [1, 2, 3, 4, 5, 6]
        );
        assert_eq!(
            levels(&format!("\\part{{x}}\\chapter{{x}}{below}")),
            [1, 2, 3, 4, 5, 6, 6]
        );
        assert_eq!(levels("\\subsection{x}\\section{x}"), [2, 1]);
        // In a list too.
        assert_eq!(
            read("\\section{A}\\begin{itemize}\\item a\\subsection{B}\\end{itemize}")
                .0
                .blocks,
            [
                heading(1, &[text("A")]),
                list(
                    false,
                    &[(None, &[para(&[text("a")]), heading(2, &[text("B")])])]
                ),
            ]
        );
    }

    /// A definition shows nothing where it's written, in any of its forms.
    /// A call isn't read yet: it shows only the groups after it.
    #[test]
    fn definitions_show_nothing() {
        for definition in [
            "\\newcommand{\\f}{hidden}",
            "\\newcommand*{\\f}[1][x]{hidden}",
            "\\renewcommand\\f[2]{hidden}",
            "\\providecommand{\\f}{hidden}",
            "\\def\\f{hidden}",
            "\\def\\f#1.{hidden}",
            "\\gdef\\f{hidden}",
            "\\edef\\f{hidden}",
            "\\xdef\\f{hidden}",
        ] {
            let source = format!("a{definition}b");
            assert_eq!(read(&source).0.blocks, [para(&[text("ab")])], "{source}");
        }
        assert_eq!(
            read("\\newcommand{\\f}[1]{hidden}a\\f b \\f{c}").0.blocks,
            [para(&[text("ab c")])]
        );
        // With no group in sight, what's after it isn't lost.
        let far = format!("a\\def\\f{}{{b}}", "#".repeat(MAX_DEFINITION_PARAMETERS));
        for source in ["a\\def\\f#1${hidden}b", "a\\def\\f#1 b", &far] {
            let blocks = format!("{:?}", read(source).0.blocks);
            assert!(blocks.contains('b'), "{source}: {blocks}");
        }
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
                .spawn(move || read_parsed(&source).0)
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
