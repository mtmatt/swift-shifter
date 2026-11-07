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
mod inputs;
mod macros;
pub mod paths;
pub mod stage;
mod verbatim;

use mitex_parser::CommandSpec;
use mitex_parser::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};
use verbatim::Verbatim;

use super::lists::loosens;
use super::walk;
use crate::converter::document::native::Warning;
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, ListItem, Meta, Table, inline_text,
};
use crate::converter::document::native::write;
use std::rc::Rc;

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

/// `source`, a document in `dir`, with the files it reads in (see
/// [`inputs`]), as [`read`] reads it, and warnings of what of those wasn't
/// read too.
#[allow(
    dead_code,
    reason = "native::convert reads .tex with it at the switch (#73)"
)]
pub fn read_in(source: &str, dir: &std::path::Path) -> (Document, Vec<Warning>) {
    let inlined = inputs::inlined(source, dir);
    let (doc, mut warnings) = read(&inlined.source);
    warnings.extend(inlined.warnings());
    (doc, warnings)
}

/// `source`, and, where it might nest too deep to parse, a warning that it
/// was read as plain text instead, or one of what of it was (see
/// [`read_parsed`]).
pub fn read(source: &str) -> (Document, Vec<Warning>) {
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
    (super::plain::read(source), vec![warning])
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
fn read_parsed(source: &str) -> (Document, Vec<Warning>) {
    let source = conditionals::keep_else(source);
    let cut = verbatim::cut(&source);
    // The parser's expanding macros, which may never end, would parse more
    // than `source` holds, past `nesting_bound`: `Reader::expand` reads them.
    let closed = formulas::close_scripts(&cut.source);
    let separated = formulas::separate_formulas(&closed);
    let root = mitex_parser::parse_without_macro(&separated, CommandSpec::new(Default::default()));
    let mut reader = Reader {
        verbatims: cut.verbatims,
        verbatim_mark: cut.mark,
        expansion_left: source
            .len()
            .saturating_mul(EXPANSION_PER_BYTE)
            .max(MIN_EXPANSION),
        nesting_left: nesting_bound(&source),
        ..Reader::default()
    };
    let body = document_body(&root);
    if body.is_some() {
        reader.preamble(&root);
    }
    reader.flow(pieces(&body.unwrap_or(root)));
    reader.close_notes();
    reader.end_paragraph();
    reader.blocks.append(&mut reader.notes);
    if !reader.note_aliases.is_empty() {
        walk::for_each_inlines(&mut reader.blocks, &mut |inlines| {
            for inline in inlines.iter_mut() {
                if let Inline::FootnoteRef(label) = inline
                    && let Some(note) = reader.note_aliases.get(label)
                {
                    *label = note.clone();
                }
            }
        });
    }
    level_headings(&mut reader.blocks);
    let mut warnings = Vec::new();
    if reader.unlisted > 0 {
        warnings.push(Warning {
            one: "list or quote nested too deeply was read as its text",
            many: "lists or quotes nested too deeply were read as their text",
            count: reader.unlisted,
            line: None,
        });
    }
    if reader.unexpanded > 0 {
        warnings.push(Warning {
            one: "command expanding too far was left unexpanded",
            many: "commands expanding too far were left unexpanded",
            count: reader.unexpanded,
            line: None,
        });
    }
    if reader.sparse_tables > 0 {
        warnings.push(Warning {
            one: "table too sparse to keep was read as its text",
            many: "tables too sparse to keep were read as their text",
            count: reader.sparse_tables,
            line: None,
        });
    }
    let mut doc = Document {
        meta: reader.meta,
        blocks: reader.blocks,
    };
    resolve_references(&mut doc, &reader.labels, &reader.note_labels);
    (doc, warnings)
}

/// Prefix of a reference's link until every label is known (see
/// [`resolve_references`]). No real URL starts with a control character.
const REFERENCE_LINK: char = '\u{1}';

/// Points the references in `doc` at their headings, by the ids writers
/// will give those, from `labels`, their index among the main text's
/// headings, which get theirs first. One to any other label keeps its text:
/// nothing else in a document has an id. A bare one (see
/// [`Reader::bare_reference`]) to a label in a note, in `note_labels`, is
/// that note's mark, which is so first, as marks say what's numbered.
fn resolve_references(
    doc: &mut Document,
    labels: &std::collections::HashMap<String, usize>,
    note_labels: &std::collections::HashMap<String, String>,
) {
    walk::for_each_inlines(&mut doc.blocks, &mut |inlines| {
        for inline in inlines.iter_mut() {
            if let Inline::Link { url, content, .. } = inline
                && content.is_empty()
                && let Some(label) = url.strip_prefix(REFERENCE_LINK)
                && let Some(note) = note_labels.get(label)
            {
                *inline = Inline::FootnoteRef(note.clone());
            }
        }
    });
    let (_, note_bodies) = write::Footnotes::collect(doc);
    let mut slugs = write::HeadingIds::default();
    let mut ids = Vec::new();
    write::for_each_heading(&doc.blocks, &note_bodies, &mut |_, content| {
        ids.push(slugs.next(content));
    });
    let blocks = &mut doc.blocks;
    walk::resolve_links(blocks, REFERENCE_LINK, &mut |label, content| {
        if content.is_empty() {
            *content = vec![Inline::Text(format!("[{label}]"))];
        }
        labels
            .get(label)
            .and_then(|heading| ids.get(*heading))
            .cloned()
    });
    // What's left as text joins the text around it, as if read as text.
    walk::for_each_inlines(blocks, &mut |inlines| {
        inlines.dedup_by(|next, text| match (next, text) {
            (Inline::Text(next), Inline::Text(text)) => {
                text.push_str(next);
                true
            }
            _ => false,
        });
    });
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

/// What a definition says: the command it defines, and what that stands for.
struct Definition {
    /// As written: `\x`.
    name: String,
    /// `None` for what can't be expanded, as a `\def` with text between its
    /// parameters.
    body: Option<Vec<Piece>>,
    /// How many arguments it takes.
    parameters: usize,
    /// What its first argument is, if that's optional.
    default: Option<String>,
    /// Whether a star followed the definition's command.
    starred: bool,
}

/// What a command defined with no arguments stands for.
struct Macro {
    pieces: Vec<Piece>,
    /// Its bytes in the source, with comments, which each call reads past
    /// too.
    length: usize,
    /// How deep it might nest (see [`nesting_bound`]).
    nesting: usize,
    /// What it stands for as TeX, for formulas (see [`macros`]).
    source: String,
    /// See [`Definition`].
    parameters: usize,
    default: Option<String>,
}

impl Macro {
    /// `pieces` as a macro, unless they leave open what they open:
    /// `\newcommand{\be}{\begin{equation}}` would need what's after its
    /// call, which the parser has read without it.
    fn new(pieces: Vec<Piece>, parameters: usize, default: Option<String>) -> Option<Macro> {
        // The braces of its definition's argument are no part of it: after
        // a last `\\`, the `}` would be a table's next row.
        let pieces = match pieces.as_slice() {
            [
                Piece::Token(SyntaxKind::TokenLBrace, _),
                inside @ ..,
                Piece::Token(SyntaxKind::TokenRBrace, _),
            ] => inside.to_vec(),
            _ => pieces,
        };
        let source = source_text(&pieces);
        // A `\\` ends a line, and what's after it isn't escaped.
        let unescaped = source.replace("\\\\", "");
        let count = |text: &str| unescaped.matches(text).count();
        // An `\end` with no `\begin` is an error to the parser.
        let stray_end = pieces.iter().any(|piece| match piece {
            Piece::Token(kind, _) => *kind == SyntaxKind::TokenError,
            Piece::Node(node) => node
                .descendants()
                .any(|node| node.kind() == SyntaxKind::TokenError),
        });
        let balanced = !stray_end
            && count("\\begin{") == count("\\end{")
            && count("\\[") == count("\\]")
            && count("\\(") == count("\\)")
            && (count("$") - count("\\$")) % 2 == 0;
        let length = pieces
            .iter()
            .map(|piece| match piece {
                Piece::Token(_, token) => token.len(),
                Piece::Node(node) => usize::from(node.text_range().len()),
            })
            .sum();
        balanced.then(|| Macro {
            pieces,
            length,
            nesting: nesting_bound(&source),
            source,
            parameters,
            default,
        })
    }
}

/// Whether mitex, which writers read formulas with, knows the command `name`.
fn known_in_formulas(name: &str) -> bool {
    let name = name.strip_prefix('\\').unwrap_or(name);
    mitex_spec_gen::DEFAULT_SPEC.get_cmd(name).is_some()
}

/// Takes the command `pieces` end in, if they end in one that would take
/// its arguments from what's after them: with the spaces after it, which
/// TeX doesn't see after a control word. Not one a command before it takes
/// as an argument, as the pieces are read in order, each command taking as
/// many as `required` says, nor a control symbol taking none with spaces
/// after it, which are text.
fn take_last_command(pieces: &mut Vec<Piece>, required: impl Fn(&str) -> usize) -> Option<Piece> {
    let shows = |piece: &Piece| match piece {
        Piece::Token(SyntaxKind::TokenWhiteSpace, _) => false,
        Piece::Token(SyntaxKind::TokenLineBreak, breaks) => line_ends(breaks) > 1,
        _ => true,
    };
    let command = |piece: &Piece| match piece {
        Piece::Node(node) if node.kind() == SyntaxKind::ItemCmd => Some(command_name(node)),
        _ => None,
    };
    let last = pieces.iter().rposition(shows)?;
    let name = command(&pieces[last])?;
    let control_word = name[1..].starts_with(|c: char| c.is_ascii_alphabetic());
    if !control_word && last + 1 < pieces.len() && required(&name) == 0 {
        return None;
    }
    // The arguments the commands read so far are still owed.
    let mut owed = 0_usize;
    for piece in pieces[..last].iter().filter(|piece| shows(piece)) {
        owed = match command(piece) {
            _ if owed > 0 => owed - 1,
            Some(name) => required(&name),
            None => 0,
        };
    }
    if owed > 0 {
        return None;
    }
    pieces.truncate(last + 1);
    pieces.pop()
}

/// How many files or folders `name`, a command reading a file in, names
/// (see [`inputs`]): what it takes, which shows nothing where the file isn't
/// read in. Not in [`signature`], which would take a letter of `\\input
/// file`'s name for one.
fn files_named(name: &str) -> Option<usize> {
    match name {
        "\\input" | "\\include" => Some(1),
        "\\import" | "\\subimport" | "\\inputfrom" | "\\subinputfrom" | "\\includefrom"
        | "\\subincludefrom" => Some(2),
        _ => None,
    }
}

/// Whether `piece` is a definition, or holds one.
fn holds_definition(piece: &Piece) -> bool {
    let Piece::Node(node) = piece else {
        return false;
    };
    node.descendants().any(|node| {
        node.kind() == SyntaxKind::ItemCmd && DEFINITIONS.contains(&command_name(&node).as_str())
    })
}

/// Takes from `items[*at..]` what the definition command `name` defines,
/// whose text doesn't show here, and says what it was.
fn take_definition(
    items: &mut [Piece],
    at: &mut usize,
    unbracketed: &mut usize,
    name: &str,
) -> Option<Definition> {
    let required = Signature {
        required: 1,
        ..Signature::default()
    };
    let name_of_it = Signature {
        star: true,
        ..required
    };
    let starred = matches!(
        items.get(past_spaces(items, *at)),
        Some(Piece::Token(SyntaxKind::TokenAsterisk, _))
    );
    let defined = take_arguments(items, at, unbracketed, name_of_it).required;
    let defined = source_text(defined.first()?).trim().to_string();
    let (mut parameters, mut default) = (Some(0), None);
    if name.ends_with("command") {
        // Its count of arguments, and the default of the first.
        let optional = Signature {
            optional: true,
            ..Signature::default()
        };
        if let Some(count) = take_arguments(items, at, unbracketed, optional).optional {
            let count = source_text(&count).trim().parse().ok();
            parameters = count.filter(|count| *count <= 9);
        }
        default = take_arguments(items, at, unbracketed, optional)
            .optional
            .map(|default| source_text(&default));
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
            Some((text, Piece::Node(node))) if node.kind() == SyntaxKind::ItemCurly => {
                parameters = undelimited_parameters(&source_text(&items[*at..*at + text]));
                *at += text;
            }
            _ => return None,
        }
    }
    let body = take_arguments(items, at, unbracketed, required)
        .required
        .into_iter()
        .next()?;
    Some(Definition {
        name: defined,
        body: parameters.is_some().then_some(body),
        parameters: parameters.unwrap_or(0),
        default,
        starred,
    })
}

/// How many parameters `text`, what a `\def` has before its group, makes,
/// unless there's text among them, which isn't read.
fn undelimited_parameters(text: &str) -> Option<usize> {
    // Spaces after it are its parameters' delimiters, but those before them
    // go with the name before.
    let text = text.trim_start();
    let count = text.len() / 2;
    let written: String = (1..=count).map(|number| format!("#{number}")).collect();
    (written == text && count <= 9).then_some(count)
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
    /// Whether the last thing read is a `'` a next one joins (see
    /// [`Reader::apostrophe`]).
    after_apostrophe: bool,
    /// The label of the note being read, if any.
    note: Option<String>,
    /// The notes read so far, which end the document.
    notes: Vec<Block>,
    /// How many notes have been given a label.
    note_count: usize,
    /// LaTeX's `footnote` counter.
    footnote_count: i64,
    /// By number, the notes with a mark and no text yet, and the note of
    /// that number before the mark, if it was given its number.
    unwritten_notes: std::collections::BTreeMap<i64, (String, Option<String>)>,
    /// By number, the last note with a text.
    finished_notes: std::collections::HashMap<i64, String>,
    /// The labels of marks that turned out to be another note's.
    note_aliases: std::collections::HashMap<String, String>,
    /// The notes with a text and no mark yet, with their numbers, in the
    /// order they're read: a number may have several.
    unmarked_notes: Vec<(i64, String)>,
    /// Each label in a note, and that note's.
    note_labels: std::collections::HashMap<String, String>,
    /// How many headings have been read.
    headings: usize,
    /// Each label on a heading, and that heading's index among them.
    labels: std::collections::HashMap<String, usize>,
    /// Whether what's read is a heading's text.
    in_heading: bool,
    /// The labels in the heading being read.
    heading_labels: Vec<String>,
    /// What's defined so far, by name, as written: `None` for what can't be
    /// expanded (see [`Reader::define`]).
    definitions: std::collections::HashMap<String, Option<Rc<Macro>>>,
    /// How to undo each local definition, in order, for the groups still
    /// open.
    definition_log: Vec<UndoDefinition>,
    /// When each global definition's name was last defined so, which the
    /// groups around it don't undo.
    global_definitions: std::collections::HashMap<String, usize>,
    /// How many definitions the document has made so far.
    definitions_made: usize,
    /// How many macros the one being read is called from.
    expansion_depth: usize,
    /// The bytes of macros the document may still read.
    expansion_left: usize,
    /// How much deeper macros may nest, within the stack [`read`] sized for
    /// the source, which also holds what's around their calls.
    nesting_left: usize,
    /// The calls [`Reader::expand`] left.
    unexpanded: usize,
    /// Whether the document's class adds each `\\author` to those before.
    authors_add: bool,
    /// What the document says of itself (see [`Reader::preamble`]).
    meta: Meta,
    /// The tables too sparse to keep, read as their text.
    sparse_tables: usize,
    /// How many columns the table cell being read spans.
    cell_span: usize,
    /// The float's last caption, if it has one.
    caption: Option<Vec<Inline>>,
    /// Whether what's read is a link's text (see [`Reader::href`]).
    in_link: bool,
    /// Whether what's read is an unknown command's: nothing in it is joined.
    literal: bool,
    /// Whether it's in a typewriter font, which joins nothing either.
    typewriter: bool,
    /// The font at each `\\begingroup` or `\\bgroup` still open, which their
    /// ends set back.
    groups: Vec<Group>,
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
                        self.define(&name, &mut items, &mut at, &mut unbracketed);
                        self.non_space();
                    }
                    SyntaxKind::ItemCmd => {
                        let name = command_name(&node);
                        if self.expand(&name) {
                            continue;
                        }
                        if let Some(body) = self.macro_taking_arguments(&name) {
                            self.expand_call(&body, &mut items, &mut at, &mut unbracketed);
                            continue;
                        }
                        let signature = if name == self.verbatim_mark {
                            Some(PLACEHOLDER)
                        } else {
                            signature(&name)
                        };
                        let args = match signature {
                            Some(signature) => {
                                let taken = take_arguments(
                                    &mut items,
                                    &mut at,
                                    &mut unbracketed,
                                    signature,
                                );
                                let mut args = taken.required;
                                // The optional arguments that show.
                                if matches!(
                                    name.as_str(),
                                    "\\cite"
                                        | "\\hyperref"
                                        | "\\footnote"
                                        | "\\footnotemark"
                                        | "\\footnotetext"
                                ) {
                                    args.extend(taken.optional);
                                }
                                args
                            }
                            // Unknown: the groups right after it; a file's
                            // name, those it names (see `files_named`).
                            None => {
                                let most = files_named(&name).unwrap_or(usize::MAX);
                                let mut groups = Vec::new();
                                while let Some(Piece::Node(group)) = items.get(at)
                                    && group.kind() == SyntaxKind::ItemCurly
                                    && groups.len() < most
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
                    SyntaxKind::ItemEnv => {
                        let group = self.open_group();
                        match environment_name(&node).as_str() {
                            "itemize" => self.list(&node, ListKind::Itemize),
                            "enumerate" => self.list(&node, ListKind::Enumerate),
                            "description" => self.list(&node, ListKind::Description),
                            "quote" | "quotation" | "verse" => self.quote(&node),
                            "center" | "flushleft" | "flushright" => self.aligned(&node),
                            "figure" | "figure*" | "table" | "table*" => self.float(&node),
                            "tabular" | "longtable" => self.tabular(&node, false),
                            "tabular*" | "tabularx" => self.tabular(&node, true),
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
                            // `\\begin{bfseries}` is a declaration too.
                            name => {
                                if let Some(declared) = declared(&format!("\\{name}"), group.font) {
                                    self.set_font(declared);
                                }
                                self.flow(pieces(&node));
                            }
                        }
                        self.close_group(group);
                    }
                    SyntaxKind::ItemFormula => self.formula(&node),
                    SyntaxKind::ItemCurly => {
                        let group = self.open_group();
                        self.flow(pieces(&node));
                        self.close_group(group);
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
                    self.after_apostrophe = false;
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
            SyntaxKind::TokenWord if self.joins() => self.text(&ligatures(text)),
            SyntaxKind::TokenApostrophe if self.joins() => self.apostrophe(),
            _ => self.text(text),
        }
    }

    /// Whether what's read is set in a font that joins characters.
    fn joins(&self) -> bool {
        !self.literal && !self.typewriter
    }

    /// The font what's read is set in. A declaration holds to the end of
    /// its group or environment, which set it back with [`Reader::set_font`].
    fn font(&self) -> Font {
        Font {
            bold: self.bold,
            emphasized: self.emphasized,
            typewriter: self.typewriter,
        }
    }

    fn set_font(&mut self, font: Font) {
        (self.bold, self.emphasized, self.typewriter) =
            (font.bold, font.emphasized, font.typewriter);
    }

    /// `'`: a closing quote, and with one right before it, a double one,
    /// as TeX's fonts join them. The parser gives each on its own, and
    /// anything between them keeps them apart.
    fn apostrophe(&mut self) {
        let joins = self.after_apostrophe;
        self.non_space();
        match self.run().last_mut() {
            Some(Inline::Text(last)) if joins => {
                last.pop();
                last.push('”');
            }
            _ => self.text("’"),
        }
        self.after_apostrophe = !joins;
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
        if let Some(symbol) = symbol(name) {
            self.text(symbol);
            // Text clears it: the spaces after a control word still go.
            self.skip_spaces = name[1..].chars().all(|c| c.is_ascii_alphabetic());
            return;
        }
        if let Some(font) = declared(name, self.font()) {
            return self.set_font(font);
        }
        if name == "\\textsuperscript"
            && let [arg] = &args[..]
            && let Some(label) = referred_label(arg)
        {
            return self.bare_reference(label);
        }
        if matches!(name, "\\begingroup" | "\\bgroup") {
            let group = self.open_group();
            return self.groups.push(group);
        }
        if matches!(name, "\\endgroup" | "\\egroup") {
            if let Some(group) = self.groups.pop() {
                self.close_group(group);
            }
            return;
        }
        if let Some(font) = font(name, self.font()) {
            // What's after it reads on as after its argument: the spaces
            // after a control space there go.
            if let Some(arg) = args.into_iter().next() {
                self.in_font(arg, font);
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
            "\\url" => {
                if let Some(arg) = args.first() {
                    let url = literal_text(arg);
                    self.url(url);
                }
            }
            "\\href" => {
                if let [url, content] = &args[..] {
                    self.href(literal_text(url), content.clone());
                }
            }
            "\\cite" => self.cite(args),
            "\\title" => {
                if let Some(arg) = args.into_iter().next() {
                    let title = self.field(arg);
                    self.meta.title = Some(title).filter(|title| !title.is_empty());
                }
            }
            "\\date" => {
                if let Some(arg) = args.into_iter().next() {
                    let date = self.field(arg);
                    self.meta.date = Some(date).filter(|date| !date.is_empty());
                }
            }
            "\\author" => {
                if let Some(arg) = args.into_iter().next() {
                    self.authors(arg);
                }
            }
            // The cell it's all of spans its count of columns.
            "\\multicolumn" => {
                if let [count, _, content] = &args[..] {
                    self.cell_span = literal_text(count).parse().unwrap_or(1);
                    self.in_font(content.clone(), self.font());
                }
            }
            "\\multirow" => {
                if let Some(content) = args.into_iter().nth(2) {
                    self.in_font(content, self.font());
                }
            }
            "\\includegraphics" => {
                if let Some(arg) = args.first() {
                    self.inline(Inline::Image {
                        url: literal_text(arg),
                        title: None,
                        alt: String::new(),
                    });
                }
            }
            "\\caption" => {
                if let Some(arg) = args.into_iter().next() {
                    self.caption(arg);
                }
            }
            // A number given is no count.
            "\\footnote" => {
                if args.len() == 1 {
                    self.footnote_count = self.footnote_count.saturating_add(1);
                }
                let number = self.note_number(args.get(1));
                if let Some(arg) = args.into_iter().next() {
                    self.footnote(number, arg);
                }
            }
            "\\footnotemark" => {
                if args.is_empty() {
                    self.footnote_count = self.footnote_count.saturating_add(1);
                }
                let number = self.note_number(args.first());
                self.footnotemark(number, !args.is_empty());
            }
            "\\footnotetext" => {
                let number = self.note_number(args.get(1));
                if let Some(arg) = args.into_iter().next() {
                    self.footnotetext(number, arg);
                }
            }
            "\\label" => {
                if let Some(arg) = args.first() {
                    self.label(literal_text(arg));
                }
            }
            "\\ref" | "\\eqref" | "\\autoref" | "\\nameref" => {
                if let Some(arg) = args.first() {
                    self.reference(literal_text(arg));
                }
            }
            "\\cref" | "\\Cref" => {
                if let Some(arg) = args.first() {
                    for (i, label) in literal_text(arg).split(',').enumerate() {
                        if i > 0 {
                            self.text(", ");
                        }
                        self.reference(label.trim().to_string());
                    }
                }
            }
            "\\hyperref" => {
                let mut args = args.into_iter();
                let content = args.next().unwrap_or_default();
                match args.next() {
                    Some(label) => {
                        let url = format!("{REFERENCE_LINK}{}", literal_text(&label));
                        self.href(url, content);
                    }
                    None => self.in_font(content, self.font()),
                }
            }
            "\\texttt" => {
                if let Some(arg) = args.into_iter().next() {
                    self.typewriter(arg);
                }
            }
            "\\newline" => {
                self.inline(Inline::LineBreak);
                self.skip_spaces = true;
            }
            // What's for a PDF's bookmarks goes.
            "\\texorpdfstring" => {
                if let Some(tex) = args.into_iter().next() {
                    self.in_font(tex, self.font());
                }
            }
            // What a list starts with (see `Reader::list`): elsewhere, it
            // shows nothing.
            // A line, which no writer's text can hold, outside of a block
            // of its own (see `Reader::aligned`).
            "\\tightlist" | "\\rule" | "\\nocite" | "\\pageref" | "\\centering" | "\\thanks" => {}
            // A file's name, where the file wasn't read in (see `inputs`).
            _ if files_named(name).is_some() => {}
            // No writer shows colors: the text, in a group of its own, and
            // a color declared shows nothing. The text isn't an argument
            // that keeps it inline: a display formula in it is one still.
            "\\textcolor" => {
                if let Some(text) = args.into_iter().nth(1) {
                    self.flow_groups(vec![text]);
                }
            }
            "\\color" => {}
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
                self.flow_groups(args);
            }
            // A control space (`\ `, or `\` at a line's end) is a space of
            // its own, and the spaces after it go.
            "\\" => {
                self.text(" ");
                self.skip_spaces = true;
            }
            // Characters LaTeX gives meanings, escaped.
            _ if ESCAPED.contains(&name) => self.text(&name[1..]),
            // Unknown commands keep their groups' text, as pandoc keeps it,
            // and as it's written: `\texttt`'s font and `\url`'s address
            // join nothing.
            _ => {
                let outer = std::mem::replace(&mut self.literal, true);
                self.flow_groups(args);
                self.literal = outer;
            }
        }
    }

    /// `groups`, each in a group of its own (see [`Reader::font`]).
    fn flow_groups(&mut self, groups: Vec<Vec<Piece>>) {
        for pieces in groups {
            let group = self.open_group();
            self.flow(pieces);
            self.close_group(group);
        }
    }

    /// Opens a group: what's declared and defined in it holds to
    /// [`Reader::close_group`].
    fn open_group(&self) -> Group {
        Group {
            font: self.font(),
            definitions: self.definition_log.len(),
        }
    }

    /// Closes `group`, setting back what was declared and defined in it,
    /// but for what was since defined globally. A `\\begingroup` may outlast
    /// the group it's in, which has set back what it defined already.
    fn close_group(&mut self, group: Group) {
        self.set_font(group.font);
        let start = group.definitions.min(self.definition_log.len());
        for undo in self.definition_log.drain(start..).rev() {
            let global = self.global_definitions.get(&undo.name);
            if global.is_some_and(|global| *global > undo.made) {
                continue;
            }
            match undo.earlier {
                Some(earlier) => self.definitions.insert(undo.name, earlier),
                None => self.definitions.remove(&undo.name),
            };
        }
    }

    /// Takes the definition at `items[*at..]`. It holds to the end of its
    /// group, or of the document with `\\gdef` and `\\xdef`, from where the
    /// reader expands what's called (see [`Reader::expand`]). What it can't
    /// expand (see [`Definition`], [`Macro::new`]) is an unknown command, as
    /// is what `\\edef` and `\\xdef` define, which would take the meanings of
    /// the commands in it as they were here. What the reader knows keeps its
    /// meaning: a `\\section` redefined in terms of the class's insides still
    /// heads a section.
    fn define(
        &mut self,
        command: &str,
        items: &mut [Piece],
        at: &mut usize,
        unbracketed: &mut usize,
    ) {
        let Some(definition) = take_definition(items, at, unbracketed, command) else {
            return;
        };
        let Definition {
            name,
            body,
            parameters,
            default,
            starred,
        } = definition;
        // Writers know more commands than the reader does.
        let provided_already = command == "\\providecommand"
            && (self.definitions.contains_key(&name) || known_in_formulas(&name));
        if provided_already || signature(&name).is_some() {
            return;
        }
        let expanded_here = matches!(command, "\\edef" | "\\xdef");
        let body = body
            .filter(|_| !expanded_here)
            .and_then(|body| Macro::new(body, parameters, default))
            .map(|mut body| {
                if command == "\\DeclareMathOperator" {
                    let star = if starred { "*" } else { "" };
                    body.source = format!("\\operatorname{star}{{{}}}", body.source);
                }
                Rc::new(body)
            });
        let earlier = self.definitions.insert(name.clone(), body);
        self.definitions_made += 1;
        let made = self.definitions_made;
        if matches!(command, "\\gdef" | "\\xdef") {
            self.global_definitions.insert(name, made);
        } else {
            self.definition_log.push(UndoDefinition {
                name,
                earlier,
                made,
            });
        }
    }

    /// `items`, an environment's, with the macros called among them spliced
    /// in, and whether any was. A table's and a list's rows, cells and items
    /// are found before what's in them is read, and a macro can hold those.
    /// What's read inside them doesn't splice again, as it's macros' text at
    /// any depth, which [`Reader::expand`] bounds as it reads.
    fn spliced(&mut self, items: Vec<Piece>) -> (Vec<Piece>, bool) {
        if self.expansion_depth > 0 || self.definitions.is_empty() {
            return (items, false);
        }
        let left = self.expansion_left;
        let mut spliced = Vec::with_capacity(items.len());
        self.splice(&items, &mut spliced, 0);
        let any = self.expansion_left != left;
        (spliced, any)
    }

    /// Appends `items` to `spliced`, with macros' pieces for their calls, as
    /// [`Reader::expand`] would read them, and the spaces after each gone.
    /// A call that's a command's argument stays, for the command to take
    /// whole. A definition among them, in a group too, may change what's
    /// called after it, so from one on they're left to be read in order, and
    /// this says so with `false`.
    fn splice(&mut self, items: &[Piece], spliced: &mut Vec<Piece>, depth: usize) -> bool {
        let mut at = 0;
        // How many of what's next are a command's arguments.
        let mut arguments = 0;
        while let Some(piece) = items.get(at) {
            at += 1;
            if holds_definition(piece) {
                spliced.extend_from_slice(&items[at - 1..]);
                return false;
            }
            if arguments > 0 {
                let space = matches!(
                    piece,
                    Piece::Token(SyntaxKind::TokenWhiteSpace | SyntaxKind::TokenLineBreak, _)
                );
                arguments -= usize::from(!space);
                spliced.push(piece.clone());
                continue;
            }
            let name = match piece {
                Piece::Node(node) if node.kind() == SyntaxKind::ItemCmd => command_name(node),
                _ => String::new(),
            };
            arguments = signature(&name).map_or(0, |signature| signature.required);
            let body = self.macro_named(&name);
            let Some(body) = body.filter(|body| self.can_expand(body, depth)) else {
                spliced.push(piece.clone());
                continue;
            };
            self.expansion_left -= body.length + 1;
            if !self.splice(&body.pieces, spliced, depth + 1) {
                spliced.extend_from_slice(&items[at..]);
                return false;
            }
            at = past_spaces(items, at);
        }
        true
    }

    /// What `name` stands for, if it's a macro taking no arguments, which
    /// is read from its pieces.
    fn macro_named(&self, name: &str) -> Option<Rc<Macro>> {
        let body = self.definitions.get(name).cloned().flatten();
        body.filter(|body| body.parameters == 0)
    }

    /// What `name` stands for, if it's a macro taking arguments, which is
    /// read from its source with them in it (see [`Reader::expand_call`]).
    fn macro_taking_arguments(&self, name: &str) -> Option<Rc<Macro>> {
        let body = self.definitions.get(name).cloned().flatten();
        body.filter(|body| body.parameters > 0)
    }

    /// Reads a call of `body`, a macro taking arguments, with those it
    /// takes from `items[*at..]`: its source with them in it, parsed again,
    /// as [`Reader::expand`] reads one taking none, and within the same
    /// bounds. Short of arguments, or past those bounds, it's an unknown
    /// command, and what it took shows as its text.
    fn expand_call(
        &mut self,
        body: &Macro,
        items: &mut [Piece],
        at: &mut usize,
        unbracketed: &mut usize,
    ) {
        let optional = body.default.is_some();
        let signature = Signature {
            optional,
            required: body.parameters - usize::from(optional),
            ..Signature::default()
        };
        let taken = take_arguments(items, at, unbracketed, signature);
        let last_is_control_word = matches!(taken.required.last().map(Vec::as_slice),
            Some([Piece::Node(node)]) if node.kind() == SyntaxKind::ItemCmd
                && command_name(node)[1..].starts_with(|c: char| c.is_ascii_alphabetic()));
        let mut arguments = Vec::with_capacity(body.parameters);
        if let Some(default) = &body.default {
            let given = taken.optional.as_deref().map(source_text);
            arguments.push(given.unwrap_or_else(|| default.clone()));
        }
        arguments.extend(taken.required.iter().map(|argument| source_text(argument)));
        if arguments.len() < body.parameters {
            return self.unexpanded_call(taken);
        }
        let text = if self.expansion_depth < MAX_EXPANSION_DEPTH {
            macros::substituted(&body.source, &arguments, self.expansion_left)
        } else {
            None
        };
        // As the document's source is, before it's parsed.
        let text = text.map(|text| formulas::separate_formulas(&formulas::close_scripts(&text)));
        let nesting = text.as_deref().map_or(usize::MAX, nesting_bound);
        let Some(text) = text.filter(|_| nesting <= self.nesting_left) else {
            self.unexpanded += 1;
            return self.unexpanded_call(taken);
        };
        self.expansion_left = self.expansion_left.saturating_sub(text.len() + CALL_COST);
        let root = mitex_parser::parse_without_macro(&text, CommandSpec::new(Default::default()));
        let mut expanded = pieces(&root);
        let last_command = take_last_command(&mut expanded, |name| self.required_arguments(name));
        self.nesting_left -= nesting;
        self.expansion_depth += 1;
        self.non_space();
        self.flow(expanded);
        self.expansion_depth -= 1;
        self.nesting_left += nesting;
        // Spaces after its last argument are read, but for those after a
        // control word, which TeX doesn't see.
        self.skip_spaces = last_is_control_word;
        // A command it ends in takes its arguments from what's after the
        // call, as in TeX: it's read next, in place of what the call took.
        // Read there, not in it, a chain of such calls costs no stack, and
        // what it may expand to still bounds it.
        if let Some(command) = last_command {
            *at -= 1;
            items[*at] = command;
        }
    }

    /// How many arguments the command `name` must take as it's read, of a
    /// command the reader knows or a macro, past an optional first one. An
    /// unknown one takes only the groups after it, none it must.
    fn required_arguments(&self, name: &str) -> usize {
        if let Some(signature) = signature(name) {
            return signature.required;
        }
        self.macro_taking_arguments(name).map_or(0, |body| {
            body.parameters - usize::from(body.default.is_some())
        })
    }

    /// What a call that isn't expanded took, as an unknown command's text.
    fn unexpanded_call(&mut self, taken: Arguments) {
        let mut groups = Vec::new();
        if let Some(optional) = taken.optional {
            let mut bracketed = vec![Piece::Token(SyntaxKind::TokenLBracket, "[".into())];
            bracketed.extend(optional);
            bracketed.push(Piece::Token(SyntaxKind::TokenRBracket, "]".into()));
            groups.push(bracketed);
        }
        groups.extend(taken.required);
        self.non_space();
        // As after any control word.
        self.skip_spaces = groups.is_empty();
        let outer = std::mem::replace(&mut self.literal, true);
        self.flow_groups(groups);
        self.literal = outer;
    }

    /// Whether `body` may be read `depth` macros deep (see
    /// [`Reader::expand`]).
    fn can_expand(&self, body: &Macro, depth: usize) -> bool {
        depth < MAX_EXPANSION_DEPTH
            && body.length < self.expansion_left
            && body.nesting <= self.nesting_left
    }

    /// Reads what `name` stands for, if it's a macro, and says whether it
    /// did. It doesn't past [`MAX_EXPANSION_DEPTH`], what the stack was
    /// sized for, or what's left of what the document may expand to, so
    /// that one that calls itself ends: that call is an unknown command,
    /// counted for a warning.
    fn expand(&mut self, name: &str) -> bool {
        let Some(body) = self.macro_named(name) else {
            return false;
        };
        if !self.can_expand(&body, self.expansion_depth) {
            self.unexpanded += 1;
            return false;
        }
        self.expansion_left -= body.length + 1;
        self.nesting_left -= body.nesting;
        self.expansion_depth += 1;
        self.non_space();
        self.flow(body.pieces.clone());
        self.expansion_depth -= 1;
        self.nesting_left += body.nesting;
        // As after any control word.
        self.skip_spaces = true;
        true
    }

    /// Sets the counter of an `enumerate` the text is in to `args`' value.
    /// That of one it isn't in does nothing: the next one starts over.
    fn set_counter(&mut self, args: Vec<Vec<Piece>>) {
        let [name, value] = &args[..] else {
            return;
        };
        if source_text(name).trim() == "footnote" {
            if let Ok(value) = source_text(value).trim().parse() {
                self.footnote_count = value;
            }
            return;
        }
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
        let tex = self.expanded_math(tex);
        let tex = without_labels(&tex);
        let tex = tex.as_str();
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

    /// `\texttt`'s argument: code, if all it added is text, as the LaTeX
    /// writer writes code. Otherwise what it holds stays where it was read,
    /// which no writer shows as typewriter.
    fn typewriter(&mut self, arg: Vec<Piece>) {
        let blocks = self.blocks.len();
        let run = self.run();
        let inlines = run.len();
        let text = match run.last() {
            Some(Inline::Text(text)) => Some(text.len()),
            _ => None,
        };
        let outer = self.outer_lengths();
        let font = Font {
            typewriter: true,
            ..self.font()
        };
        self.in_font(arg, font);
        // A font changed in it may leave what's after in another run, and
        // a section in it clears a paragraph that shows nothing.
        if self.blocks.len() > blocks || self.outer_lengths() != outer {
            return;
        }
        let run = self.run();
        // `None` where a cleared paragraph left fewer than there were.
        let added = run.len().checked_sub(inlines);
        let code = match (run.last_mut(), text) {
            (Some(Inline::Text(last)), Some(text))
                if added == Some(0) && last.len() > text && last.is_char_boundary(text) =>
            {
                last.split_off(text)
            }
            (Some(Inline::Text(_)), None) if added == Some(1) => {
                let Some(Inline::Text(code)) = run.pop() else {
                    unreachable!("the last inline is text");
                };
                code
            }
            _ => return,
        };
        // Right after other code, it's one with it, as in `verbatim`. It's
        // pushed as it is: spaces after what ended the argument still go.
        match run.last_mut() {
            Some(Inline::Code(before)) => before.push_str(&code),
            _ => run.push(Inline::Code(code)),
        }
    }

    /// The lengths of the inlines and runs [`Reader::run`] goes through to
    /// the run, not counting the run: while they stay, so does the run,
    /// and nothing's been read but into it. `None` if it's gone.
    fn outer_lengths(&self) -> Option<Vec<usize>> {
        let mut lengths = Vec::new();
        let mut run = &self.inlines;
        if self.bold {
            lengths.push(run.len());
            let Some(Inline::Strong(bold)) = run.last() else {
                return None;
            };
            run = bold;
        }
        if self.emphasized {
            lengths.push(run.len());
            if !matches!(run.last(), Some(Inline::Emph(_))) {
                return None;
            }
        }
        Some(lengths)
    }

    /// `\href`'s link to `url`, its text `content`, in the font around it,
    /// which goes in the link: it's outside any run, so that what's in it can
    /// leave that font. Spaces at its edges move out of it, as they do out of
    /// a run. One in another's text is that text, so links don't nest: no
    /// writer can show that, and dropping a document recurses as deep as they
    /// would go. Blocks in its text leave no link, only what it holds in
    /// order.
    fn href(&mut self, url: String, content: Vec<Piece>) {
        if self.in_link {
            return self.flow(content);
        }
        let outer = (
            std::mem::take(&mut self.blocks),
            std::mem::take(&mut self.inlines),
        );
        self.in_link = true;
        self.in_font(content, self.font());
        self.in_link = false;
        let blocks = std::mem::replace(&mut self.blocks, outer.0);
        let inlines = std::mem::replace(&mut self.inlines, outer.1);
        if !blocks.is_empty() {
            self.end_paragraph();
            self.blocks.extend(blocks);
            self.inlines = inlines;
            return;
        }
        let inlines = hoisted(inlines);
        let space_before =
            matches!(inlines.first(), Some(Inline::Text(text)) if text.starts_with(' '));
        let space_after = matches!(inlines.last(), Some(Inline::Text(text)) if text.ends_with(' '));
        let mut content = trimmed(inlines);
        // A note's mark is a link of its own in HTML, which links can't hold.
        let mut marks = Vec::new();
        take_note_marks(&mut content, &mut marks);
        if space_before {
            self.space();
        }
        if !content.is_empty() {
            self.non_space();
            self.inlines.push(Inline::Link {
                url,
                title: None,
                content,
            });
        }
        for mark in marks {
            self.inline(mark);
        }
        if space_after {
            self.space();
        }
    }

    /// `\url`'s link to `url`, in a link's text only its address.
    fn url(&mut self, url: String) {
        let text = Inline::Text(url.clone());
        if self.in_link {
            self.inline(text);
        } else if !url.is_empty() {
            self.inline(Inline::Link {
                url,
                title: None,
                content: vec![text],
            });
        }
    }

    /// `\footnote`'s note, marked where it's read. One showing nothing is
    /// none.
    fn footnote(&mut self, number: i64, arg: Vec<Piece>) {
        let label = self.new_note();
        let content = self.note_content(&label, arg);
        if !content.is_empty() {
            self.finished_notes.insert(number, label.clone());
            self.notes.push(Block::FootnoteDefinition {
                label: label.clone(),
                content,
            });
            self.inline(Inline::FootnoteRef(label));
        }
    }

    /// The number `\footnotemark` or `\footnotetext` is given, or, with none
    /// given, the count of notes: `\footnotemark` adds to it first.
    fn note_number(&mut self, given: Option<&Vec<Piece>>) -> i64 {
        given
            .and_then(|given| literal_text(given).parse().ok())
            .unwrap_or(self.footnote_count)
    }

    /// A mark for note `number`, whose text may be yet to come. With a
    /// number `given`, and no text by the end, it's the mark of the note of
    /// that number read before it (see [`Reader::close_notes`]). One with no
    /// number given is a note of its own, so it settles such a mark first.
    fn footnotemark(&mut self, number: i64, given: bool) {
        let unmarked = self
            .unmarked_notes
            .iter()
            .rposition(|(unmarked, _)| *unmarked == number);
        if let Some(unmarked) = unmarked {
            let label = self.unmarked_notes.remove(unmarked).1;
            return self.inline(Inline::FootnoteRef(label));
        }
        let settled = !given
            && self
                .unwritten_notes
                .get(&number)
                .is_some_and(|(_, earlier)| earlier.is_some());
        if settled && let Some((label, Some(note))) = self.unwritten_notes.remove(&number) {
            self.note_aliases.insert(label, note);
        }
        let label = match self.unwritten_notes.get(&number) {
            Some((label, _)) => label.clone(),
            None => {
                let label = self.new_note();
                let earlier = self.finished_notes.get(&number).filter(|_| given).cloned();
                self.unwritten_notes
                    .insert(number, (label.clone(), earlier));
                label
            }
        };
        self.inline(Inline::FootnoteRef(label));
    }

    /// The text of note `number`: of the mark read for it, if any, or for
    /// the one to come.
    fn footnotetext(&mut self, number: i64, arg: Vec<Piece>) {
        let marked = self.unwritten_notes.remove(&number).map(|(label, _)| label);
        let label = marked.clone().unwrap_or_else(|| self.new_note());
        let content = self.note_content(&label, arg);
        self.notes.push(Block::FootnoteDefinition {
            label: label.clone(),
            content,
        });
        self.finished_notes.insert(number, label.clone());
        if marked.is_none() {
            self.unmarked_notes.push((number, label));
        }
    }

    /// Settles the notes left half done, once all is read. No text is lost:
    /// a mark with no text is the mark of the note its number was given, or
    /// has an empty note, and a text with no mark gets one at the end.
    fn close_notes(&mut self) {
        self.end_paragraph();
        for (label, earlier) in std::mem::take(&mut self.unwritten_notes).into_values() {
            match earlier {
                Some(note) => {
                    self.note_aliases.insert(label, note);
                }
                None => self.notes.push(Block::FootnoteDefinition {
                    label,
                    content: Vec::new(),
                }),
            }
        }
        for (_, label) in std::mem::take(&mut self.unmarked_notes) {
            self.inlines.push(Inline::FootnoteRef(label));
        }
    }

    /// A label for a new note.
    fn new_note(&mut self) -> String {
        self.note_count += 1;
        self.note_count.to_string()
    }

    /// Note `label`'s blocks: read as a text of its own, in the normal font,
    /// whatever it's in. Labels in it are the note's.
    fn note_content(&mut self, label: &str, arg: Vec<Piece>) -> Vec<Block> {
        let blocks = std::mem::take(&mut self.blocks);
        let inlines = std::mem::take(&mut self.inlines);
        let group = self.open_group();
        let note = self.note.replace(label.to_string());
        let state = (
            self.in_argument,
            self.in_heading,
            self.in_link,
            self.literal,
            self.headings,
            self.breaks,
            self.in_comment,
            self.line_start,
            self.skip_spaces,
        );
        self.set_font(Font::default());
        (self.in_argument, self.in_heading, self.in_link) = (false, false, false);
        self.literal = false;
        (self.breaks, self.in_comment) = (0, false);
        (self.line_start, self.skip_spaces) = (true, false);
        self.flow(arg);
        self.end_paragraph();
        let content = std::mem::replace(&mut self.blocks, blocks);
        self.inlines = inlines;
        self.close_group(group);
        self.note = note;
        (
            self.in_argument,
            self.in_heading,
            self.in_link,
            self.literal,
            self.headings,
            self.breaks,
            self.in_comment,
            self.line_start,
            self.skip_spaces,
        ) = state;
        content
    }

    /// A reference that shows only what it goes to: our LaTeX writer's
    /// `\textsuperscript{\ref{label}}`, for a note referred to again.
    fn bare_reference(&mut self, label: String) {
        if self.in_link {
            self.text(&format!("[{label}]"));
        } else {
            self.inline(Inline::Link {
                url: format!("{REFERENCE_LINK}{label}"),
                title: None,
                content: Vec::new(),
            });
        }
    }

    /// `\\label{label}`, of the heading it's in or right after; of anything
    /// else, it's no label a reference can go to, as nothing else has an id.
    fn label(&mut self, label: String) {
        if let Some(note) = &self.note {
            self.note_labels.insert(label, note.clone());
            return;
        }
        if self.in_heading {
            self.heading_labels.push(label);
        } else if self.inlines.is_empty()
            && matches!(self.blocks.last(), Some(Block::Heading { .. }))
        {
            self.labels.insert(label, self.headings - 1);
        }
    }

    /// A reference to `label`, as it's written: its number isn't known here.
    /// One to a heading links to it (see [`resolve_references`]).
    fn reference(&mut self, label: String) {
        let text = Inline::Text(format!("[{label}]"));
        if self.in_link {
            self.inline(text);
        } else {
            self.inline(Inline::Link {
                url: format!("{REFERENCE_LINK}{label}"),
                title: None,
                content: vec![text],
            });
        }
    }

    /// `\\cite`'s keys, and its note if it has one, in brackets: no writer
    /// has citations, and without the bibliography there's nothing else to
    /// show.
    fn cite(&mut self, args: Vec<Vec<Piece>>) {
        let mut args = args.into_iter();
        let keys = args.next().unwrap_or_default();
        let keys = literal_text(&keys);
        let keys: Vec<&str> = keys.split(',').map(str::trim).collect();
        let note = args.next().filter(|note| !note.is_empty());
        self.text(&format!("[{}", keys.join(", ")));
        if let Some(note) = note {
            self.text(", ");
            self.in_font(note, self.font());
        }
        self.text("]");
    }

    /// A `tabular`, with a width before its column specification if it
    /// `has_width`: a table, if it has any rows. The first row is its header
    /// if a rule ends it, as no writer sets a table without a header apart.
    fn tabular(&mut self, env: &SyntaxNode, has_width: bool) {
        self.end_paragraph();
        let (items, spliced) = self.spliced(pieces(env));
        self.expansion_depth += usize::from(spliced);
        self.table(items, has_width);
        self.expansion_depth -= usize::from(spliced);
    }

    /// The table `items`, a `tabular`'s, make (see [`Reader::tabular`]).
    fn table(&mut self, mut items: Vec<Piece>, has_width: bool) {
        let (mut at, mut unbracketed) = (1, 0);
        if has_width {
            let width = Signature {
                required: 1,
                ..Signature::default()
            };
            take_arguments(&mut items, &mut at, &mut unbracketed, width);
        }
        let specification = take_arguments(&mut items, &mut at, &mut unbracketed, TABULAR).required;
        let mut align = specification
            .first()
            .map(|columns| columns_of(&source_text(columns)))
            .unwrap_or_default();
        let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
        // A `longtable`'s rows up to where it says its head ends.
        let mut head = None;
        let (mut foot, mut foot_is_last) = (Vec::new(), false);
        let mut header_rule = false;
        let mut after_caption = false;
        let mut row: Vec<Vec<Piece>> = Vec::new();
        let mut cell: Vec<Piece> = Vec::new();
        while let Some(piece) = items.get(at).cloned() {
            at += 1;
            let name = match &piece {
                Piece::Node(node) if node.kind() == SyntaxKind::ItemCmd => command_name(node),
                _ => String::new(),
            };
            match &piece {
                Piece::Node(node) if node.kind() == SyntaxKind::ItemEnd => break,
                Piece::Token(SyntaxKind::TokenAmpersand, _) => {
                    row.push(std::mem::take(&mut cell));
                }
                Piece::Token(SyntaxKind::ItemNewLine, _) => {
                    take_arguments(&mut items, &mut at, &mut unbracketed, LINE_BREAK);
                    row.push(std::mem::take(&mut cell));
                    self.table_row(std::mem::take(&mut row), align.len(), &mut rows);
                    if std::mem::take(&mut after_caption)
                        && matches!(rows.last().map(Vec::as_slice), Some([only]) if only.is_empty())
                    {
                        rows.pop();
                    }
                }
                _ if name == "\\tabularnewline" => {
                    take_arguments(&mut items, &mut at, &mut unbracketed, LINE_BREAK);
                    row.push(std::mem::take(&mut cell));
                    self.table_row(std::mem::take(&mut row), align.len(), &mut rows);
                    if std::mem::take(&mut after_caption)
                        && matches!(rows.last().map(Vec::as_slice), Some([only]) if only.is_empty())
                    {
                        rows.pop();
                    }
                }
                _ if RULES.contains(&name.as_str()) && row.is_empty() && is_blank(&cell) => {
                    cell.clear();
                    skip_rule_arguments(&name, &items, &mut at);
                    header_rule |= rows.len() == 1;
                }
                // A first page's head, then the head of the rest, the same.
                _ if name == "\\endfirsthead" && row.is_empty() && is_blank(&cell) => {
                    cell.clear();
                    header_rule = false;
                    head = Some(std::mem::take(&mut rows));
                }
                _ if name == "\\endhead" && row.is_empty() && is_blank(&cell) => {
                    cell.clear();
                    header_rule = false;
                    if head.is_none() {
                        head = Some(std::mem::take(&mut rows));
                    }
                    rows.clear();
                }
                // The foot of each page but the last is a repeat; the last's
                // ends the table, whatever order they're written in.
                _ if matches!(name.as_str(), "\\endfoot" | "\\endlastfoot")
                    && row.is_empty()
                    && is_blank(&cell) =>
                {
                    cell.clear();
                    header_rule = false;
                    let last = name == "\\endlastfoot";
                    if last || !foot_is_last {
                        foot = std::mem::take(&mut rows);
                    }
                    foot_is_last |= last;
                    rows.clear();
                }
                // A `longtable`'s caption is a row, before the table here.
                _ if name == "\\caption" && row.is_empty() && is_blank(&cell) => {
                    cell.clear();
                    let caption = take_arguments(&mut items, &mut at, &mut unbracketed, CAPTION);
                    if let Some(caption) = caption.required.into_iter().next() {
                        self.caption(caption);
                    }
                    after_caption = true;
                }
                _ => cell.push(piece),
            }
        }
        if !row.is_empty() || !is_blank(&cell) {
            row.push(cell);
            self.table_row(row, align.len(), &mut rows);
        }
        rows.append(&mut foot);
        if let Some(mut head) = head.filter(|head: &Vec<_>| !head.is_empty()) {
            header_rule = true;
            head.append(&mut rows);
            rows = head;
        }
        if rows.is_empty() {
            return;
        }
        for row in &mut rows {
            join_past_columns(row);
        }
        let longest = rows.iter().map(Vec::len).max().unwrap_or(0);
        let width = longest.max(align.len());
        let cells: usize = rows.iter().map(Vec::len).sum();
        let area = rows.len().saturating_mul(width);
        if area > MAX_SPARSE_AREA && area / MAX_SPARSE_RATIO > cells {
            self.sparse_tables += 1;
            for row in rows {
                self.table_text(row);
            }
            return;
        }
        align.resize(width, Align::Default);
        for row in &mut rows {
            row.resize(width, Vec::new());
        }
        let header = if header_rule {
            rows.remove(0)
        } else {
            Vec::new()
        };
        self.blocks.push(Block::Table(Table {
            align,
            header,
            rows,
        }));
    }

    /// `row`, a table's that's too sparse to keep, as a paragraph of its
    /// cells.
    fn table_text(&mut self, row: Vec<Vec<Inline>>) {
        let mut content = Vec::new();
        for cell in row.into_iter().filter(|cell| !cell.is_empty()) {
            push_apart(&mut content, cell);
        }
        if !content.is_empty() {
            self.blocks.push(Block::Paragraph(content));
        }
    }

    /// Adds to `rows` the cells of `row`, at most `columns` for what a cell
    /// spans.
    fn table_row(
        &mut self,
        row: Vec<Vec<Piece>>,
        columns: usize,
        rows: &mut Vec<Vec<Vec<Inline>>>,
    ) {
        let mut cells = Vec::with_capacity(row.len());
        for cell in row {
            // A table in the cell has spans of its own.
            let outer = std::mem::replace(&mut self.cell_span, 1);
            let content = self.table_cell(cell);
            let span = std::mem::replace(&mut self.cell_span, outer);
            let span = span.min(columns.saturating_sub(cells.len()));
            cells.push(content);
            if span > 1 {
                cells.resize(cells.len() + span - 1, Vec::new());
            }
        }
        rows.push(cells);
    }

    /// A cell's inlines: a group, so what it declares ends with it. It holds
    /// no blocks, so those in it come first, each apart, as inlines.
    fn table_cell(&mut self, cell: Vec<Piece>) -> Vec<Inline> {
        let blocks = std::mem::take(&mut self.blocks);
        let inlines = std::mem::take(&mut self.inlines);
        self.in_font(cell, self.font());
        let content = trimmed(std::mem::replace(&mut self.inlines, inlines));
        let mut inner = std::mem::replace(&mut self.blocks, blocks);
        let mut flat = Vec::new();
        walk::for_each_block(&mut inner, &mut |block| flatten_block(block, &mut flat));
        push_apart(&mut flat, content);
        flat
    }

    /// What the preamble of `root` says of the document, read as the
    /// document's own commands would be: past what definitions hold, and up
    /// to `\\begin{document}`.
    fn preamble(&mut self, root: &SyntaxNode) {
        let mut items = pieces(root);
        let (mut at, mut unbracketed) = (0, 0);
        while let Some(item) = items.get(at).cloned() {
            at += 1;
            let Piece::Node(node) = item else {
                continue;
            };
            if node.kind() == SyntaxKind::ItemEnv && environment_name(&node) == "document" {
                return;
            }
            if node.kind() != SyntaxKind::ItemCmd {
                continue;
            }
            let name = command_name(&node);
            if DEFINITIONS.contains(&name.as_str()) {
                self.define(&name, &mut items, &mut at, &mut unbracketed);
            } else if name == "\\documentclass" {
                let class = take_arguments(&mut items, &mut at, &mut unbracketed, TABULAR).required;
                let class = class.first().map(|class| literal_text(class));
                self.authors_add = class.is_some_and(|class| {
                    AUTHORS_ADD.iter().any(|adding| class.starts_with(adding))
                });
            } else if matches!(name.as_str(), "\\title" | "\\author" | "\\date")
                && let Some(signature) = signature(&name)
            {
                let args = take_arguments(&mut items, &mut at, &mut unbracketed, signature);
                self.command(&name, args.required);
            }
        }
    }

    /// `arg`'s text: a field of the document's, read apart from its text.
    fn field(&mut self, arg: Vec<Piece>) -> String {
        inline_text(&self.table_cell(arg)).trim().to_string()
    }

    /// The authors `arg` names, one for each stretch between its `\\and`s,
    /// in place of those before it, or after them in a class that adds them.
    fn authors(&mut self, arg: Vec<Piece>) {
        if !self.authors_add {
            self.meta.authors.clear();
        }
        let is_and = |piece: &Piece| {
            matches!(piece, Piece::Node(node)
                if node.kind() == SyntaxKind::ItemCmd && command_name(node) == "\\and")
        };
        for author in arg.split(is_and) {
            let author = self.field(author.to_vec());
            if !author.is_empty() {
                self.meta.authors.push(author);
            }
        }
    }

    /// A `figure` or `table`: its blocks, where it's written, as no writer
    /// floats them. An image and a caption alone are one image, with the
    /// caption for its alt text, as writers take a figure.
    fn float(&mut self, env: &SyntaxNode) {
        self.end_paragraph();
        let mut items = pieces(env);
        let (mut at, mut unbracketed) = (0, 0);
        if matches!(items.first(), Some(Piece::Node(node)) if node.kind() == SyntaxKind::ItemBegin)
        {
            at = 1;
            take_arguments(&mut items, &mut at, &mut unbracketed, ITEM);
        }
        let outer = std::mem::take(&mut self.blocks);
        let caption = self.caption.take();
        self.flow(items.split_off(at));
        self.end_paragraph();
        let blocks = std::mem::replace(&mut self.blocks, outer);
        let caption = std::mem::replace(&mut self.caption, caption);
        self.blocks.extend(captioned(blocks, caption));
    }

    /// A caption: a paragraph of its own, in the font around it.
    fn caption(&mut self, arg: Vec<Piece>) {
        self.end_paragraph();
        self.in_font(arg, self.font());
        let content = trimmed(std::mem::take(&mut self.inlines));
        if !content.is_empty() {
            self.caption = Some(content.clone());
            self.blocks.push(Block::Paragraph(content));
        }
    }

    /// `arg`, an argument, in `font`.
    fn in_font(&mut self, arg: Vec<Piece>, font: Font) {
        let group = self.open_group();
        let in_argument = std::mem::replace(&mut self.in_argument, true);
        self.set_font(font);
        self.flow(arg);
        self.in_argument = in_argument;
        self.close_group(group);
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
        let (items, spliced) = self.spliced(pieces(env));
        self.expansion_depth += usize::from(spliced);
        self.list_of(items, kind);
        self.expansion_depth -= usize::from(spliced);
    }

    /// The list `pieces`, a list environment's, make (see [`Reader::list`]).
    fn list_of(&mut self, pieces: Vec<Piece>, kind: ListKind) {
        let numbered = kind == ListKind::Enumerate;
        self.end_paragraph();
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
                    let bold = Font {
                        bold: true,
                        ..self.font()
                    };
                    self.in_font(label, bold);
                } else {
                    self.flow_groups(vec![label]);
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
        let group = self.open_group();
        self.emphasized = false;
        let in_argument = std::mem::replace(&mut self.in_argument, true);
        let in_heading = std::mem::replace(&mut self.in_heading, true);
        self.flow(arg);
        self.in_heading = in_heading;
        self.in_argument = in_argument;
        self.close_group(group);
        let content = trimmed(std::mem::take(&mut self.inlines));
        let labels = std::mem::take(&mut self.heading_labels);
        if !content.is_empty() {
            // Seven of them, so an index fits.
            let level = level as u8 + 1;
            self.blocks.push(Block::Heading { level, content });
            self.labels
                .extend(labels.into_iter().map(|label| (label, self.headings)));
            self.headings += 1;
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
        self.after_apostrophe = false;
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

/// How deep macros may call macros: deeper than documents do, and an end to
/// one that calls itself.
const MAX_EXPANSION_DEPTH: usize = 32;

/// The bytes of macros a document may read at least, however short it is
/// (see [`EXPANSION_PER_BYTE`]).
const MIN_EXPANSION: usize = 1 << 16;

/// The bytes of macros a document may read for each of its own: well past
/// what documents do, and an end to macros calling others twice over, which
/// double at each level.
const EXPANSION_PER_BYTE: usize = 4;

/// What a call of a macro taking arguments costs of what a document may
/// expand to, besides the bytes it expands to: parsing them again costs
/// more than their bytes, and a short one that calls itself would cost
/// many times what reading the document does.
const CALL_COST: usize = 32;

/// What a group's end does to a definition made in it.
struct UndoDefinition {
    name: String,
    /// What it replaced: `None` if it wasn't defined.
    earlier: Option<Option<Rc<Macro>>>,
    /// How many definitions had been made with it.
    made: usize,
}

/// What a group sets back at its end (see [`Reader::open_group`]).
struct Group {
    font: Font,
    /// How long [`Reader::definition_log`] was at its start.
    definitions: usize,
}

/// The commands that define others (see [`Reader::define`]).
const DEFINITIONS: [&str; 8] = [
    "\\DeclareMathOperator",
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

/// `\\caption`'s.
const CAPTION: Signature = Signature {
    star: true,
    optional: true,
    unstarred_optional: false,
    required: 1,
};

/// `tabular`'s, after a width: its position and columns.
const TABULAR: Signature = Signature {
    star: false,
    optional: true,
    unstarred_optional: false,
    required: 1,
};

/// `\\`'s: `\\*[1em]`.
const LINE_BREAK: Signature = Signature {
    star: true,
    optional: true,
    unstarred_optional: false,
    required: 0,
};

/// The font a command sets its argument in, in text set in `font`; `None`
/// for a command that sets none. `\\emph` switches between italic and
/// upright, so inside another it's upright: the reader nests no emphasis,
/// which writers can't set apart from the one around it (#126). No writer
/// shows the other families and shapes, which leave the font as it is, but
/// for leaving typewriter.
fn font(name: &str, font: Font) -> Option<Font> {
    Some(match name {
        "\\emph" => Font {
            emphasized: !font.emphasized,
            ..font
        },
        "\\textbf" => Font { bold: true, ..font },
        "\\textmd" => Font {
            bold: false,
            ..font
        },
        "\\textit" | "\\textsl" => Font {
            emphasized: true,
            ..font
        },
        "\\textup" => Font {
            emphasized: false,
            ..font
        },
        "\\textnormal" => Font::default(),
        "\\textrm" | "\\textsf" => Font {
            typewriter: false,
            ..font
        },
        "\\textsc" | "\\underline" | "\\mbox" => font,
        _ => return None,
    })
}

/// The label `arg`, an argument, refers to, if a `\\ref` is all it holds.
fn referred_label(arg: &[Piece]) -> Option<String> {
    let shown: Vec<&Piece> = arg
        .iter()
        .filter(|piece| {
            !matches!(
                piece,
                Piece::Token(
                    SyntaxKind::TokenWhiteSpace | SyntaxKind::TokenLBrace | SyntaxKind::TokenRBrace,
                    _
                )
            )
        })
        .collect();
    match shown[..] {
        [Piece::Node(command), Piece::Node(group)]
            if command.kind() == SyntaxKind::ItemCmd
                && command_name(command) == "\\ref"
                && group.kind() == SyntaxKind::ItemCurly =>
        {
            Some(literal_text(&pieces(group)))
        }
        _ => None,
    }
}

/// What a rule in a table is written with: they show nothing.
const RULES: [&str; 6] = [
    "\\hline",
    "\\cline",
    "\\toprule",
    "\\midrule",
    "\\bottomrule",
    "\\cmidrule",
];

/// The start of the names of the classes where each `\\author` adds one:
/// in the rest, as in `article`, it takes the place of the last.
const AUTHORS_ADD: [&str; 6] = [
    "amsart",
    "amsbook",
    "revtex",
    "lipics",
    "acmart",
    "elsarticle",
];

/// How many columns a table's specification gives at most: more than any
/// table has, and few enough that a repeat count can't take all the memory.
const MAX_COLUMNS: usize = 256;

/// How many cells a table may come to by evening out its rows however
/// empty they are: writers even them out too, so a table this short of
/// cells could write far more than its source holds.
const MAX_SPARSE_AREA: usize = 1 << 12;

/// How many times its cells a table's area may be, past [`MAX_SPARSE_AREA`].
const MAX_SPARSE_RATIO: usize = 16;

/// Joins the cells of `row` past [`MAX_COLUMNS`] to the last it keeps.
fn join_past_columns(row: &mut Vec<Vec<Inline>>) {
    if row.len() <= MAX_COLUMNS {
        return;
    }
    let past = row.split_off(MAX_COLUMNS);
    if let Some(last) = row.last_mut() {
        for cell in past {
            push_apart(last, cell);
        }
    }
}

/// Adds `more` to `inlines`, a space apart from what's there, on its line.
fn push_apart(inlines: &mut Vec<Inline>, more: Vec<Inline>) {
    if more.is_empty() {
        return;
    }
    if !matches!(inlines.last(), None | Some(Inline::LineBreak)) {
        push_text(inlines, " ");
    }
    for inline in more {
        match inline {
            Inline::Text(text) => push_text(inlines, &text),
            other => inlines.push(other),
        }
    }
}

/// Adds what `block` itself holds to `inlines`, what's nested in it being
/// walked on its own: a cell holds no blocks. Formulas and code go in line.
fn flatten_block(block: &mut Block, inlines: &mut Vec<Inline>) {
    match block {
        Block::Paragraph(content) | Block::Heading { content, .. } => {
            push_apart(inlines, std::mem::take(content));
        }
        Block::Code { code, .. } => {
            push_apart(inlines, vec![Inline::Code(std::mem::take(code))]);
        }
        Block::Math(tex) => push_apart(inlines, vec![Inline::Math(std::mem::take(tex))]),
        // A table in a cell sets its lines, as our LaTeX writer writes a
        // cell of several.
        Block::Table(table) => {
            let header = std::mem::take(&mut table.header);
            let rows = std::mem::take(&mut table.rows);
            let lines = std::iter::once(header)
                .chain(rows)
                .filter(|row| !row.is_empty());
            for (line, row) in lines.enumerate() {
                if line > 0 {
                    inlines.push(Inline::LineBreak);
                }
                for cell in row {
                    push_apart(inlines, cell);
                }
            }
        }
        _ => {}
    }
}

/// Whether `pieces` show nothing.
fn is_blank(pieces: &[Piece]) -> bool {
    pieces.iter().all(|piece| {
        matches!(
            piece,
            Piece::Token(
                SyntaxKind::TokenWhiteSpace | SyntaxKind::TokenLineBreak | SyntaxKind::TokenComment,
                _
            )
        )
    })
}

/// Past the arguments the rule `name` takes in `items`: `\cline` a group of
/// columns; `\cmidrule` a thickness, a trim and that; the other `booktabs`
/// rules a thickness; `\hline` none.
fn skip_rule_arguments(name: &str, items: &[Piece], at: &mut usize) {
    if name == "\\hline" {
        return;
    }
    if name != "\\cline" {
        skip_delimited(
            items,
            at,
            SyntaxKind::TokenLBracket,
            SyntaxKind::TokenRBracket,
        );
    }
    if name == "\\cmidrule" {
        skip_delimited(items, at, SyntaxKind::TokenLParen, SyntaxKind::TokenRParen);
    }
    if matches!(name, "\\cline" | "\\cmidrule")
        && let Some(group) = past_blanks(items, *at)
        && matches!(&items[group], Piece::Node(node) if node.kind() == SyntaxKind::ItemCurly)
    {
        *at = group + 1;
    }
}

/// Past what's between `open` and `close` in `items`, if that's next. With
/// no `close`, it's no argument, and all stays.
fn skip_delimited(items: &[Piece], at: &mut usize, open: SyntaxKind, close: SyntaxKind) {
    let is = |piece: Option<&Piece>, wanted: SyntaxKind| matches!(piece, Some(Piece::Token(kind, _)) if *kind == wanted);
    let Some(start) = past_blanks(items, *at).filter(|start| is(items.get(*start), open)) else {
        return;
    };
    if let Some(end) = (start..items.len()).find(|end| is(items.get(*end), close)) {
        *at = end + 1;
    }
}

/// Where in `items`, from `at`, what's not blank is.
fn past_blanks(items: &[Piece], at: usize) -> Option<usize> {
    (at..items.len()).find(|at| !is_blank(std::slice::from_ref(&items[*at])))
}

/// The columns of a table's `specification`, up to [`MAX_COLUMNS`]: `l`, `c`
/// and `r` as they say, and `p`, `m`, `b` and `X`, which fill their text,
/// to the left. `|` and what `@`, `!`, `>` and `<` put between are none, and
/// `*{3}{c}` repeats.
fn columns_of(specification: &str) -> Vec<Align> {
    let mut columns = Vec::new();
    let mut rest = specification;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        if columns.len() >= MAX_COLUMNS {
            break;
        }
        match c {
            'l' | 'p' | 'm' | 'b' | 'X' => columns.push(Align::Left),
            'c' => columns.push(Align::Center),
            'r' => columns.push(Align::Right),
            '*' => {
                let (count, after) = group_text(rest);
                let (repeated, after) = group_text(after);
                rest = after;
                let repeated = columns_of(repeated);
                let count: usize = count.trim().parse().unwrap_or(0);
                for _ in 0..count.min(MAX_COLUMNS) {
                    if columns.len() + repeated.len() > MAX_COLUMNS {
                        break;
                    }
                    columns.extend(&repeated);
                }
            }
            '{' => {
                rest = &rest[group_end(rest).unwrap_or(rest.len())..];
            }
            _ => {}
        }
    }
    columns
}

/// The group `text` starts with, what's in it and what's after it; with
/// none, nothing, and `text`.
fn group_text(text: &str) -> (&str, &str) {
    let Some(inside) = text.trim_start().strip_prefix('{') else {
        return ("", text);
    };
    match group_end(inside) {
        Some(end) => (&inside[..end - 1], &inside[end..]),
        None => (inside, ""),
    }
}

/// `blocks`, a float's, the two made one if they're an image with no alt
/// text and its `caption`. Alt text is plain, so a caption holding a note's
/// mark, which it would lose, stays.
fn captioned(blocks: Vec<Block>, caption: Option<Vec<Inline>>) -> Vec<Block> {
    let Some(caption) = caption else {
        return blocks;
    };
    let mut plain = caption.clone();
    let mut marks = Vec::new();
    take_note_marks(&mut plain, &mut marks);
    let image = match &blocks[..] {
        [Block::Paragraph(first), Block::Paragraph(second)] if *second == caption => first,
        [Block::Paragraph(first), Block::Paragraph(second)] if *first == caption => second,
        _ => return blocks,
    };
    match &image[..] {
        [Inline::Image { url, title, alt }] if alt.is_empty() && marks.is_empty() => {
            vec![Block::Paragraph(vec![Inline::Image {
                url: url.clone(),
                title: title.clone(),
                alt: inline_text(&plain),
            }])]
        }
        _ => blocks,
    }
}

/// Moves the marks of notes in `inlines`, a link's text or a caption, to
/// `marks`.
fn take_note_marks(inlines: &mut Vec<Inline>, marks: &mut Vec<Inline>) {
    inlines.retain_mut(|inline| match inline {
        Inline::FootnoteRef(_) => {
            marks.push(inline.clone());
            false
        }
        // One that may turn out to be a mark (see `Reader::bare_reference`).
        Inline::Link { content, .. } if content.is_empty() => {
            marks.push(inline.clone());
            false
        }
        Inline::Strong(run) | Inline::Emph(run) => {
            take_note_marks(run, marks);
            !run.is_empty()
        }
        _ => true,
    });
}

/// `tex` without its `\label`s, which show nothing, and which nothing goes
/// to: a formula has no id. A letter that would run into what's before it
/// stays apart from it.
fn without_labels(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let (mut copied, mut at) = (0, 0);
    while let Some((start, name)) = verbatim::next_command(&tex[at..], verbatim::tex_letter) {
        let command = at + start;
        at = command + name.len();
        if name != "\\label" {
            continue;
        }
        let after = tex[at..].trim_start();
        let Some(group) = after.strip_prefix('{').and_then(group_end) else {
            continue;
        };
        out.push_str(&tex[copied..command]);
        at = tex.len() - after.len() + 1 + group;
        copied = at;
        let letter = |c: char| c.is_ascii_alphabetic();
        if out.ends_with(letter) && tex[at..].starts_with(letter) {
            out.push(' ');
        }
    }
    out.push_str(&tex[copied..]);
    out
}

/// How long `text`, after a group's `{`, is to past its `}`. An escaped
/// brace ends nothing.
fn group_end(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '{' => depth += 1,
            '}' if depth == 0 => return Some(at + 1),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// What `pieces`, an argument that's no text, such as an address or a key,
/// say: as written, without the group [`formulas::close_scripts`] puts after
/// a script, and with what's escaped as itself.
fn literal_text(pieces: &[Piece]) -> String {
    let source = source_text(pieces).replace("^{}", "^").replace("_{}", "_");
    let mut text = String::with_capacity(source.len());
    let mut chars = source.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match chars.peek() {
            Some(&escaped) if c == '\\' && "%#&_$~^{}".contains(escaped) => {
                text.push(escaped);
                chars.next();
            }
            _ => text.push(c),
        }
    }
    text
}

/// What text is set in, of what writers show or what changes how it's read.
#[derive(Clone, Copy, Default)]
struct Font {
    bold: bool,
    emphasized: bool,
    typewriter: bool,
}

/// The font the declaration `name` makes of `font`; `None` for a command
/// that's none. `\\bf` and those of its age start from the normal font, as
/// LaTeX defines them, and `\\bfseries` and those of its time change one thing.
fn declared(name: &str, font: Font) -> Option<Font> {
    let normal = Font::default();
    Some(match name {
        "\\bf" => Font {
            bold: true,
            ..normal
        },
        "\\it" | "\\sl" => Font {
            emphasized: true,
            ..normal
        },
        "\\tt" => Font {
            typewriter: true,
            ..normal
        },
        "\\rm" | "\\sf" | "\\sc" | "\\normalfont" => normal,
        "\\em" => Font {
            emphasized: !font.emphasized,
            ..font
        },
        "\\bfseries" => Font { bold: true, ..font },
        "\\mdseries" => Font {
            bold: false,
            ..font
        },
        "\\itshape" | "\\slshape" => Font {
            emphasized: true,
            ..font
        },
        "\\upshape" | "\\scshape" => Font {
            emphasized: false,
            ..font
        },
        "\\ttfamily" => Font {
            typewriter: true,
            ..font
        },
        "\\rmfamily" | "\\sffamily" => Font {
            typewriter: false,
            ..font
        },
        _ => return None,
    })
}

/// Whether `name` is a font declaration (see [`declared`]).
fn is_declaration(name: &str) -> bool {
    declared(name, Font::default()).is_some()
}

/// `word`, a word of text, with what TeX's fonts join in it joined.
fn ligatures(word: &str) -> String {
    word.replace("---", "—")
        .replace("--", "–")
        .replace("``", "“")
        .replace('`', "‘")
}

/// What the command `name` sets, for those that stand for a character. The
/// spaces are pandoc's.
fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "\\textquotedblleft" => "“",
        "\\textquotedblright" => "”",
        "\\textquoteleft" => "‘",
        "\\textquoteright" => "’",
        "\\textquotesingle" => "'",
        "\\textasciigrave" => "`",
        "\\textendash" => "–",
        "\\textemdash" => "—",
        "\\textellipsis" | "\\ldots" | "\\dots" => "…",
        "\\textbackslash" => "\\",
        "\\textasciitilde" => "~",
        "\\textasciicircum" => "^",
        "\\textless" => "<",
        "\\textgreater" => ">",
        "\\textbar" => "|",
        "\\textbullet" => "•",
        "\\textperiodcentered" => "·",
        "\\textregistered" => "®",
        "\\texttrademark" => "™",
        "\\copyright" | "\\textcopyright" => "©",
        "\\S" | "\\textsection" => "§",
        "\\P" | "\\textparagraph" => "¶",
        "\\dag" | "\\textdagger" => "†",
        "\\ddag" | "\\textdaggerdbl" => "‡",
        "\\pounds" | "\\textsterling" => "£",
        "\\ss" => "ß",
        "\\ae" => "æ",
        "\\AE" => "Æ",
        "\\oe" => "œ",
        "\\OE" => "Œ",
        "\\aa" => "å",
        "\\AA" => "Å",
        "\\o" => "ø",
        "\\O" => "Ø",
        "\\l" => "ł",
        "\\L" => "Ł",
        "\\i" => "ı",
        "\\j" => "ȷ",
        "\\LaTeX" => "LaTeX",
        "\\TeX" => "TeX",
        "\\," | "\\thinspace" => "\u{2006}",
        "\\;" | "\\:" | "\\>" => "\u{2005}",
        "\\enspace" => "\u{2002}",
        "\\quad" => "\u{2003}",
        "\\qquad" => "\u{2003}\u{2003}",
        _ => return None,
    })
}

/// What a command the reader knows takes; `None` for one it doesn't, which
/// keeps the text of the groups right after it (see [`Reader::command`]).
/// Those that take nothing leave the groups after them to be read on.
fn signature(name: &str) -> Option<Signature> {
    match name {
        "\\ensuremath" | "\\texttt" => Some(Signature {
            required: 1,
            ..Signature::default()
        }),
        _ if font(name, Font::default()).is_some() => Some(Signature {
            required: 1,
            ..Signature::default()
        }),
        "\\par" | "\\" | "\\tightlist" | "\\newline" => Some(Signature::default()),
        "\\texorpdfstring" => Some(Signature {
            required: 2,
            ..Signature::default()
        }),
        // A color model in brackets, then the color.
        "\\textcolor" => Some(Signature {
            optional: true,
            required: 2,
            ..Signature::default()
        }),
        "\\color" => Some(Signature {
            optional: true,
            required: 1,
            ..Signature::default()
        }),
        "\\href" => Some(Signature {
            optional: true,
            required: 2,
            ..Signature::default()
        }),
        "\\footnote" | "\\footnotetext" => Some(Signature {
            optional: true,
            required: 1,
            ..Signature::default()
        }),
        "\\includegraphics" | "\\caption" => Some(Signature {
            star: true,
            optional: true,
            required: 1,
            ..Signature::default()
        }),
        "\\centering" => Some(Signature::default()),
        "\\multicolumn" => Some(Signature {
            required: 3,
            ..Signature::default()
        }),
        "\\multirow" => Some(Signature {
            optional: true,
            required: 3,
            ..Signature::default()
        }),
        "\\footnotemark" => Some(Signature {
            optional: true,
            ..Signature::default()
        }),
        "\\title" | "\\author" | "\\date" | "\\thanks" => Some(Signature {
            optional: true,
            required: 1,
            ..Signature::default()
        }),
        "\\url" | "\\nocite" | "\\label" | "\\eqref" | "\\pageref" | "\\nameref" => {
            Some(Signature {
                required: 1,
                ..Signature::default()
            })
        }
        "\\ref" | "\\autoref" | "\\cref" | "\\Cref" => Some(Signature {
            star: true,
            required: 1,
            ..Signature::default()
        }),
        "\\hyperref" => Some(Signature {
            optional: true,
            required: 1,
            ..Signature::default()
        }),
        "\\cite" => Some(Signature {
            star: true,
            optional: true,
            required: 1,
            ..Signature::default()
        }),
        _ if symbol(name).is_some() => Some(Signature::default()),
        "\\begingroup" | "\\endgroup" | "\\bgroup" | "\\egroup" => Some(Signature::default()),
        _ if is_declaration(name) => Some(Signature::default()),
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
        assert!(read(source).1.is_empty());
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
            ("\\emph It's", &[&[emph(&[text("I")]), text("t’s")]]),
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
                &[&[text("A"), emph(&[text("B")]), text("’s")]],
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
                &[vec![text("a"), Inline::LineBreak, text("’b ] c")]],
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
                    assert!(warning.is_empty());
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

    /// Each row: the inlines the source reads as, in a paragraph.
    #[test]
    fn links_and_citations_are_read() {
        let link = |url: &str, content: &[Inline]| Inline::Link {
            url: url.to_string(),
            title: None,
            content: content.to_vec(),
        };
        let bare = |url: &str| link(url, &[text(url)]);
        let strong = |inlines: &[Inline]| Inline::Strong(inlines.to_vec());
        let cases: Vec<(&str, Vec<Inline>)> = vec![
            ("\\url{https://a.org}", vec![bare("https://a.org")]),
            (
                "a \\url{https://a.org} b",
                vec![text("a "), bare("https://a.org"), text(" b")],
            ),
            // As they're written, with what's escaped as itself.
            (
                "\\url{https://a.org/b_c^d~e--f}",
                vec![bare("https://a.org/b_c^d~e--f")],
            ),
            ("\\url{a\\%20b\\#c\\_d\\&e}", vec![bare("a%20b#c_d&e")]),
            // What's closed before it's parsed isn't part of it.
            ("\\url{a_}b", vec![bare("a_"), text("b")]),
            (
                "\\href{https://a.org}{b}",
                vec![link("https://a.org", &[text("b")])],
            ),
            ("\\href[pdfnewwindow]{u}{b}", vec![link("u", &[text("b")])]),
            ("\\href{u_v}{a--b}", vec![link("u_v", &[text("a–b")])]),
            (
                "\\href{u}{\\emph{a} b}",
                vec![link("u", &[emph(&[text("a")]), text(" b")])],
            ),
            // The font around it goes in it, so it can be left there.
            (
                "\\textbf{\\href{u}{a}}",
                vec![link("u", &[strong(&[text("a")])])],
            ),
            (
                "\\textbf{x\\href{u}{\\textmd{a}}y}",
                vec![
                    strong(&[text("x")]),
                    link("u", &[text("a")]),
                    strong(&[text("y")]),
                ],
            ),
            ("\\textbf{\\url{u}}", vec![strong(&[bare("u")])]),
            // Spaces at its ends go out of it.
            (
                "x\\href{u}{ a }y",
                vec![text("x "), link("u", &[text("a")]), text(" y")],
            ),
            ("x\\href{u}{ }y", vec![text("x y")]),
            ("\\emph{x\\url{u}}", vec![emph(&[text("x"), bare("u")])]),
            ("\\href{u}{ a }", vec![link("u", &[text("a")])]),
            // With no text, it's none, and links don't nest.
            ("a\\href{u}{}b", vec![text("ab")]),
            ("\\href{u}{a\\href{v}{b}c}", vec![link("u", &[text("abc")])]),
            ("\\href{u}{a\\url{v}}", vec![link("u", &[text("av")])]),
            // Citations show their keys.
            ("\\cite{a}", vec![text("[a]")]),
            ("x\\cite{a_b, c}y", vec![text("x[a_b, c]y")]),
            ("\\cite*{a}", vec![text("[a]")]),
            ("\\cite[p.~1]{a} b", vec![text("[a, p.\u{a0}1] b")]),
            ("\\cite[]{a}", vec![text("[a]")]),
            ("\\textbf{\\cite{a}}", vec![strong(&[text("[a]")])]),
            ("a\\nocite{b}c", vec![text("ac")]),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, [para(&expected)], "{source}");
        }
        // Blocks in a link's text keep their order, and no link.
        assert_eq!(
            read("x \\href{u}{a\\begin{center}b\\end{center}c} y")
                .0
                .blocks,
            [
                para(&[text("x")]),
                para(&[text("a")]),
                para(&[text("b")]),
                para(&[text("c y")]),
            ]
        );
        // Deep ones read flat.
        let source = format!("{}x{}", "\\href{u}{".repeat(5_000), "}".repeat(5_000));
        assert_eq!(read(&source).0.blocks, [para(&[link("u", &[text("x")])])]);
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn references_go_to_labels_on_headings() {
        let link = |url: &str, content: &[Inline]| Inline::Link {
            url: url.to_string(),
            title: None,
            content: content.to_vec(),
        };
        let sec = || heading(1, &[text("Intro")]);
        let to_it = |key: &str| link("#intro", &[text(&format!("[{key}]"))]);
        let cases: Vec<(&str, Vec<Block>)> = vec![
            // On it, or right after it; a label shows nothing.
            (
                "\\section{Intro}\\label{a}\\ref{a}",
                vec![sec(), para(&[to_it("a")])],
            ),
            (
                "\\section{Intro\\label{a}}\\ref{a}",
                vec![sec(), para(&[to_it("a")])],
            ),
            (
                "\\section{Intro}\n\\label{a}\n\nb \\ref{a}",
                vec![sec(), para(&[text("b "), to_it("a")])],
            ),
            (
                "\\ref{a}\\section{Intro}\\label{a}",
                vec![para(&[to_it("a")]), sec()],
            ),
            ("a\\label{b} c", vec![para(&[text("a c")])]),
            // All kinds, as written; to none, their text.
            (
                "\\section{Intro}\\label{a}\\eqref{a}",
                vec![sec(), para(&[to_it("a")])],
            ),
            (
                "\\section{Intro}\\label{a}\\autoref{a}",
                vec![sec(), para(&[to_it("a")])],
            ),
            (
                "\\section{Intro}\\label{a}\\cref{a}",
                vec![sec(), para(&[to_it("a")])],
            ),
            (
                "\\section{Intro}\\label{a}\\ref*{a}",
                vec![sec(), para(&[to_it("a")])],
            ),
            ("\\ref{none}", vec![para(&[text("[none]")])]),
            ("\\ref{a_b}", vec![para(&[text("[a_b]")])]),
            ("\\cref{a,b}", vec![para(&[text("[a], [b]")])]),
            // Only `\\cref` takes a list.
            (
                "\\section{Intro}\\label{a}\\ref{a,b}",
                vec![sec(), para(&[text("[a,b]")])],
            ),
            ("a\\pageref{b}c", vec![para(&[text("ac")])]),
            // After text, the label is no heading's.
            (
                "\\section{Intro}a\\label{b}\\ref{b}",
                vec![sec(), para(&[text("a[b]")])],
            ),
            // With text of its own.
            (
                "\\section{Intro}\\label{a}\\hyperref[a]{see}",
                vec![sec(), para(&[link("#intro", &[text("see")])])],
            ),
            ("\\hyperref[none]{see}", vec![para(&[text("see")])]),
            ("\\hyperref{see}", vec![para(&[text("see")])]),
            // In a link, it's text.
            (
                "\\href{u}{\\ref{a}}",
                vec![para(&[link("u", &[text("[a]")])])],
            ),
            // In a quote; by the id its text makes, the second's unique.
            (
                "\\section{Intro}\\begin{quote}\\section{Intro}\\label{a}\\end{quote}\\ref{a}",
                vec![
                    sec(),
                    Block::Quote(vec![sec()]),
                    para(&[link("#intro-1", &[text("[a]")])]),
                ],
            ),
            // An empty one holds no label.
            ("\\section{\\label{a}}\\ref{a}", vec![para(&[text("[a]")])]),
            // A formula's shows nothing, what the parser needs closed too.
            (
                "\\begin{equation}\\label{a^}x\\end{equation}",
                vec![Block::Math("\\begin{equation}x\\end{equation}".to_string())],
            ),
            (
                "$x\\label {a{b}c}y$",
                vec![para(&[Inline::Math("x y".to_string())])],
            ),
            // What's joined stays apart, and only a command is one.
            (
                "$\\alpha\\label{a}x$",
                vec![para(&[Inline::Math("\\alpha x".to_string())])],
            ),
            (
                "$\\alpha\\label{a}+x$",
                vec![para(&[Inline::Math("\\alpha+x".to_string())])],
            ),
            (
                "$x\\\\label{a}$",
                vec![para(&[Inline::Math("x\\\\label{a}".to_string())])],
            ),
            (
                "$\\label{a\\}b}c$",
                vec![para(&[Inline::Math("c".to_string())])],
            ),
            (
                "$\\labels{a}$",
                vec![para(&[Inline::Math("\\labels{a}".to_string())])],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// The source's paragraph, and its notes' contents, by their labels 1,
    /// 2, ….
    #[test]
    fn footnotes_are_notes() {
        let note = |label: &str, content: &[Block]| Block::FootnoteDefinition {
            label: label.to_string(),
            content: content.to_vec(),
        };
        let mark = |label: &str| Inline::FootnoteRef(label.to_string());
        let strong = |text_of: &str| Inline::Strong(vec![text(text_of)]);
        let link = |url: &str, content: &[Inline]| Inline::Link {
            url: url.to_string(),
            title: None,
            content: content.to_vec(),
        };
        let cases: Vec<(&str, Vec<Block>)> = vec![
            (
                "a\\footnote{b} c",
                vec![
                    para(&[text("a"), mark("1"), text(" c")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            // In the order they're read, a note in a note before its end.
            (
                "a\\footnote{b}c\\footnote{d}",
                vec![
                    para(&[text("a"), mark("1"), text("c"), mark("2")]),
                    note("1", &[para(&[text("b")])]),
                    note("2", &[para(&[text("d")])]),
                ],
            ),
            (
                "a\\footnote{b\\footnote{c}}",
                vec![
                    para(&[text("a"), mark("1")]),
                    note("2", &[para(&[text("c")])]),
                    note("1", &[para(&[text("b"), mark("2")])]),
                ],
            ),
            // Paragraphs and blocks; its number shows nothing here.
            (
                "a\\footnote[7]{b\n\nc}",
                vec![
                    para(&[text("a"), mark("1")]),
                    note("1", &[para(&[text("b")]), para(&[text("c")])]),
                ],
            ),
            // Empty, it's none.
            ("a\\footnote{}b", vec![para(&[text("ab")])]),
            ("a\\footnote{ }b", vec![para(&[text("ab")])]),
            // In the normal font, with what's around it left as it was.
            (
                "\\textbf{a\\footnote{b}c}",
                vec![
                    para(&[Inline::Strong(vec![text("a"), mark("1"), text("c")])]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            (
                "{\\ttfamily a--b\\footnote{c--d}e--f}",
                vec![
                    para(&[text("a--b"), mark("1"), text("e--f")]),
                    note("1", &[para(&[text("c–d")])]),
                ],
            ),
            (
                "\\foo{a\\footnote{b--c}}",
                vec![
                    para(&[text("a"), mark("1")]),
                    note("1", &[para(&[text("b–c")])]),
                ],
            ),
            // Paragraphs in it, even from an argument.
            (
                "\\emph{a\\footnote{b\n\nc}}",
                vec![
                    para(&[Inline::Emph(vec![text("a"), mark("1")])]),
                    note("1", &[para(&[text("b")]), para(&[text("c")])]),
                ],
            ),
            // In a heading or a link, all its own.
            (
                "\\section{a\\footnote{b}}",
                vec![
                    heading(1, &[text("a"), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            // A link can't hold its mark, which follows it.
            (
                "\\href{u}{a\\footnote{b}c} d",
                vec![
                    para(&[link("u", &[text("ac")]), mark("1"), text(" d")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            (
                "\\href{u}{\\textbf{a\\footnote{b}}}",
                vec![
                    para(&[link("u", &[strong("a")]), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            (
                "\\href{u}{\\footnote{b}}",
                vec![para(&[mark("1")]), note("1", &[para(&[text("b")])])],
            ),
            (
                "\\footnote{\\textbf{a}} b",
                vec![
                    para(&[mark("1"), text(" b")]),
                    note("1", &[para(&[strong("a")])]),
                ],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
        // A heading in a note leaves the main text's references as they were.
        let (doc, _) = read(
            "\\footnote{\\section{a}}\\section{b}\\label{c}\\section{a}\\ref{c} \\ref{d}\\footnote{\\label{d}e}",
        );
        let Block::Paragraph(text_after) = &doc.blocks[3] else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(
            text_after[0],
            Inline::Link {
                url: "#b".to_string(),
                title: None,
                content: vec![text("[c]")],
            }
        );
        assert_eq!(text_after[1], text(" [d]"));
        // Nested deep, they're flat.
        let source = format!("{}x{}", "\\footnote{a".repeat(5_000), "}".repeat(5_000));
        assert_eq!(read(&source).0.blocks.len(), 5_001);
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn footnote_marks_and_texts_are_matched() {
        let note = |label: &str, content: &[Block]| Block::FootnoteDefinition {
            label: label.to_string(),
            content: content.to_vec(),
        };
        let mark = |label: &str| Inline::FootnoteRef(label.to_string());
        let cases: Vec<(&str, Vec<Block>)> = vec![
            // By the count: the mark adds to it, the text doesn't.
            (
                "a\\footnotemark b\\footnotetext{c}",
                vec![
                    para(&[text("a"), mark("1"), text("b")]),
                    note("1", &[para(&[text("c")])]),
                ],
            ),
            (
                "a\\footnotemark\\footnotemark\\footnotetext{c}",
                vec![
                    para(&[text("a"), mark("1"), mark("2")]),
                    note("2", &[para(&[text("c")])]),
                    note("1", &[]),
                ],
            ),
            // By number, whichever comes first.
            (
                "a\\footnotemark[5]\\footnotetext[5]{b}",
                vec![
                    para(&[text("a"), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            (
                "\\footnotetext[5]{b}a\\footnotemark[5]",
                vec![
                    para(&[text("a"), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            // More than one mark of a number before its text is one note.
            (
                "a\\footnotemark[5]c\\footnotemark[5]\\footnotetext[5]{b}",
                vec![
                    para(&[text("a"), mark("1"), text("c"), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            // A number used again is another note.
            (
                "\\footnotemark[1]\\footnotetext[1]{a}\\footnotemark[1]\\footnotetext[1]{b}",
                vec![
                    para(&[mark("1"), mark("2")]),
                    note("1", &[para(&[text("a")])]),
                    note("2", &[para(&[text("b")])]),
                ],
            ),
            // `\\footnote` counts, and `\\setcounter` sets it.
            (
                "\\footnote{a}\\footnotemark\\footnotetext[2]{b}",
                vec![
                    para(&[mark("1"), mark("2")]),
                    note("1", &[para(&[text("a")])]),
                    note("2", &[para(&[text("b")])]),
                ],
            ),
            (
                "\\setcounter{footnote}{4}\\footnotemark\\footnotetext[5]{b}",
                vec![para(&[mark("1")]), note("1", &[para(&[text("b")])])],
            ),
            // A number given to `\\footnote` is no count.
            (
                "\\footnote[7]{a}\\footnotemark\\footnotetext[1]{b}",
                vec![
                    para(&[mark("1"), mark("2")]),
                    note("1", &[para(&[text("a")])]),
                    note("2", &[para(&[text("b")])]),
                ],
            ),
            // Texts with no mark keep their order, and all of them.
            (
                "\\footnotetext{a}\\footnotetext{b}",
                vec![
                    para(&[mark("1"), mark("2")]),
                    note("1", &[para(&[text("a")])]),
                    note("2", &[para(&[text("b")])]),
                ],
            ),
            // No text is lost, nor does a mark show as a label.
            (
                "a\\footnotemark",
                vec![para(&[text("a"), mark("1")]), note("1", &[])],
            ),
            (
                "a\n\n\\footnotetext[3]{b}",
                vec![
                    para(&[text("a")]),
                    para(&[mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            // Given a number that's done, it's that note's, by the end.
            (
                "a\\footnote{b}c\\footnotemark[1]",
                vec![
                    para(&[text("a"), mark("1"), text("c"), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            (
                "\\footnote[7]{b}\\footnotemark[7]",
                vec![
                    para(&[mark("1"), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            (
                "\\footnotemark[2]\\footnotetext[2]{b}\\footnotemark[2]",
                vec![
                    para(&[mark("1"), mark("1")]),
                    note("1", &[para(&[text("b")])]),
                ],
            ),
            (
                "a\\footnote{b\\footnote{c}\\footnotemark[2]}",
                vec![
                    para(&[text("a"), mark("1")]),
                    note("2", &[para(&[text("c")])]),
                    note("1", &[para(&[text("b"), mark("2"), mark("2")])]),
                ],
            ),
            // Marks of a number waiting for its text wait together.
            (
                "\\footnotemark[1]\\footnotemark\\footnotetext{a}",
                vec![
                    para(&[mark("1"), mark("1")]),
                    note("1", &[para(&[text("a")])]),
                ],
            ),
            // It's the note of that number before it, not one after.
            (
                "\\footnote[1]{a}\\footnotemark[1]\\footnote[1]{b}",
                vec![
                    para(&[mark("1"), mark("1"), mark("3")]),
                    note("1", &[para(&[text("a")])]),
                    note("3", &[para(&[text("b")])]),
                ],
            ),
            // With no number given, it's a note of its own, even by another
            // mark of that number.
            (
                "\\footnote{a}\\footnotemark[1]\\setcounter{footnote}{0}\\footnotemark",
                vec![
                    para(&[mark("1"), mark("1"), mark("3")]),
                    note("1", &[para(&[text("a")])]),
                    note("3", &[]),
                ],
            ),
            (
                "\\footnote{a}\\setcounter{footnote}{0}\\footnotemark",
                vec![
                    para(&[mark("1"), mark("2")]),
                    note("1", &[para(&[text("a")])]),
                    note("2", &[]),
                ],
            ),
            // Past the highest count there is, it stays.
            (
                "\\setcounter{footnote}{9223372036854775807}\\footnote{a}\\footnotemark",
                vec![
                    para(&[mark("1"), mark("2")]),
                    note("1", &[para(&[text("a")])]),
                    note("2", &[]),
                ],
            ),
            // Referred to again, it's its mark; anything else, its text.
            (
                "a\\footnote{\\label{b}c} d\\textsuperscript{\\ref{b}}",
                vec![
                    para(&[text("a"), mark("1"), text(" d"), mark("1")]),
                    note("1", &[para(&[text("c")])]),
                ],
            ),
            ("a\\textsuperscript{\\ref{b}}", vec![para(&[text("a[b]")])]),
            ("a\\textsuperscript{b}", vec![para(&[text("ab")])]),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
        // Past nine, too, as numbers.
        let source: String = (1..=12)
            .map(|n| format!("\\footnotetext[{n}]{{{n}}}"))
            .collect();
        let doc = read(&source).0;
        let [Block::Paragraph(marks), ..] = &doc.blocks[..] else {
            panic!("{:?}", doc.blocks);
        };
        let expected: Vec<Inline> = (1..=12).map(|n| mark(&n.to_string())).collect();
        assert_eq!(*marks, expected);
    }

    /// Notes our LaTeX writer writes read back.
    #[test]
    fn our_latex_footnotes_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "a[^1] b\n\n[^1]: note\n",
            "# h[^1]\n\n[^1]: note\n\n    more\n",
            // A note in a note, and one referred to again.
            "a[^1]\n\n[^1]: **b**[^2]\n\n[^2]: c\n",
            "a[^1] b[^2] c[^1]\n\n[^1]: d\n\n[^2]: e\n",
        ] {
            let doc = markdown::read(source);
            let tex = write::latex::write(&doc);
            assert_eq!(
                write::markdown::write(&read(&tex).0),
                write::markdown::write(&doc),
                "{tex}"
            );
        }
    }

    /// Links to headings our LaTeX writer writes read back.
    #[test]
    fn our_latex_heading_links_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "# Intro\n\n[see](#intro)\n",
            "# Café\n\n[back](#café)\n",
            "[on](#b)\n\n# a\n\n# b\n",
        ] {
            let doc = markdown::read(source);
            let tex = write::latex::write(&doc);
            assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
        }
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn images_and_figures_are_read() {
        let image = |url: &str, alt: &str| Inline::Image {
            url: url.to_string(),
            title: None,
            alt: alt.to_string(),
        };
        let cases: Vec<(&str, Vec<Block>)> = vec![
            (
                "\\includegraphics{a.png}",
                vec![para(&[image("a.png", "")])],
            ),
            (
                "x \\includegraphics[width=2cm]{a_b.png} y",
                vec![para(&[text("x "), image("a_b.png", ""), text(" y")])],
            ),
            (
                "\\includegraphics*{a.png}",
                vec![para(&[image("a.png", "")])],
            ),
            (
                "\\includegraphics{a\\%b.png}",
                vec![para(&[image("a%b.png", "")])],
            ),
            // A figure: its caption is the image's alt text, above it or
            // below, its options, `\\centering` and label showing nothing.
            (
                "\\begin{figure}[htbp]\n\\centering\n\\includegraphics{a.png}\n\\caption{Cap}\n\\label{f}\n\\end{figure}",
                vec![para(&[image("a.png", "Cap")])],
            ),
            (
                "\\begin{figure*}\\caption{Cap}\\includegraphics{a.png}\\end{figure*}",
                vec![para(&[image("a.png", "Cap")])],
            ),
            (
                "\\begin{figure}\\includegraphics{a.png}\\caption[Short]{A \\emph{long} one, $x$}\\end{figure}",
                vec![para(&[image("a.png", "A long one, x")])],
            ),
            (
                "\\begin{figure}\\includegraphics{a.png}\\end{figure}",
                vec![para(&[image("a.png", "")])],
            ),
            // Text around a figure is apart from it.
            (
                "a\\begin{figure}\\includegraphics{b.png}\\caption{c}\\end{figure}d",
                vec![
                    para(&[text("a")]),
                    para(&[image("b.png", "c")]),
                    para(&[text("d")]),
                ],
            ),
            // More than that keeps all of it, in order.
            (
                "\\begin{figure}\\includegraphics{a.png}\\includegraphics{b.png}\\caption{c}\\end{figure}",
                vec![
                    para(&[image("a.png", ""), image("b.png", "")]),
                    para(&[text("c")]),
                ],
            ),
            (
                "\\begin{figure}\\includegraphics{a.png}\n\n\\includegraphics{b.png}\\caption{c}\\end{figure}",
                vec![
                    para(&[image("a.png", "")]),
                    para(&[image("b.png", "")]),
                    para(&[text("c")]),
                ],
            ),
            (
                "\\begin{figure}text\\caption{c}\\end{figure}",
                vec![para(&[text("text")]), para(&[text("c")])],
            ),
            (
                "\\begin{table}[t]\\caption{c}a\\end{table}",
                vec![para(&[text("c")]), para(&[text("a")])],
            ),
            // A caption elsewhere is its text.
            (
                "a\\caption{b}c",
                vec![para(&[text("a")]), para(&[text("b")]), para(&[text("c")])],
            ),
            (
                "\\begin{figure}\\includegraphics{a.png}\\caption{}\\end{figure}",
                vec![para(&[image("a.png", "")])],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
        // In a block of its own, it's that block's, not the figure's.
        assert_eq!(
            read("\\begin{figure}\\begin{quote}\\caption{a}\\end{quote}b\\end{figure}")
                .0
                .blocks,
            [Block::Quote(vec![para(&[text("a")])]), para(&[text("b")])]
        );
        assert_eq!(
            read("\\begin{figure}\\includegraphics{a.png}\\begin{quote}x\n\ny\n\n\\caption{c}\\end{quote}\\end{figure}").0.blocks,
            [
                para(&[image("a.png", "")]),
                Block::Quote(vec![para(&[text("x")]), para(&[text("y")]), para(&[text("c")])]),
            ]
        );
        // A mark to come stays, as a note's does.
        let (doc, _) = read(
            "\\footnote{\\label{n}a}\\begin{figure}\\includegraphics{b.png}\\caption{c\\textsuperscript{\\ref{n}}}\\end{figure}",
        );
        assert_eq!(
            doc.blocks[2],
            para(&[text("c"), Inline::FootnoteRef("1".to_string())])
        );
        // A note in a caption keeps its mark, so the caption stays text.
        let (doc, _) =
            read("\\begin{figure}\\includegraphics{a.png}\\caption{b\\footnote{c}}\\end{figure}");
        assert!(
            matches!(
                &doc.blocks[..],
                [
                    Block::Paragraph(_),
                    Block::Paragraph(_),
                    Block::FootnoteDefinition { .. }
                ]
            ),
            "{:?}",
            doc.blocks
        );
    }

    /// Images our LaTeX writer writes read back.
    #[test]
    fn our_latex_images_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        for source in ["![](a.png)\n", "![A cat](images/1.png)\n"] {
            let doc = markdown::read(source);
            let tex = write::latex::write(&doc);
            assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
        }
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn tabular_is_a_table() {
        use Align::{Center, Default as Unset, Left, Right};
        let t = |align: &[Align], header: &[&[&str]], rows: &[&[&str]]| {
            let row = |cells: &&[&str]| -> Vec<Vec<Inline>> {
                cells
                    .iter()
                    .map(|cell| {
                        if cell.is_empty() {
                            Vec::new()
                        } else {
                            vec![text(cell)]
                        }
                    })
                    .collect()
            };
            Block::Table(Table {
                align: align.to_vec(),
                header: header.first().map(row).unwrap_or_default(),
                rows: rows.iter().map(row).collect(),
            })
        };
        let cases: Vec<(&str, Vec<Block>)> = vec![
            // A rule after the first row makes it the header.
            (
                "\\begin{tabular}{lcr}\n\\hline\na & b & c \\\\\n\\hline\nd & e & f \\\\\n\\hline\n\\end{tabular}",
                vec![t(
                    &[Left, Center, Right],
                    &[&["a", "b", "c"]],
                    &[&["d", "e", "f"]],
                )],
            ),
            (
                "\\begin{tabular}{ll}\\toprule a&b\\\\\\midrule c&d\\\\\\bottomrule\\end{tabular}",
                vec![t(&[Left, Left], &[&["a", "b"]], &[&["c", "d"]])],
            ),
            // None, none.
            (
                "\\begin{tabular}{ll}a&b\\\\c&d\\end{tabular}",
                vec![t(&[Left, Left], &[], &[&["a", "b"], &["c", "d"]])],
            ),
            (
                "\\begin{tabular}{l}\\hline a\\\\b\\\\\\hline c\\end{tabular}",
                vec![t(&[Left], &[], &[&["a"], &["b"], &["c"]])],
            ),
            // Specifications.
            (
                "\\begin{tabular}[t]{|l|c@{ x }r|p{3cm}>{\\raggedright}m{1em}!{a}b{2pt}|}a\\end{tabular}",
                vec![t(
                    &[Left, Center, Right, Left, Left, Left],
                    &[],
                    &[&["a", "", "", "", "", ""]],
                )],
            ),
            (
                "\\begin{tabular}{*{2}{c}r}a\\end{tabular}",
                vec![t(&[Center, Center, Right], &[], &[&["a", "", ""]])],
            ),
            (
                "\\begin{tabular*}{5cm}{lc}a&b\\end{tabular*}",
                vec![t(&[Left, Center], &[], &[&["a", "b"]])],
            ),
            (
                "\\begin{tabularx}{\\textwidth}{Xr}a&b\\end{tabularx}",
                vec![t(&[Left, Right], &[], &[&["a", "b"]])],
            ),
            // More cells than the specification says.
            (
                "\\begin{tabular}{l}a&b\\end{tabular}",
                vec![t(&[Left, Unset], &[], &[&["a", "b"]])],
            ),
            // Spacing and other line ends; a last row with no line end.
            (
                "\\begin{tabular}{l}a\\\\[2pt]b\\\\*c\\tabularnewline d\\end{tabular}",
                vec![t(&[Left], &[], &[&["a"], &["b"], &["c"], &["d"]])],
            ),
            // Only what a rule takes is its own.
            (
                "\\begin{tabular}{l}\\hline{a}\\end{tabular}",
                vec![t(&[Left], &[], &[&["a"]])],
            ),
            (
                "\\begin{tabular}{l}\\hline(a)\\end{tabular}",
                vec![t(&[Left], &[], &[&["(a)"]])],
            ),
            (
                "\\begin{tabular}{l}\\hline[a]\\end{tabular}",
                vec![t(&[Left], &[], &[&["[a]"]])],
            ),
            (
                "\\begin{tabular}{l}a\\\\\\toprule[1pt]{b}\\end{tabular}",
                vec![t(&[Left], &[&["a"]], &[&["b"]])],
            ),
            (
                "\\begin{tabular}{l}a\\\\\\cline {1-1}{b}\\end{tabular}",
                vec![t(&[Left], &[&["a"]], &[&["b"]])],
            ),
            (
                "\\begin{tabular}{l}a\\tabularnewline[2pt]b\\end{tabular}",
                vec![t(&[Left], &[], &[&["a"], &["b"]])],
            ),
            // Rules with their arguments.
            (
                "\\begin{tabular}{l}a\\\\\\cline{1-1}b\\\\\\cmidrule(lr){1-1}c\\\\\\cmidrule[1pt](r){1-1}\\end{tabular}",
                vec![t(&[Left], &[&["a"]], &[&["b"], &["c"]])],
            ),
            // Groups keep what they hold; empty cells stay.
            (
                "\\begin{tabular}{ll}{a & b}&\\\\&c\\end{tabular}",
                vec![t(&[Left, Left], &[], &[&["a & b", ""], &["", "c"]])],
            ),
            // Spans.
            (
                "\\begin{tabular}{lll}\\multicolumn{2}{c}{a}&b\\\\\\multirow{2}{*}{c}&d&e\\end{tabular}",
                vec![t(
                    &[Left, Left, Left],
                    &[],
                    &[&["a", "", "b"], &["c", "d", "e"]],
                )],
            ),
            (
                "\\begin{tabular}{ll}\\multicolumn{99999999}{c}{a}\\end{tabular}",
                vec![t(&[Left, Left], &[], &[&["a", ""]])],
            ),
            // A blank row stays.
            (
                "\\begin{tabular}{l}a\\\\\\\\b\\end{tabular}",
                vec![t(&[Left], &[], &[&["a"], &[""], &["b"]])],
            ),
            // Nothing in it, nothing of it; text around it is apart.
            ("\\begin{tabular}{ll}\\hline\\end{tabular}", vec![]),
            (
                "a\\begin{tabular}{l}b\\end{tabular}c",
                vec![
                    para(&[text("a")]),
                    t(&[Left], &[], &[&["b"]]),
                    para(&[text("c")]),
                ],
            ),
            // In a table float, its caption is a paragraph of its own.
            (
                "\\begin{table}[h]\\caption{c}\\begin{tabular}{l}a\\end{tabular}\\end{table}",
                vec![para(&[text("c")]), t(&[Left], &[], &[&["a"]])],
            ),
            // A table in a cell keeps the cell's span.
            (
                "\\begin{tabular}{lll}\\multicolumn{2}{c}{\\begin{tabular}{l}a\\end{tabular}}&b\\end{tabular}",
                vec![t(&[Left, Left, Left], &[], &[&["a", "", "b"]])],
            ),
            // Blocks in a cell are their inlines, in order.
            (
                "\\begin{tabular}{l}\\begin{quote}a\\end{quote}b\\end{tabular}",
                vec![t(&[Left], &[], &[&["a b"]])],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
        // Inlines, and what a cell declares ends with it.
        let (doc, _) = read("\\begin{tabular}{ll}\\bf a&\\emph{b}\\end{tabular}");
        assert_eq!(
            doc.blocks,
            [Block::Table(Table {
                align: vec![Left, Left],
                header: Vec::new(),
                rows: vec![vec![
                    vec![Inline::Strong(vec![text("a")])],
                    vec![Inline::Emph(vec![text("b")])],
                ]],
            })]
        );
        // Blocks in a cell keep all they hold, in order, each apart.
        let code = |code: &str| Inline::Code(code.to_string());
        let math = |tex: &str| Inline::Math(tex.to_string());
        let image = Inline::Image {
            url: "b.png".to_string(),
            title: None,
            alt: String::new(),
        };
        for (source, cell) in [
            (
                "\\begin{tabular}{p{3cm}}a\\begin{verbatim}b\\end{verbatim}c\\end{tabular}",
                vec![text("a "), code("b"), text(" c")],
            ),
            (
                "\\begin{tabular}{p{3cm}}\\begin{quote}a\\includegraphics{b.png}\\end{quote}\\end{tabular}",
                vec![text("a"), image.clone()],
            ),
            (
                "\\begin{tabular}{p{3cm}}\\begin{quote}\\[x\\]\\end{quote}c\\end{tabular}",
                vec![math("x"), text(" c")],
            ),
            (
                "\\begin{tabular}{p{3cm}}\\begin{itemize}\\item a\\item b\\end{itemize}\\end{tabular}",
                vec![text("a b")],
            ),
            (
                "\\begin{tabular}{p{3cm}}\\begin{tabular}{l}\\textbf{a}\\\\b\\end{tabular}\\end{tabular}",
                vec![
                    Inline::Strong(vec![text("a")]),
                    Inline::LineBreak,
                    text("b"),
                ],
            ),
        ] {
            let (doc, _) = read(source);
            let [Block::Table(table)] = &doc.blocks[..] else {
                panic!("{source}: {:?}", doc.blocks);
            };
            assert_eq!(table.rows[0][0], cell, "{source}");
        }
        // A note's mark too, so the note stays.
        let (doc, _) =
            read("\\begin{tabular}{p{3cm}}\\begin{quote}a\\footnote{b}\\end{quote}\\end{tabular}");
        let [Block::Table(table), Block::FootnoteDefinition { .. }] = &doc.blocks[..] else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(
            table.rows[0][0],
            [text("a"), Inline::FootnoteRef("1".to_string())]
        );
        // A row past the most columns keeps its cells in the last.
        let source = format!(
            "\\begin{{tabular}}{{l}}{}z\\end{{tabular}}",
            "a&".repeat(MAX_COLUMNS + 3)
        );
        let (doc, _) = read(&source);
        let [Block::Table(table)] = &doc.blocks[..] else {
            panic!("no table");
        };
        assert_eq!(table.rows[0].len(), MAX_COLUMNS);
        assert_eq!(table.rows[0][MAX_COLUMNS - 1], [text("a a a a z")]);
        // A dense table of any size stays one.
        let row = format!("{}a\\\\", "a&".repeat(9));
        let source = format!(
            "\\begin{{tabular}}{{l}}{}\\end{{tabular}}",
            row.repeat(1_000)
        );
        let (doc, warning) = read(&source);
        assert!(matches!(&doc.blocks[..], [Block::Table(_)]));
        assert_eq!(warning, []);
        // A small source doesn't write a huge document: a sparse table is
        // its text, said so.
        let source = format!(
            "\\begin{{tabular}}{{l}}{}a\\\\{}\\end{{tabular}}",
            "&".repeat(5_000),
            "a\\\\".repeat(5_000)
        );
        let (doc, warning) = read(&source);
        assert_eq!(warning.len(), 1);
        assert_eq!(warning[0].count, 1);
        assert!(
            doc.blocks
                .iter()
                .all(|block| matches!(block, Block::Paragraph(_)))
        );
        assert_eq!(doc.blocks.len(), 5_001);
        let written = crate::converter::document::native::write::markdown::write(&doc);
        assert!(written.len() < 3 * source.len(), "{}", written.len());
        // Each of what was given up on is said.
        let nested = "\\begin{quote}".repeat(MAX_LIST_NESTING + 1);
        let (_, warnings) = read(&format!("{nested}{source}"));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        // A repeat of any size is bounded.
        let Block::Table(table) = &read("\\begin{tabular}{*{999999999}{c}}a\\end{tabular}")
            .0
            .blocks[0]
        else {
            panic!("no table");
        };
        assert_eq!(table.align.len(), MAX_COLUMNS);
    }

    /// Each row: the table the source reads as.
    #[test]
    fn longtable_is_a_table() {
        use Align::Left;
        let table = |header: &[&str], rows: &[&[&str]]| {
            let row = |cells: &[&str]| -> Vec<Vec<Inline>> {
                cells.iter().map(|cell| vec![text(cell)]).collect()
            };
            Block::Table(Table {
                align: vec![Left, Left],
                header: row(header),
                rows: rows.iter().map(|cells| row(cells)).collect(),
            })
        };
        let cases: Vec<(&str, Vec<Block>)> = vec![
            // As our LaTeX writer writes it.
            (
                "\\begin{longtable}{@{}ll@{}}\n\\toprule\na & b \\\\\n\\midrule\n\\endhead\nc & d \\\\\n\\bottomrule\n\\end{longtable}",
                vec![table(&["a", "b"], &[&["c", "d"]])],
            ),
            (
                "\\begin{longtable}[c]{ll}\na & b \\\\\\endhead\nc & d\\end{longtable}",
                vec![table(&["a", "b"], &[&["c", "d"]])],
            ),
            // With a head of its own for the first page; the second's the same.
            (
                "\\begin{longtable}{ll}a&b\\\\\\endfirsthead x&y\\\\\\endhead c&d\\\\\\end{longtable}",
                vec![table(&["a", "b"], &[&["c", "d"]])],
            ),
            // The last page's foot ends it, and the others' go.
            (
                "\\begin{longtable}{ll}a&b\\\\\\endhead x&y\\\\\\endfoot z&w\\\\\\endlastfoot c&d\\\\\\end{longtable}",
                vec![table(&["a", "b"], &[&["c", "d"], &["z", "w"]])],
            ),
            (
                "\\begin{longtable}{ll}a&b\\\\\\endhead z&w\\\\\\endlastfoot x&y\\\\\\endfoot c&d\\\\\\end{longtable}",
                vec![table(&["a", "b"], &[&["c", "d"], &["z", "w"]])],
            ),
            // A rule in a foot says nothing of the rows after it.
            (
                "\\begin{longtable}{ll}c&d\\\\\\hline\\endfoot a&b\\\\e&f\\\\\\end{longtable}",
                vec![Block::Table(Table {
                    align: vec![Left, Left],
                    header: Vec::new(),
                    rows: [["a", "b"], ["e", "f"], ["c", "d"]]
                        .iter()
                        .map(|row| row.iter().map(|cell| vec![text(cell)]).collect())
                        .collect(),
                })],
            ),
            // With one foot, it's the last page's too.
            (
                "\\begin{longtable}{ll}a&b\\\\\\endhead z&w\\\\\\endfoot c&d\\\\\\end{longtable}",
                vec![table(&["a", "b"], &[&["c", "d"], &["z", "w"]])],
            ),
            // A head of more rows has its first for the header.
            (
                "\\begin{longtable}{ll}a&b\\\\e&f\\\\\\endhead c&d\\end{longtable}",
                vec![table(&["a", "b"], &[&["e", "f"], &["c", "d"]])],
            ),
            // A caption is before the table, what's left of its row gone.
            (
                "\\begin{longtable}{ll}\\caption{x}\\label{y}\\\\a&b\\\\\\hline c&d\\end{longtable}",
                vec![para(&[text("x")]), table(&["a", "b"], &[&["c", "d"]])],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// Tables our LaTeX writer writes read back.
    #[test]
    fn our_latex_tables_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "| a | b |\n| :-- | --: |\n| c | d |\n",
            "| a | b |\n| :-: | :-: |\n| c | d |\n| e | f |\n",
            "| a |\n| :-- |\n| x<br>y |\n",
        ] {
            let doc = markdown::read(source);
            let tex = write::latex::write(&doc);
            assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
        }
    }

    /// Each row: what the source says of itself.
    #[test]
    fn titles_are_the_document_s() {
        type Case<'a> = (&'a str, Option<&'a str>, &'a [&'a str], Option<&'a str>);
        let cases: Vec<Case> = vec![
            (
                "\\title{T}\\author{A}\\date{D}\\begin{document}x\\end{document}",
                Some("T"),
                &["A"],
                Some("D"),
            ),
            // In the document as well; `\\maketitle` shows nothing.
            (
                "\\begin{document}\\title{T}\\author{A}\\maketitle x\\end{document}",
                Some("T"),
                &["A"],
                None,
            ),
            // A fragment.
            ("\\title{T}x", Some("T"), &[], None),
            // Their text; a short title goes.
            (
                "\\title[S]{A \\& \\emph{B}--C}\\begin{document}x\\end{document}",
                Some("A & B–C"),
                &[],
                None,
            ),
            // Authors: by `\\and`.
            (
                "\\author{A \\and B\\and{} C}\\begin{document}x\\end{document}",
                None,
                &["A", "B", "C"],
                None,
            ),
            // Each `\\author` replaces the last, but where the class adds it.
            (
                "\\author{A}\\author{B}\\begin{document}x\\end{document}",
                None,
                &["B"],
                None,
            ),
            (
                "\\documentclass[11pt]{article}\\author{A}\\begin{document}\\author{B}x\\end{document}",
                None,
                &["B"],
                None,
            ),
            (
                "\\documentclass[aps]{revtex4-2}\\author{A}\\begin{document}\\author{B \\and C}x\\end{document}",
                None,
                &["A", "B", "C"],
                None,
            ),
            (
                "\\documentclass{amsart}\\author{A}\\author{B}\\begin{document}x\\end{document}",
                None,
                &["A", "B"],
                None,
            ),
            // A fragment's are read once.
            ("\\author{A}x", None, &["A"], None),
            // Empty, or what we can't work out, is none.
            (
                "\\title{}\\author{}\\date{\\today}\\begin{document}x\\end{document}",
                None,
                &[],
                None,
            ),
            // `\\thanks` is no part of them.
            (
                "\\title{T\\thanks{N}}\\author{A\\thanks{M}}\\begin{document}x\\end{document}",
                Some("T"),
                &["A"],
                None,
            ),
            // What a definition holds is none of them.
            (
                "\\newcommand{\\x}{\\title{N}}\\renewcommand\\title{N}\\def\\y{\\title{M}}\\begin{document}x\\end{document}",
                None,
                &[],
                None,
            ),
            // After the document's end, nothing is read.
            (
                "\\begin{document}x\\end{document}\\title{T}",
                None,
                &[],
                None,
            ),
        ];
        for (source, title, authors, date) in cases {
            let (doc, _) = read(source);
            assert_eq!(doc.meta.title.as_deref(), title, "{source}");
            assert_eq!(doc.meta.authors, authors, "{source}");
            assert_eq!(doc.meta.date.as_deref(), date, "{source}");
            assert_eq!(doc.blocks, [para(&[text("x")])], "{source}");
        }
    }

    /// What our LaTeX writer writes in the preamble reads back.
    #[test]
    fn our_latex_titles_read_back() {
        use crate::converter::document::native::write;
        let doc = Document {
            meta: Meta {
                title: Some("T & U".to_string()),
                authors: vec!["Ann".to_string(), "Bo".to_string()],
                date: Some("2024-01-02".to_string()),
                ..Meta::default()
            },
            blocks: vec![para(&[text("x")])],
        };
        let (read_back, _) = read(&write::latex::write(&doc));
        assert_eq!(read_back, doc);
    }

    /// Links our LaTeX writer writes read back.
    #[test]
    fn our_latex_links_read_back() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "<https://e.org>\n",
            "[site](https://e.org/a_b?x=1%20#top) and [**b**](u)\n",
            "[*a* b](https://e.org/~c)\n",
        ] {
            let doc = markdown::read(source);
            let tex = write::latex::write(&doc);
            assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
        }
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn font_declarations_hold_to_their_group_s_end() {
        let strong = |text_of: &str| Inline::Strong(vec![text(text_of)]);
        let italic = |text_of: &str| Inline::Emph(vec![text(text_of)]);
        let cases: Vec<(&str, Vec<Block>)> = vec![
            ("{\\bf a} b", vec![para(&[strong("a"), text(" b")])]),
            (
                "a {\\bfseries b c} d",
                vec![para(&[text("a "), strong("b c"), text(" d")])],
            ),
            ("{\\it a} b", vec![para(&[italic("a"), text(" b")])]),
            ("{\\sl a}", vec![para(&[italic("a")])]),
            ("{\\itshape a}", vec![para(&[italic("a")])]),
            // `\\em` switches, as `\\emph` does.
            ("{\\em a \\em b}", vec![para(&[italic("a"), text(" b")])]),
            // The old ones start over; the new ones add.
            ("{\\bf\\it a}", vec![para(&[italic("a")])]),
            (
                "{\\bfseries\\itshape a}",
                vec![para(&[Inline::Strong(vec![italic("a")])])],
            ),
            ("{\\bf a\\rm b}", vec![para(&[strong("a"), text("b")])]),
            (
                "{\\bf a\\normalfont b}",
                vec![para(&[strong("a"), text("b")])],
            ),
            (
                "{\\bf a\\mdseries b}",
                vec![para(&[strong("a"), text("b")])],
            ),
            ("{\\it a\\upshape b}", vec![para(&[italic("a"), text("b")])]),
            // No writer shows these.
            ("{\\sc a}{\\sf b}{\\scshape c}", vec![para(&[text("abc")])]),
            // With no group, to the end, past a paragraph's and a group after.
            (
                "\\bf a\n\nb",
                vec![para(&[strong("a")]), para(&[strong("b")])],
            ),
            ("\\bf{a} b", vec![para(&[strong("a b")])]),
            // An environment ends it, its items don't.
            (
                "\\begin{itemize}\\item\\bf a\\item b\\end{itemize}c",
                vec![
                    list(
                        false,
                        &[
                            (None, &[para(&[strong("a")])]),
                            (None, &[para(&[strong("b")])]),
                        ],
                    ),
                    para(&[text("c")]),
                ],
            ),
            (
                "\\begin{quote}\\it a\\end{quote}b",
                vec![Block::Quote(vec![para(&[italic("a")])]), para(&[text("b")])],
            ),
            (
                "\\begin{bfseries}a\\end{bfseries} b",
                vec![para(&[strong("a"), text(" b")])],
            ),
            // So does an argument.
            ("\\foo{\\bf a}b", vec![para(&[strong("a"), text("b")])]),
            (
                "\\textit{\\bfseries a}b",
                vec![para(&[Inline::Strong(vec![italic("a")]), text("b")])],
            ),
            (
                "\\section{\\bf a}b",
                vec![heading(1, &[strong("a")]), para(&[text("b")])],
            ),
            (
                "\\begin{itemize}\\item[\\bf a] b\\item c\\end{itemize}",
                vec![list(
                    false,
                    &[
                        (None, &[para(&[strong("a"), text(" b")])]),
                        (None, &[para(&[text("c")])]),
                    ],
                )],
            ),
            // Groups as commands end them too.
            (
                "\\begingroup\\bfseries a\\endgroup b",
                vec![para(&[strong("a"), text("b")])],
            ),
            (
                "\\bgroup\\it a\\egroup b",
                vec![para(&[italic("a"), text("b")])],
            ),
            (
                "\\begingroup\\it a\\begingroup\\upshape b\\endgroup c\\endgroup d",
                vec![para(&[italic("a"), text("b"), italic("c"), text("d")])],
            ),
            ("\\endgroup a", vec![para(&[text("a")])]),
            // Typewriter joins nothing, in its group alone.
            (
                "{\\ttfamily a--b ``c'' d'e}--",
                vec![para(&[text("a--b ``c'' d'e–")])],
            ),
            ("{\\tt a--b}", vec![para(&[text("a--b")])]),
            ("{\\ttfamily\\rmfamily a--b}", vec![para(&[text("a–b")])]),
            ("{\\tt\\rm a--b}", vec![para(&[text("a–b")])]),
            // Commands for another family leave it, in their argument.
            (
                "{\\ttfamily\\textrm{a--b}c--d}",
                vec![para(&[text("a–bc--d")])],
            ),
            ("{\\ttfamily\\textsf{a--b}}", vec![para(&[text("a–b")])]),
            ("{\\ttfamily\\textnormal{a--b}}", vec![para(&[text("a–b")])]),
            ("{\\ttfamily\\textsc{a--b}}", vec![para(&[text("a--b")])]),
            (
                "\\emph{\\ttfamily a--b}c--d",
                vec![para(&[italic("a--b"), text("c–d")])],
            ),
            (
                "\\begin{ttfamily}a--b\\end{ttfamily}c--d",
                vec![para(&[text("a--bc–d")])],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// Each row: the inlines the source reads as, in a paragraph.
    #[test]
    fn typewriter_text_is_code() {
        let code = |code: &str| Inline::Code(code.to_string());
        let strong = |inlines: &[Inline]| Inline::Strong(inlines.to_vec());
        let cases: Vec<(&str, Vec<Inline>)> = vec![
            ("\\texttt{a}", vec![code("a")]),
            ("x \\texttt{a} y", vec![text("x "), code("a"), text(" y")]),
            ("\\texttt a b", vec![code("a"), text(" b")]),
            ("\\texttt'", vec![code("'")]),
            ("\\texttt\\TeX a", vec![code("TeX"), text("a")]),
            // As it's written, and its spaces too; joined by nothing.
            ("\\texttt{a--b ``c'' d'e}", vec![code("a--b ``c'' d'e")]),
            ("\\texttt{ a  b }", vec![code(" a b ")]),
            // With characters as the writer writes them.
            (
                "\\texttt{a\\_b\\textbackslash{}c-{}-{}d}",
                vec![code("a_b\\c--d")],
            ),
            // In bold and emphasis, in their runs.
            ("\\textbf{\\texttt{a}}", vec![strong(&[code("a")])]),
            ("\\emph{\\texttt{a}}", vec![emph(&[code("a")])]),
            (
                "\\textbf{\\emph{\\texttt{a}}}",
                vec![strong(&[emph(&[code("a")])])],
            ),
            (
                "\\textbf{x\\texttt{a}y}",
                vec![strong(&[text("x"), code("a"), text("y")])],
            ),
            // Empty, it's none.
            ("x\\texttt{}y", vec![text("xy")]),
            // Holding more than text, it's what it holds, joining nothing.
            (
                "\\texttt{a--\\emph{b}}",
                vec![text("a--"), emph(&[text("b")])],
            ),
            ("\\texttt{\\texttt{a}}", vec![code("a")]),
            // A font changed in it leaves what's after in a run of its own.
            (
                "\\emph{x\\texttt{\\emph{b}é}}",
                vec![emph(&[text("x")]), text("b"), emph(&[text("é")])],
            ),
            (
                "\\emph{x\\texttt{\\emph{b}cd}}",
                vec![emph(&[text("x")]), text("b"), emph(&[text("cd")])],
            ),
            (
                "\\textbf{x\\texttt{\\textmd{b}é}}",
                vec![strong(&[text("x")]), text("b"), strong(&[text("é")])],
            ),
            ("é\\texttt{ü}", vec![text("é"), code("ü")]),
            ("\\emph{é\\texttt{ü}}", vec![emph(&[text("é"), code("ü")])]),
            // Right after other code, it's one with it.
            ("\\texttt{a}\\texttt{b}", vec![code("ab")]),
            ("\\verb|a|\\texttt{b}", vec![code("ab")]),
            ("\\texttt{a}\\verb|b|", vec![code("ab")]),
            (
                "\\texttt{a} \\texttt{b}",
                vec![code("a"), text(" "), code("b")],
            ),
            ("\\texttt{\\emph{a}b}", vec![emph(&[text("a")]), text("b")]),
            (
                "x\\texttt{\\emph{a}b}",
                vec![text("x"), emph(&[text("a")]), text("b")],
            ),
            (
                "\\texttt{a\\emph{b}}c",
                vec![text("a"), emph(&[text("b")]), text("c")],
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, [para(&expected)], "{source}");
        }
        // Nothing left of what's before it is no trouble.
        assert_eq!(read("\\emph{ }\\texttt{\\section{}}").0.blocks, []);
        // A block in it stays between what's around it.
        assert_eq!(
            read("x \\texttt{a\\begin{center}b\\end{center}c} y")
                .0
                .blocks,
            [
                para(&[text("x a")]),
                para(&[text("b")]),
                para(&[text("c y")])
            ]
        );
        // Nesting costs no more than it holds.
        let source = format!(
            "{}x{}",
            "\\texttt{\\emph{".repeat(5_000),
            "}}".repeat(5_000)
        );
        assert!(!read(&source).0.blocks.is_empty());
    }

    /// Code read from LaTeX reads back from Markdown as it was.
    #[test]
    fn typewriter_code_reads_back_from_markdown() {
        use crate::converter::document::native::{read::markdown, write};
        for source in ["\\texttt{a}\\texttt{b}", "\\verb|a|\\texttt{b} c"] {
            let doc = read(source).0;
            let md = write::markdown::write(&doc);
            assert_eq!(markdown::read(&md).blocks, doc.blocks, "{md}");
        }
    }

    /// Code our LaTeX writer writes reads back.
    #[test]
    fn our_latex_code_reads_back() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "`a`\n",
            "see `a--b` and `c d`\n",
            "`'q' \"r\" <x> ~ ^ \\ { } $ & % # _`\n",
            "**`a`** *`b`*\n",
        ] {
            let doc = markdown::read(source);
            let tex = write::latex::write(&doc);
            assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
        }
    }

    /// Each row: the text the source reads as.
    #[test]
    fn punctuation_is_set_as_tex_sets_it() {
        let cases = [
            // What fonts join; words stay apart.
            ("a-b--c---d", "a-b–c—d"),
            ("``a'' `b'", "“a” ‘b’"),
            ("don't", "don’t"),
            ("a'''", "a”’"),
            ("a-{}-{}b `{}`c", "a--b ‘‘c"),
            // What's between two `'` keeps them apart.
            ("a'{}'b", "a’’b"),
            ("a'\\mbox{}'b", "a’’b"),
            ("a' 'b", "a’ ’b"),
            ("\\textquoteright'b", "’’b"),
            // Unknown commands' text is as written: it may be an address.
            ("\\path{a--b ``c'' d'e}--", "a--b ``c'' d'e–"),
            ("\\foo{\\mbox{a--b}}c--d", "a--bc–d"),
            ("a'\n'b", "a’ ’b"),
            ("a'%\n'b", "a”b"),
            // Commands for characters: spaces after a word go, `{}` keeps them.
            ("\\textendash a", "–a"),
            ("\\textendash{} a", "– a"),
            ("a\\ldots b", "a…b"),
            ("a\\dots{} b", "a… b"),
            ("\\textquotedblleft{}a\\textquotedblright{}", "“a”"),
            ("\\textquoteleft{}a\\textquoteright{}", "‘a’"),
            ("\\textquotesingle{}\\textasciigrave{}", "'`"),
            (
                "\\textbackslash{}a \\textasciitilde{}\\textasciicircum{}",
                "\\a ~^",
            ),
            ("\\textless{}a\\textgreater{}\\textbar{}", "<a>|"),
            (
                "\\textperiodcentered{}\\textbullet{}\\textellipsis{}",
                "·•…",
            ),
            ("\\S{}1 \\P{}2 \\dag\\ddag\\copyright\\pounds", "§1 ¶2 †‡©£"),
            ("Stra\\ss e \\ae\\oe\\o\\aa\\l\\i{}", "Straße æœøåłı"),
            ("\\LaTeX{} and \\TeX", "LaTeX and TeX"),
            // Spaces of their own.
            ("a\\,b", "a\u{2006}b"),
            ("a\\;b", "a\u{2005}b"),
            ("a\\quad b", "a\u{2003}b"),
            ("a\\qquad b", "a\u{2003}\u{2003}b"),
            // Showing nothing.
            ("\\protect\\emph{}a", "a"),
            ("a\\/b\\-c\\!d", "abcd"),
            // Their first argument alone; a box keeps its text.
            ("\\texorpdfstring{a}{b}c", "ac"),
            ("\\mbox{a}b \\mbox c", "ab c"),
            // Colors, which no writer shows: the text alone.
            ("a\\textcolor{red}{b}c", "abc"),
            ("a\\textcolor[rgb]{1,0,0}{b} c", "ab c"),
            ("a {\\color{red} b} c", "a b c"),
            ("a\\color[rgb]{1,0,0}b", "ab"),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, [para(&[text(expected)])], "{source}");
        }
        // A color's text is in the font around it.
        assert_eq!(
            read("\\textbf{a\\textcolor{red}{b}}c").0.blocks,
            [para(&[Inline::Strong(vec![text("ab")]), text("c")])]
        );
        // A display formula in it is one still.
        assert_eq!(
            read("a\\textcolor{red}{\\[x=1\\]}b").0.blocks,
            read("a\\[x=1\\]b").0.blocks
        );
        assert!(matches!(
            read("a\\textcolor{red}{\\[x=1\\]}b").0.blocks[..],
            [_, Block::Math(_), _]
        ));
        // In code and math, nothing's joined.
        assert_eq!(
            read("\\verb|a--b| $a--b'$").0.blocks,
            [para(&[
                Inline::Code("a--b".to_string()),
                text(" "),
                Inline::Math("a--b'".to_string()),
            ])]
        );
        // In bold, an apostrophe joins as well.
        assert_eq!(
            read("\\textbf{a''}").0.blocks,
            [para(&[Inline::Strong(vec![text("a”")])])]
        );
        assert_eq!(
            read("a\\newline  b").0.blocks,
            [para(&[text("a"), Inline::LineBreak, text("b")])]
        );
        assert_eq!(
            read("\\section{\\texorpdfstring{$x$}{x} a}").0.blocks,
            [Block::Heading {
                level: 1,
                content: vec![Inline::Math("x".to_string()), text(" a")],
            }]
        );
    }

    /// What our LaTeX writer writes for these reads back.
    #[test]
    fn our_latex_punctuation_reads_back() {
        use crate::converter::document::native::{read::markdown, write};
        for source in [
            "“a” ‘b’ it’s – and — so… a · b\n",
            "~ ^ \\\\ < > | \\_ \\* # $ & %\n",
            "a  \nb\n",
        ] {
            let doc = markdown::read(source);
            let tex = write::latex::write(&doc);
            assert_eq!(read(&tex).0.blocks, doc.blocks, "{tex}");
        }
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
            warning[0].message(),
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
            warning[0].message(),
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
        // With no group in sight, what's after it isn't lost.
        let far = format!("a\\def\\f{}{{b}}", "#".repeat(MAX_DEFINITION_PARAMETERS));
        for source in ["a\\def\\f#1${hidden}b", "a\\def\\f#1 b", &far] {
            let blocks = format!("{:?}", read(source).0.blocks);
            assert!(blocks.contains('b'), "{source}: {blocks}");
        }
    }

    /// Each row: the blocks the source reads as.
    #[test]
    fn macros_are_expanded() {
        let strong = |content: &str| Inline::Strong(vec![text(content)]);
        let plain = |content: &str| vec![para(&[text(content)])];
        let note = |label: &str, content: &str| Block::FootnoteDefinition {
            label: label.to_string(),
            content: vec![para(&[text(content)])],
        };
        let cases: Vec<(&str, Vec<Block>)> = vec![
            ("\\newcommand{\\x}{a}\\x", plain("a")),
            ("\\newcommand\\x{a}\\x", plain("a")),
            ("\\newcommand*{\\x}{a}\\x", plain("a")),
            ("\\renewcommand{\\x}{a}\\x", plain("a")),
            ("\\providecommand{\\x}{a}\\x", plain("a")),
            ("\\def\\x{a}\\x", plain("a")),
            ("\\gdef\\x{a}\\x", plain("a")),
            ("\\newcommand{\\x}[0]{a}\\x", plain("a")),
            (
                "\\newcommand{\\x}{a}\\begin{document}\\x\\end{document}",
                plain("a"),
            ),
            // The spaces after a call go, and a group after it is a group.
            ("\\def\\x{a}\\x b", plain("ab")),
            ("\\def\\x{a}\\x{} b", plain("a b")),
            ("\\def\\x{a}\\x{b}", plain("ab")),
            ("\\def\\x{}a \\x b", plain("a b")),
            // What it stands for reads as it would anywhere.
            ("\\def\\x{\\textbf{a}}\\x", vec![para(&[strong("a")])]),
            (
                "\\def\\x{\\bfseries}{\\x a}b",
                vec![para(&[strong("a"), text("b")])],
            ),
            ("\\def\\x{a}\\def\\y{\\x b}\\y", plain("ab")),
            ("\\def\\x{\\section{a}}\\x", vec![heading(1, &[text("a")])]),
            (
                "\\def\\x{a\\footnote{b}}\\x",
                vec![
                    para(&[text("a"), Inline::FootnoteRef("1".to_string())]),
                    note("1", "b"),
                ],
            ),
            ("\\def\\x{\\$}\\x", plain("$")),
            (
                "\\def\\x{a\\\\$b$}\\x",
                vec![para(&[
                    text("a"),
                    Inline::LineBreak,
                    Inline::Math("b".to_string()),
                ])],
            ),
            // A later definition holds, in its group alone.
            ("\\def\\x{a}\\def\\x{b}\\x", plain("b")),
            ("\\def\\x{a}{\\def\\x{b}\\x}\\x", plain("ba")),
            ("{\\def\\x{a}}b\\x", plain("b")),
            ("{\\gdef\\x{a}}\\x", plain("a")),
            ("\\def\\x{a}{\\def\\x{b}\\gdef\\x{c}}\\x", plain("c")),
            ("{\\gdef\\x{a}\\def\\x{b}}\\x", plain("a")),
            (
                "\\def\\x{a}\\begingroup\\def\\x{b}\\x\\endgroup\\x",
                plain("ba"),
            ),
            (
                "\\def\\x{a}{\\def\\x{b}\\begingroup}\\endgroup\\x",
                plain("a"),
            ),
            (
                "\\def\\x{a}\\begin{quote}\\def\\x{b}\\end{quote}\\x",
                plain("a"),
            ),
            ("\\def\\x{a}\\emph{\\def\\x{b}}\\x", plain("a")),
            (
                "\\def\\x{a}c\\footnote{\\def\\x{b}d}\\x",
                vec![
                    para(&[text("c"), Inline::FootnoteRef("1".to_string()), text("a")]),
                    note("1", "d"),
                ],
            ),
            (
                "\\def\\x{a}\\section*{\\def\\x{b}c}\\x",
                vec![heading(1, &[text("c")]), para(&[text("a")])],
            ),
            (
                "\\section*{\\gdef\\x{a}c}\\x",
                vec![heading(1, &[text("c")]), para(&[text("a")])],
            ),
            ("\\def\\x{a}\\providecommand{\\x}{b}\\x", plain("a")),
            (
                "\\newcommand{\\x}[1]{#1}\\providecommand{\\x}{b}\\x{a}",
                plain("a"),
            ),
            // Not before it's defined.
            ("\\x\\def\\x{a}b", plain("b")),
            // What the reader knows keeps its meaning.
            (
                "\\providecommand{\\emph}{b}\\emph{a}",
                vec![para(&[emph(&[text("a")])])],
            ),
            (
                "\\renewcommand{\\emph}{b}\\emph{a}",
                vec![para(&[emph(&[text("a")])])],
            ),
            (
                "\\renewcommand\\section{\\@startsection{section}{1}}\\section{a}",
                vec![heading(1, &[text("a")])],
            ),
            // Taking arguments, it replaces what it replaced.
            ("\\def\\x{a}\\def\\x#1{b#1}\\x c", plain("bc")),
            // One that takes its meanings where it's defined is unknown, and
            // so is what it replaced.
            ("\\edef\\x{a}\\x b", plain("b")),
            ("\\def\\x{a}{\\xdef\\x{b}}\\x c", plain("c")),
            // Not balanced, it's unknown, and what's after it stays.
            (
                "\\newcommand{\\be}{\\begin{quote}}\\newcommand{\\ee}{\\end{quote}}a\\be b\\ee c",
                plain("abc"),
            ),
            (
                "\\newcommand{\\be}{\\begin{equation}}\\newcommand{\\ee}{\\end{equation}}a\\be b\\ee c",
                plain("abc"),
            ),
            ("\\def\\x{$}a\\x b", plain("ab")),
            ("\\def\\x{\\[}a\\x b", plain("ab")),
        ];
        for (source, expected) in cases {
            assert_eq!(read(source).0.blocks, expected, "{source}");
        }
    }

    /// Each row: a call of a macro taking arguments, and what it stands for,
    /// written out, which it reads as.
    #[test]
    fn macros_taking_arguments_are_expanded() {
        let f = "\\newcommand{\\f}[1]{(#1)}";
        let cases = [
            (format!("{f}a\\f{{b}}c"), "a(b)c"),
            ("\\newcommand{\\f}[2]{#2#1}\\f{a}{b}".into(), "ba"),
            ("\\def\\f#1#2{#2#1}\\f ab".into(), "ba"),
            // The spaces after it are read, but after a control word.
            (format!("{f}\\f{{a}} b"), "(a) b"),
            (format!("{f}\\f a b"), "(a) b"),
            (format!("\\def\\y{{y}}{f}\\f\\y b"), "(y)b"),
            // An argument doesn't run on into what's after it.
            (
                "\\def\\x{X}\\def\\xy{Z}\\newcommand{\\f}[1]{#1y}\\f\\x".into(),
                "Xy",
            ),
            ("\\newcommand{\\f}[1]{a\\\\#1}\\f{b}".into(), "a\\\\b"),
            ("\\newcommand{\\f}[1]{\\##1\\#}\\f{a}".into(), "\\#a\\#"),
            ("\\newcommand{\\f}[1]{#1##}\\f{a}".into(), "a\\#"),
            // What's in an argument is as it was.
            (format!("{f}\\f{{a  b}}"), "(a b)"),
            (format!("{f}\\f{{a%c\nb}}"), "(ab)"),
            (format!("{f}\\f{{\\verb|a#1|}}"), "(\\verb|a#1|)"),
            (format!("{f}\\f{{$x^2$}}"), "($x^2$)"),
            (
                "\\newcommand{\\f}[1]{$\\text{#1 c}$}\\f{a b}".into(),
                "$\\text{a b c}$",
            ),
            (format!("{f}\\f{{\\emph{{a}}}}"), "(\\emph{a})"),
            // An optional first argument.
            ("\\newcommand{\\f}[2][d]{#1#2}\\f{a}".into(), "da"),
            ("\\newcommand{\\f}[2][d]{#1#2}\\f[c]{a}".into(), "ca"),
            // What it stands for is read as the document is.
            (
                "\\newcommand{\\f}[1]{\\section{#1}}\\f{a} b".into(),
                "\\section{a} b",
            ),
            (
                format!("{f}\\newcommand{{\\g}}[1]{{\\f{{#1}}\\f{{#1}}}}\\g{{a}}"),
                "(a)(a)",
            ),
            ("\\newcommand{\\f}[1]{\\def\\y{#1}}\\f{a}\\y".into(), "a"),
            // A command it ends in takes its arguments from after the call.
            (
                "\\newcommand{\\id}[1]{#1}A\\id\\footnote{note}B".into(),
                "A\\footnote{note}B",
            ),
            (
                "\\newcommand{\\id}[1]{#1}\\id\\verb|hello|".into(),
                "\\verb|hello|",
            ),
            (
                format!("{f}\\newcommand{{\\g}}[1]{{#1\\f}}\\g{{a}}{{b}} c"),
                "a(b) c",
            ),
            (
                "\\newcommand{\\f}[1]{(#1)\\textbf }\\f{a}{b}".into(),
                "(a)\\textbf{b}",
            ),
            // Not one a command before it takes.
            (
                "\\newcommand{\\id}[1]{#1}\\id{\\textbf\\LaTeX} b".into(),
                "\\textbf\\LaTeX{} b",
            ),
            (
                format!("{f}\\newcommand{{\\id}}[1]{{#1}}\\id{{\\f\\LaTeX}} b"),
                "(LaTeX) b",
            ),
            // A command taking none, or one a command before took, doesn't.
            (
                "\\newcommand{\\id}[1]{#1}\\id{\\footnotemark\\textbf}{b}".into(),
                "\\footnotemark\\textbf{b}",
            ),
            (
                "\\newcommand{\\id}[1]{#1}\\id{\\textbf\\footnotemark\\textbf}{b}".into(),
                "\\textbf\\footnotemark\\textbf{b}",
            ),
            // A control symbol's spaces are text, unless it takes arguments.
            ("\\newcommand{\\id}[1]{#1}\\id{\\%  }x".into(), "\\% x"),
            (
                "\\newcommand{\\?}[1]{(#1)}\\newcommand{\\id}[1]{#1}\\id{\\? }{v}".into(),
                "(v)",
            ),
            ("\\newcommand{\\id}[1]{#1}\\id{\\%}x".into(), "\\%x"),
            // Short of arguments, it's unknown, and what it took is text.
            ("\\newcommand{\\f}[2]{#1#2}a\\f{b}".into(), "ab"),
            ("\\newcommand{\\f}[2][d]{#1#2}a\\f[c] ".into(), "a[c]"),
            ("\\newcommand{\\f}[1]{#1}a\\f\n\nb".into(), "a\n\nb"),
        ];
        for (source, written) in cases {
            assert_eq!(read(&source).0.blocks, read(written).0.blocks, "{source}");
        }
    }

    /// A macro taking arguments that calls itself ends, with the warning
    /// for that, however it grows.
    #[test]
    fn a_macro_taking_arguments_that_never_ends_is_cut_short() {
        for source in [
            "\\newcommand{\\a}[1]{\\a{#1}}\\a{x} done",
            "\\newcommand{\\a}[1]{\\a{#1#1}}\\a{x} done",
            "\\newcommand{\\a}[1]{#1\\a{#1}}\\a{x} done",
            "\\newcommand{\\a}[1]{{\\a{#1}}}\\a{x} done",
            "\\newcommand{\\a}[1]{\\a{{#1}}}\\a{x} done",
            "\\newcommand{\\a}[1]{\\a#1\\a#1}\\a x done",
            "\\newcommand{\\a}[1][d]{\\a}\\a done",
            "\\newcommand{\\a}[1][d]{#1\\a}\\a done",
        ] {
            let (doc, warnings) = read(source);
            assert!(format!("{:?}", doc.blocks).contains("done"), "{source}");
            assert_eq!(warnings.len(), 1, "{source}");
            assert!(warnings[0].one.contains("unexpanded"), "{source}");
        }
    }

    /// Each row: a source, and what it reads as with its macros written out.
    /// Tables and lists find their rows, cells and items in macros too.
    #[test]
    fn macros_hold_rows_and_items() {
        let cases = [
            (
                "\\def\\myrow{a&b\\\\}\\begin{tabular}{ll}\\myrow c&d\\\\\\end{tabular}",
                "\\begin{tabular}{ll}a&b\\\\c&d\\\\\\end{tabular}",
            ),
            (
                "\\def\\mycell{b}\\def\\myrow{a&\\mycell\\\\}\\begin{tabular}{ll}\\myrow\\end{tabular}",
                "\\begin{tabular}{ll}a&b\\\\\\end{tabular}",
            ),
            (
                "\\def\\myrule{\\hline}\\begin{tabular}{l}a\\\\\\myrule b\\\\\\end{tabular}",
                "\\begin{tabular}{l}a\\\\\\hline b\\\\\\end{tabular}",
            ),
            (
                "\\def\\myend{\\\\}\\begin{tabular}{l}a\\myend b\\myend\\end{tabular}",
                "\\begin{tabular}{l}a\\\\b\\\\\\end{tabular}",
            ),
            (
                "\\def\\myitem{\\item a}\\begin{itemize}\\myitem\\item b\\end{itemize}",
                "\\begin{itemize}\\item a\\item b\\end{itemize}",
            ),
            (
                "\\def\\myitem{\\item a}\\begin{itemize}\\item\\begin{itemize}\\myitem\\end{itemize}\\end{itemize}",
                "\\begin{itemize}\\item\\begin{itemize}\\item a\\end{itemize}\\end{itemize}",
            ),
            // The spaces after a call go, as they do anywhere.
            (
                "\\def\\x{a}\\begin{itemize}\\item\\x b\\end{itemize}",
                "\\begin{itemize}\\item ab\\end{itemize}",
            ),
            // A call that's an argument is taken whole.
            (
                "\\def\\x{ab}\\begin{itemize}\\item T\\footnote\\x\\end{itemize}",
                "\\begin{itemize}\\item T\\footnote{ab}\\end{itemize}",
            ),
            (
                "\\def\\x{ab}\\begin{tabular}{l}\\textbf\\x\\\\\\end{tabular}",
                "\\begin{tabular}{l}\\textbf{ab}\\\\\\end{tabular}",
            ),
            // What's defined there holds from there, in a group too.
            (
                "\\def\\x{a}\\begin{itemize}\\item{\\gdef\\x{b}}\\x\\end{itemize}",
                "\\begin{itemize}\\item b\\end{itemize}",
            ),
            (
                "\\def\\x{a}\\begin{tabular}{l}{\\gdef\\x{b}}\\x\\\\\\end{tabular}",
                "\\begin{tabular}{l}b\\\\\\end{tabular}",
            ),
            (
                "\\def\\x{a}\\begin{itemize}\\item\\def\\x{b}\\x\\end{itemize}",
                "\\begin{itemize}\\item b\\end{itemize}",
            ),
            (
                "\\def\\x{a}\\def\\y{\\def\\x{b}}\\begin{itemize}\\item\\y\\x\\end{itemize}",
                "\\begin{itemize}\\item b\\end{itemize}",
            ),
        ];
        for (source, written_out) in cases {
            assert_eq!(
                read(source).0.blocks,
                read(written_out).0.blocks,
                "{source}"
            );
        }
    }

    /// What each call reads is counted, comments too, so that a long one
    /// calling itself is cut short after a few calls, not after millions.
    #[test]
    fn a_macro_is_counted_with_its_comments() {
        let comment = format!("%{}\n", "x".repeat(1 << 20));
        let source = format!("\\def\\a{{{comment}\\a\\a}}\\a done");
        let (doc, warnings) = read(&source);
        assert!(format!("{:?}", doc.blocks).contains("done"));
        assert!(warnings[0].count < 64, "{}", warnings[0].count);
    }

    /// One calling itself ends, or calling others twice over, each level
    /// doubling: what's after it stays, with a warning counting what was
    /// left unexpanded.
    #[test]
    fn a_macro_that_never_ends_is_cut_short() {
        let mut doubling = "\\def\\xa{xxxxxxxx}".to_string();
        let mut last = 'a';
        for name in 'b'..='z' {
            doubling.push_str(&format!("\\def\\x{name}{{\\x{last}\\x{last}}}"));
            last = name;
        }
        doubling.push_str("\\xz done");
        // Nesting as deep as the document does, at every call, would take
        // more than the stack it's sized for.
        let braces = 20_000;
        let nesting = format!(
            "\\def\\a{{{}\\a{}}}\\a done",
            "{".repeat(braces),
            "}".repeat(braces)
        );
        let sources = [
            "\\newcommand{\\again}{\\again}\\again done".to_string(),
            "\\def\\a{\\a\\a}\\a done".to_string(),
            "\\def\\a{x\\a\\a\\a}\\a done".to_string(),
            "\\def\\a{\\b\\b}\\def\\b{\\a\\a}\\a done".to_string(),
            "\\def\\a{\\a\\a}\\begin{itemize}\\item x\\a\\end{itemize} done".to_string(),
            "\\def\\tb{\\begin{tabular}{l}\\tb\\\\\\end{tabular}}\
             \\begin{tabular}{l}\\tb\\\\\\end{tabular} done"
                .to_string(),
            doubling,
            nesting,
        ];
        let count = sources.len();
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for source in sources {
                sent.send((source.clone(), read(&source))).unwrap();
            }
        });
        for _ in 0..count {
            let (source, (doc, warnings)) = received
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("reading never ended");
            let start: String = source.chars().take(40).collect();
            let blocks = format!("{:?}", doc.blocks);
            assert!(blocks.contains("done"), "{start}");
            assert!(blocks.len() < 1 << 22, "{start}: {}", blocks.len());
            assert_eq!(warnings.len(), 1, "{start}");
            assert!(warnings[0].one.contains("unexpanded"), "{start}");
        }
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
            warning[0].message(),
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
