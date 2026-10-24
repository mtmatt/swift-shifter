//! Typst markup → [`Document`], for documents written as text: headings,
//! paragraphs and line breaks, lists and term lists (as markup or calls),
//! emphasis, code, math (converted to TeX), links, labels and references,
//! footnotes, images, figures and their captions, quotes and their
//! attributions, and tables. The document's `#set document(…)` and `#set
//! text(lang: …)` become its metadata, `#set enum(start: …)` numbers its
//! lists, and `#set math.equation(numbering: …)` its display equations.
//!
//! Styling (`set`/`show` rules, the settings given to `#text(…)` and the
//! like, spacing) and code (`let`, loops) are dropped. What a call shows
//! stays where typst shows it: inline, in the paragraph around the call,
//! with its paragraph breaks and blocks breaking that paragraph, but in a
//! box, which keeps what it holds in the line unless it can't be written
//! there (a table). A function of typst's own shows the arguments its
//! parameters take as content, and the string a value it makes is made of
//! (`int("12")`). One the document defines shows the content and strings
//! it's given for the parameters its body shows, or nothing if it returns
//! a setting (`#let gap(x) = 1em`); one imported from where the reader
//! can't see shows the content and strings it's given, but for strings
//! before its content, where functions take their settings. A method of a
//! value typst makes shows nothing (`state("x").update(…)`), and one of
//! another value the document makes the content it's given. The names
//! `#import` and `#let` bind are followed, into
//! the `.typ` files imported from the input's folder, for the tables,
//! grids and table parts they stand for, the arguments the functions and
//! presets wrapping those give them (`#let wide(body) =
//! table.cell(colspan: 2, body)`), and the functions of typst's own they
//! wrap under the same name (`#let link(..args) = std.link(..args)`).

mod finish;
mod names;
mod signature;
mod table;
mod wrapper;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use typst::model::NumberingPattern;
use typst::syntax::SyntaxNode;
use typst::syntax::ast::{self, AstNode, Expr};

use super::markdown::push_text;
use super::{typst_math, walk};
use crate::converter::document::native::Warning;
use crate::converter::document::native::ast::{
    Block, Document, Inline, List, ListItem, Meta, RawFormat,
};

use finish::{
    FRAME_MARK, HEADING_MARK, LABEL_LINK, MAX_REFERENCE_TEXT, QUOTE_MARK, REFERENCE_TEXT_PER_BYTE,
    equation_supplement, quoted_text, spend_on_reference,
};
use names::{Module, Name, Names, PartNames};
use signature::{Given, std_symbol_text};
use table::{MAX_GROWN_CELLS, array_items};
use wrapper::{MAX_UNWRAPPED, Presets, UNWRAPPED_PER_BYTE};

/// [`read_in`] for a document with no folder: files it imports aren't read.
#[cfg(test)]
pub fn read(source: &str) -> Document {
    Reader::default().read(source).0
}

#[cfg(test)]
thread_local! {
    /// How many equation numbers the reader has made from their pattern on
    /// this thread, for tests to tell numbers it skipped making.
    static NUMBERS_APPLIED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`read_in`], also returning what of the document's code that would
/// show something the reader didn't run (see [`Skipped`]).
pub fn read_in_reporting(source: &str, dir: &Path) -> (Document, Vec<Warning>) {
    // `in.typ`'s parent is the empty path.
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let root = dir.canonicalize().ok();
    let reader = Reader {
        dir: root.clone(),
        root,
        ..Reader::default()
    };
    reader.read(source)
}

/// Read `source`, a typst file in `dir`. The `.typ` files it imports from
/// inside `dir` are read for what they define (see [`Reader::module`]).
#[cfg(test)]
pub fn read_in(source: &str, dir: &Path) -> Document {
    read_in_reporting(source, dir).0
}

/// The line (from 1) and the byte each piece of [`Skipped`] code in `root`,
/// parsed from `source`, starts at, by its node, wherever it is: in markup
/// or given to a call. Walked with a stack of its own, as deep as typst
/// parses, past what the stack conversions run on holds; numbering the
/// spans instead (`Source::detached`) recurses as deep, and overflows it.
fn skipped_places(root: &SyntaxNode, source: &str) -> HashMap<*const SyntaxNode, (usize, usize)> {
    // Typst's own, which ends lines where typst does (CR, CRLF, LF and
    // Unicode's line and paragraph separators).
    let source_lines = typst::syntax::Lines::new(source);
    let mut places = HashMap::new();
    let mut nodes = vec![(root, 0)];
    while let Some((node, start)) = nodes.pop() {
        if node.cast::<Expr>().and_then(Skipped::of).is_some()
            && let Some(line) = source_lines.byte_to_line(start)
        {
            places.insert(node as *const SyntaxNode, (line + 1, start));
        }
        let mut offset = start;
        for child in node.children() {
            nodes.push((child, offset));
            offset += child.len();
        }
    }
    places
}

/// What code the reader didn't run (see [`Reader::skip`]).
#[derive(Default)]
struct Skips {
    /// How much of each [`Skipped`] kind, and the line of the first.
    tally: [(usize, Option<usize>); Skipped::ALL.len()],
    /// The code counted: each piece once, however often it's read or
    /// placed in table parts. A table's part made beforehand is read only
    /// where it's placed (#113), so the code it holds is counted there,
    /// and not at all if it isn't placed: it needs holding no more, as it
    /// did while parts were read where they were made.
    counted: HashSet<SkipId>,
}

/// A piece of code the reader didn't run, as it's counted once: where it
/// is, in the input (no file) or an imported one (its canonical path),
/// which outlasts its syntax; or, where the reader has no place for it,
/// its node.
#[derive(Clone, PartialEq, Eq, Hash)]
enum SkipId {
    Place(Option<Rc<Path>>, usize),
    Node(*const SyntaxNode),
}

/// Where each piece of [`Skipped`] code in a file starts: its line and byte,
/// by its node (see [`skipped_places`]).
type Places = Rc<HashMap<*const SyntaxNode, (usize, usize)>>;

/// Code the reader doesn't run that would show something (see
/// [`Reader::skip`]). Imported files show nothing, so what they hold
/// isn't counted.
#[derive(Clone, Copy)]
enum Skipped {
    Loop,
    Conditional,
    CodeBlock,
    Context,
    Value,
}

impl Skipped {
    /// The kind of code `expr` is, if the reader doesn't run it and typst
    /// would show something.
    fn of(expr: Expr) -> Option<Skipped> {
        Some(match expr {
            Expr::ForLoop(_) | Expr::WhileLoop(_) => Skipped::Loop,
            // `if false` with no `else` shows nothing.
            Expr::Conditional(conditional)
                if !(matches!(conditional.condition(), Expr::Bool(b) if !b.get())
                    && conditional.else_body().is_none()) =>
            {
                Skipped::Conditional
            }
            // A block of only bindings and rules shows nothing.
            Expr::CodeBlock(block)
                if !block.body().exprs().all(|e| {
                    matches!(
                        e,
                        Expr::LetBinding(_)
                            | Expr::SetRule(_)
                            | Expr::ShowRule(_)
                            | Expr::ModuleImport(_)
                    )
                }) =>
            {
                Skipped::CodeBlock
            }
            Expr::Contextual(_) => Skipped::Context,
            // What the parentheses hold, which the reader drops even where
            // it would show it bare (`#([text])`).
            Expr::Parenthesized(parenthesized) => {
                let inner = parenthesized.expr();
                return match inner {
                    Expr::None(_)
                    | Expr::Auto(_)
                    | Expr::Conditional(_)
                    | Expr::CodeBlock(_)
                    | Expr::Parenthesized(_) => Skipped::of(inner),
                    _ => Some(Skipped::of(inner).unwrap_or(Skipped::Value)),
                };
            }
            Expr::Unary(_)
            | Expr::Binary(_)
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::Numeric(_)
            | Expr::Array(_)
            | Expr::Dict(_) => Skipped::Value,
            _ => return None,
        })
    }

    const ALL: [Skipped; 5] = [
        Skipped::Loop,
        Skipped::Conditional,
        Skipped::CodeBlock,
        Skipped::Context,
        Skipped::Value,
    ];

    /// What [`Warning`] says of one and of several.
    fn said(self) -> (&'static str, &'static str) {
        match self {
            Skipped::Loop => ("Typst loop wasn't run", "Typst loops weren't run"),
            Skipped::Conditional => (
                "Typst condition wasn't evaluated",
                "Typst conditions weren't evaluated",
            ),
            Skipped::CodeBlock => (
                "Typst code block wasn't run",
                "Typst code blocks weren't run",
            ),
            Skipped::Context => (
                "Typst context expression wasn't run",
                "Typst context expressions weren't run",
            ),
            Skipped::Value => (
                "computed Typst value wasn't shown",
                "computed Typst values weren't shown",
            ),
        }
    }
}

/// How deep calls and markup (a content block, a list item) are read
/// inside each other, a call and the content blocks it's given one level
/// (see [`Reader::nested`]): as deep as typst parses calls given content
/// (126 deep, `#text[#text[…]]`), and shallow enough for the 2 MiB stack
/// conversions run on, which the 256 levels typst's parser allows other
/// calls (`#f(f(…))`) would overflow.
const MAX_NESTING: usize = 128;

#[derive(Default)]
struct Reader {
    meta: Meta,
    /// Footnote bodies in reading order, appended after the text; note `n`
    /// is labelled `note-{n}`.
    notes: Vec<Block>,
    /// Footnote labels by the typst `<label>` given to the note.
    note_labels: HashMap<String, String>,
    /// Heading ids (their [`HEADING_MARK`]) by the typst `<label>` on them.
    heading_labels: HashMap<String, usize>,
    /// Numbered equations by the typst `<label>` on them.
    equation_labels: HashMap<String, EquationRef>,
    /// How `#set math.equation(numbering: …)` numbers display equations,
    /// scoped like the names; `None` leaves them unnumbered.
    equation_numbering: Option<NumberingPattern>,
    /// What `#set math.equation(supplement: …)` makes references to
    /// equations show before the number, scoped like the names: `None`
    /// typst's own, in [`Reader::text_lang`], and `Some(None)` nothing.
    /// Shared by the equations it's set for, however long it is.
    equation_supplement: Option<Option<Rc<str>>>,
    /// Whether `#set math.equation(block: …)` displays the equations
    /// `math.equation` calls make, scoped like the names: `$ … $` says for
    /// itself.
    equation_block: bool,
    /// The language `#set text(lang:)` sets, scoped like the names.
    text_lang: Option<Rc<str>>,
    /// Whether what's read isn't laid out (the document's title), so its
    /// equations aren't numbered whatever is set inside it.
    unlaid: bool,
    /// Numbered equations read so far.
    equations_numbered: u64,
    /// Headings read so far: the next one's id.
    headings_read: usize,
    /// What the names bound by `#import` and `#let` stand for; a name not
    /// here is the standard library's, if it has one (see
    /// [`Reader::resolve`]).
    names: Names,
    /// The names the table parts made beforehand keep, in every file read
    /// (see [`Name::Part`]).
    part_names: PartNames,
    /// The number `#set enum(start: …)` starts lists at, scoped like the
    /// names.
    enum_start: Option<u64>,
    /// The folder imported files must be in, canonical; `None` reads none.
    root: Option<PathBuf>,
    /// The folder of the file being read, canonical.
    dir: Option<PathBuf>,
    /// How many imports deep the file being read is.
    depth: usize,
    /// How deep in calls and markup the reader is (see
    /// [`Reader::nested`]).
    nesting: usize,
    /// The level of the call whose arguments are being read, while they
    /// are: the content blocks it's given are at its level (see
    /// [`Reader::flow`]).
    call_level: Option<usize>,
    /// The files imported so far, by canonical path: their names, or
    /// `None` while they're being read (see [`Reader::module`]).
    modules: HashMap<PathBuf, Option<Module>>,
    /// What's left of what the reader may spend unwrapping calls, in every
    /// file (see [`MAX_UNWRAPPED`]).
    unwrapping_left: usize,
    /// How many more cells tables may grow by past those they're given, in
    /// every file (see [`MAX_GROWN_CELLS`]).
    grown_cells_left: usize,
    /// What's left of what references to headings may copy of their text
    /// (see [`MAX_REFERENCE_TEXT`]).
    reference_text_left: usize,
    /// The line and the byte each piece of [`Skipped`] code in the file
    /// being read starts at, by its node. Code from elsewhere (a function
    /// imported from another file) has none.
    skipped_places: Places,
    /// The file being read, canonical, if it's imported; `None` for the
    /// input.
    file: Option<Rc<Path>>,
    /// The code the reader didn't run (see [`Reader::skip`]), counted
    /// where arguments are only looked at, too.
    skipped: RefCell<Skips>,
}

/// What a function call (or a content block) shows, as it continues the
/// paragraph around it, the way typst lays it out.
enum Item {
    /// Inline content: it stays in the paragraph around the call.
    Inlines(Vec<Inline>),
    /// Content that breaks the paragraph: `head` ends the paragraph before
    /// the call, `blocks` follow, and `tail` starts the paragraph after it.
    /// Blocks of their own (a table, a figure) have neither.
    Blocks {
        head: Vec<Inline>,
        blocks: Vec<Block>,
        tail: Vec<Inline>,
    },
}

impl Item {
    /// Blocks of their own: the paragraph around the call ends before them
    /// and starts again after them.
    fn blocks(blocks: Vec<Block>) -> Item {
        Item::Blocks {
            head: Vec::new(),
            blocks,
            tail: Vec::new(),
        }
    }

    /// `self`, then `next`, with a space between their inline content.
    fn then(self, next: Item) -> Item {
        match (self, next) {
            (Item::Inlines(mut content), Item::Inlines(more)) => {
                join_run(&mut content, more);
                Item::Inlines(content)
            }
            (Item::Inlines(mut content), Item::Blocks { head, blocks, tail }) => {
                join_run(&mut content, head);
                Item::Blocks {
                    head: content,
                    blocks,
                    tail,
                }
            }
            (
                Item::Blocks {
                    head,
                    blocks,
                    mut tail,
                },
                Item::Inlines(more),
            ) => {
                join_run(&mut tail, more);
                Item::Blocks { head, blocks, tail }
            }
            (
                Item::Blocks {
                    head,
                    mut blocks,
                    mut tail,
                },
                Item::Blocks {
                    head: next_head,
                    blocks: next_blocks,
                    tail: next_tail,
                },
            ) => {
                join_run(&mut tail, next_head);
                blocks.extend(paragraph(tail));
                blocks.extend(next_blocks);
                Item::Blocks {
                    head,
                    blocks,
                    tail: next_tail,
                }
            }
        }
    }

    /// The blocks `self` makes where blocks go on their own (a list item,
    /// a note): its inline content in paragraphs.
    fn into_blocks(self) -> Vec<Block> {
        match self {
            Item::Inlines(content) => paragraph(content).into_iter().collect(),
            Item::Blocks { head, blocks, tail } => paragraph(head)
                .into_iter()
                .chain(blocks)
                .chain(paragraph(tail))
                .collect(),
        }
    }

    /// Whether `self` can be written inside a line, as typst lays out a
    /// box: unless it holds blocks other than paragraphs, display math and
    /// rules, which have an inline form (see [`flatten`]; a rule shows
    /// nothing there). A table or a list has none worth the name.
    fn fits_inline(&self) -> bool {
        match self {
            Item::Inlines(_) => true,
            Item::Blocks { blocks, .. } => blocks.iter().all(|block| {
                matches!(
                    block,
                    Block::Paragraph(_) | Block::Math(_) | Block::ThematicBreak
                )
            }),
        }
    }

    /// The inline content of `self`, for where only inlines can go (in
    /// emphasis, a table cell): its blocks give their text (see
    /// [`flatten`]), a space on either side.
    fn into_inlines(self) -> Vec<Inline> {
        match self {
            Item::Inlines(content) => content,
            Item::Blocks { head, blocks, tail } => {
                let mut out = head;
                join_run(&mut out, flatten(blocks));
                join_run(&mut out, tail);
                out
            }
        }
    }

    /// `self` in `style` (`Inline::Strong`, …): its inline content and the
    /// paragraphs and table cells of its blocks, as typst styles each.
    fn styled(self, style: fn(Vec<Inline>) -> Inline) -> Item {
        match self {
            Item::Inlines(content) => Item::Inlines(styled(style, content)),
            Item::Blocks {
                head,
                mut blocks,
                tail,
            } => {
                walk::for_each_block(&mut blocks, &mut |block| match block {
                    Block::Paragraph(content) => {
                        *content = styled(style, std::mem::take(content));
                    }
                    Block::Table(table) => {
                        for cell in table
                            .header
                            .iter_mut()
                            .chain(table.rows.iter_mut().flatten())
                        {
                            *cell = styled(style, std::mem::take(cell));
                        }
                    }
                    _ => {}
                });
                Item::Blocks {
                    head: styled(style, head),
                    blocks,
                    tail: styled(style, tail),
                }
            }
        }
    }
}

/// Append `more` to the run of inlines `run`, with a space between them if
/// both show something.
fn join_run(run: &mut Vec<Inline>, more: Vec<Inline>) {
    if !run.is_empty() && !more.is_empty() {
        push_text(run, " ");
    }
    run.extend(more);
}

/// A paragraph of `content`, trimmed, unless that leaves nothing.
fn paragraph(content: Vec<Inline>) -> Option<Block> {
    let content = trim(content);
    (!content.is_empty()).then_some(Block::Paragraph(content))
}

/// The blocks read so far from a run of markup, and the paragraph and the
/// list being read (see [`Reader::flow`]).
#[derive(Default)]
struct Flow {
    /// The inline content before the first paragraph break, once there's
    /// been one.
    head: Option<Vec<Inline>>,
    blocks: Vec<Block>,
    /// The paragraph being read.
    inline: Vec<Inline>,
    /// The list being read: list items join it until something else ends
    /// it.
    list: Option<List>,
    /// A blank line after a list item: the list is loose if another item
    /// follows.
    break_in_list: bool,
}

impl Flow {
    /// End the paragraph being read: the first one is the head, which the
    /// paragraph around the markup continues.
    fn break_paragraph(&mut self) {
        let run = std::mem::take(&mut self.inline);
        if self.head.is_none() {
            self.head = Some(run);
        } else {
            self.blocks.extend(paragraph(run));
        }
    }

    /// A blank line.
    fn parbreak(&mut self) {
        self.break_paragraph();
        self.break_in_list = self.list.is_some();
    }

    fn end_list(&mut self) {
        if let Some(list) = self.list.take() {
            self.blocks.push(Block::List(list));
        }
    }

    /// Blocks of their own, after the paragraph and the list.
    fn push_blocks(&mut self, blocks: Vec<Block>) {
        self.break_paragraph();
        self.end_list();
        self.blocks.extend(blocks);
    }

    /// What a call or content block shows (see [`Item`]).
    fn push(&mut self, item: Item) {
        match item {
            Item::Inlines(content) => self.inline.extend(content),
            Item::Blocks { head, blocks, tail } => {
                self.inline.extend(head);
                self.push_blocks(blocks);
                self.inline = tail;
            }
        }
    }

    /// Append a list item, continuing the current list when it's the same
    /// kind: `start` is `None` for a bullet list's item, and an enum's
    /// number otherwise. A blank line before it makes the list loose, as
    /// an item holding blocks that would run together does (see
    /// [`loosens`]).
    fn push_item(&mut self, start: Option<u64>, content: Vec<Block>) {
        self.break_paragraph();
        let same_kind = self
            .list
            .as_ref()
            .is_some_and(|l| l.start.is_some() == start.is_some());
        if !same_kind {
            self.end_list();
        }
        let list = self.list.get_or_insert_with(|| List {
            start,
            tight: true,
            items: Vec::new(),
        });
        if (same_kind && self.break_in_list) || loosens(&content) {
            list.tight = false;
        }
        list.items.push(ListItem {
            task: None,
            content,
        });
        self.break_in_list = false;
    }

    fn finish(mut self) -> Item {
        self.end_list();
        match self.head {
            None => Item::Inlines(self.inline),
            Some(head) => Item::Blocks {
                head,
                blocks: self.blocks,
                tail: self.inline,
            },
        }
    }
}

/// A `list.item(…)` or `enum.item(…)` call (see [`Reader::item_call`]).
struct ItemCall {
    /// Whether it's an enum's item.
    numbered: bool,
    /// The number given an enum's item.
    number: Option<u64>,
    content: Vec<Block>,
}

/// A typst element a following `<label>` attaches to.
#[derive(Clone)]
enum Labelled {
    Heading(usize),
    Note(String),
    Equation(EquationRef),
}

/// What a reference to a numbered equation needs.
#[derive(Clone)]
struct EquationRef {
    /// The index of its id (see [`equation_id`]).
    index: u64,
    /// The number a reference shows.
    number: String,
    /// The supplement set where it is (see [`Reader::equation_supplement`]).
    supplement: Option<Option<Rc<str>>>,
    /// Typst's own supplement in the language where it is, shown where none
    /// is set.
    default_supplement: &'static str,
}

impl EquationRef {
    /// What a reference to it shows ("Equation 1"). Made only once it's
    /// paid for (see [`EquationRef::text_len`]), so a long supplement
    /// referred to again and again isn't copied past what references may
    /// copy.
    fn text(&self) -> String {
        match self.supplement() {
            Some(supplement) => format!("{supplement} {}", self.number),
            None => self.number.clone(),
        }
    }

    /// The length of [`EquationRef::text`].
    fn text_len(&self) -> usize {
        self.supplement()
            .map_or(0, |supplement| supplement.len() + 1)
            + self.number.len()
    }

    fn supplement(&self) -> Option<&str> {
        match &self.supplement {
            None => Some(self.default_supplement),
            Some(supplement) => supplement.as_deref(),
        }
    }
}

impl Reader {
    /// The document `source` holds, read to the end (see
    /// [`Reader::finish`]), and what of its code it didn't run.
    fn read(mut self, source: &str) -> (Document, Vec<Warning>) {
        self.unwrapping_left = MAX_UNWRAPPED.max(source.len().saturating_mul(UNWRAPPED_PER_BYTE));
        self.grown_cells_left = MAX_GROWN_CELLS;
        self.reference_text_left =
            MAX_REFERENCE_TEXT.max(source.len().saturating_mul(REFERENCE_TEXT_PER_BYTE));
        let root = typst::syntax::parse(source);
        self.skipped_places = Rc::new(skipped_places(&root, source));
        let blocks = match root.cast::<ast::Markup>() {
            Some(markup) => self.blocks(markup),
            None => Vec::new(),
        };
        let warnings = Skipped::ALL
            .iter()
            .zip(self.skipped.take().tally)
            .filter(|(_, (count, _))| *count > 0)
            .map(|(kind, (count, line))| {
                let (one, many) = kind.said();
                Warning {
                    one,
                    many,
                    count,
                    line,
                }
            })
            .collect();
        (self.finish(blocks), warnings)
    }

    /// Counts `expr`, code of `kind` the reader doesn't run, at its line:
    /// the reader may meet the code given to a call after what follows it.
    fn skip(&self, kind: Skipped, expr: Expr) {
        let node = expr.to_untyped() as *const SyntaxNode;
        let (id, line) = match self.skipped_places.get(&node) {
            // Lines are the input's: an imported file's would mislead.
            Some(&(line, offset)) => (
                SkipId::Place(self.file.clone(), offset),
                self.file.is_none().then_some(line),
            ),
            None => (SkipId::Node(node), None),
        };
        let mut skipped = self.skipped.borrow_mut();
        if skipped.counted.insert(id) {
            skipped.add(kind, line);
        }
    }
}

impl Skips {
    /// Counts code of `kind` on `line`.
    fn add(&mut self, kind: Skipped, line: Option<usize>) {
        let (count, first) = &mut self.tally[kind as usize];
        *count += 1;
        *first = match (*first, line) {
            (Some(first), Some(line)) => Some(first.min(line)),
            (first, line) => first.or(line),
        };
    }
}

impl Reader {
    /// Counts `expr`, an argument the reader drops from where the call
    /// shows content, if it's code, however it's written: the callers
    /// tell where content goes apart from where settings do.
    fn skip_arg(&self, expr: Expr) {
        if let Some(kind) = Skipped::of(expr) {
            self.skip(kind, expr);
        }
    }

    fn blocks(&mut self, markup: ast::Markup) -> Vec<Block> {
        self.flow(markup).into_blocks()
    }

    /// What `markup` shows, as it continues the paragraph around it (see
    /// [`Item`]), a level deeper in calls and markup, unless it's what a
    /// call is given: a call and its content are one level (see
    /// [`MAX_NESTING`]), and what that markup holds nests on from there.
    fn flow(&mut self, markup: ast::Markup) -> Item {
        if self.call_level == Some(self.nesting) {
            let level = self.call_level.take();
            let item = self.read_flow(markup);
            self.call_level = level;
            return item;
        }
        self.nested(markup.to_untyped(), |reader| reader.read_flow(markup))
    }

    /// `read` what `node` shows, one level deeper in calls and markup:
    /// past [`MAX_NESTING`] levels, deeper than typst lays out, it's kept
    /// as it's written, on one line.
    fn nested(&mut self, node: &SyntaxNode, read: impl FnOnce(&mut Reader) -> Item) -> Item {
        if self.nesting >= MAX_NESTING {
            return Item::Inlines(vec![written(node)]);
        }
        self.nesting += 1;
        let item = read(self);
        self.nesting -= 1;
        item
    }

    fn read_flow(&mut self, markup: ast::Markup) -> Item {
        let mut flow = Flow::default();
        let mut last: Option<Labelled> = None;
        for expr in markup.exprs() {
            if !labels_skip(expr) {
                last = None;
            }
            match expr {
                Expr::ListItem(item) => {
                    let content = self.rules_scoped(|reader| reader.blocks(item.body()));
                    flow.push_item(None, content);
                }
                Expr::EnumItem(item) => {
                    let content = self.rules_scoped(|reader| reader.blocks(item.body()));
                    let number = item.number().or(self.enum_start).unwrap_or(1);
                    flow.push_item(Some(number), content);
                }
                Expr::Space(_) if flow.list.is_some() => {}
                Expr::Parbreak(_) => flow.parbreak(),
                Expr::Heading(heading) => {
                    // The parser ends a heading at a label: its label
                    // follows it.
                    let content = self.rules_scoped(|reader| reader.inlines(heading.body()));
                    let heading = self.heading(heading.depth().get(), content, &mut last);
                    flow.push_blocks(heading);
                }
                Expr::Equation(eq) if eq.block() => {
                    let equation = self.display_equation(eq, &mut last);
                    flow.push_blocks(equation);
                }
                Expr::Raw(raw) if raw.block() => flow.push_blocks(vec![code_block(raw)]),
                Expr::TermItem(item) => {
                    let (term, description) = self.rules_scoped(|reader| {
                        (
                            reader.inlines(item.term()),
                            reader.inlines(item.description()),
                        )
                    });
                    flow.push_blocks(vec![Block::Paragraph(term_item(term, description))]);
                }
                Expr::Label(label) => {
                    self.attach_label(label.get(), last.take());
                }
                Expr::SetRule(rule) => self.set_rule(rule),
                Expr::FuncCall(call) => match self.item_call(expr) {
                    // Items next to each other make one list, as in markup.
                    Some(item) => {
                        let start = self.item_start(&item);
                        flow.push_item(start, item.content);
                    }
                    None => {
                        flow.end_list();
                        flow.push(self.call(call, &mut last));
                    }
                },
                Expr::ContentBlock(block) => {
                    flow.end_list();
                    flow.push(self.content_flow(block));
                }
                other => {
                    if !matches!(other, Expr::Space(_)) {
                        flow.end_list();
                    }
                    self.inline(&mut flow.inline, other, &mut last);
                }
            }
        }
        flow.finish()
    }

    /// The heading of `level` showing `content`, ending in a mark labels
    /// find it by, wherever it ends up (in a note, say); a label after it
    /// names it (`last`). A heading showing nothing is none.
    fn heading(
        &mut self,
        level: usize,
        content: Vec<Inline>,
        last: &mut Option<Labelled>,
    ) -> Vec<Block> {
        let mut content = trim(content);
        if content.is_empty() {
            return Vec::new();
        }
        let id = self.headings_read;
        self.headings_read += 1;
        content.push(Inline::Link {
            url: format!("{HEADING_MARK}{id}"),
            title: None,
            content: Vec::new(),
        });
        *last = Some(Labelled::Heading(id));
        let level = level.clamp(1, 6) as u8;
        vec![Block::Heading { level, content }]
    }

    /// A display equation. Numbered (see [`Reader::equation_numbering`]),
    /// it shows its number as `\tag*` does, after an anchor references
    /// link to, and a label after it names it (`last`).
    fn display_equation(&mut self, eq: ast::Equation, last: &mut Option<Labelled>) -> Vec<Block> {
        let tex = typst_math::equation(eq);
        let Some((index, shown)) = self.number_equation(last) else {
            return vec![Block::Math(tex)];
        };
        let anchor = Block::Raw {
            format: RawFormat::Html,
            text: format!("<a id=\"{}\"></a>", equation_id(index)),
        };
        if fits_tag(&shown) {
            vec![anchor, Block::Math(format!("{tex} \\tag*{{{shown}}}"))]
        } else {
            // A number no tag takes shows after the equation, as it is.
            vec![
                anchor,
                Block::Math(tex),
                Block::Paragraph(vec![Inline::Text(shown)]),
            ]
        }
    }

    /// A display equation inside text (`*$ x $*`), which typst sets apart
    /// all the same: inline math, numbered like [`Reader::display_equation`]
    /// but with its number after it, since `\tag` isn't allowed inline.
    fn display_equation_inline(
        &mut self,
        out: &mut Vec<Inline>,
        eq: ast::Equation,
        last: &mut Option<Labelled>,
    ) {
        let tex = typst_math::equation(eq);
        match self.number_equation(last) {
            Some((index, shown)) => push_numbered(out, index, &shown, [Inline::Math(tex)]),
            None => out.push(Inline::Math(tex)),
        }
    }

    /// Counts a display equation when equations are numbered, and names it
    /// to a label after it (`last`): its index and the number it shows.
    fn number_equation(&mut self, last: &mut Option<Labelled>) -> Option<(u64, String)> {
        if self.unlaid {
            return None;
        }
        let pattern = self.equation_numbering.as_ref()?;
        self.equations_numbered += 1;
        let index = self.equations_numbered;
        let prefix = pattern
            .pieces
            .first()
            .map_or("", |(prefix, _)| prefix.as_str());
        // Every equation repeats the prefix and suffix, which may be long:
        // they're within what the reader may copy, past which an equation
        // shows its bare number, without making the rest first.
        let least = prefix.len() + pattern.suffix.len() + index.to_string().len();
        let applied = if least <= self.reference_text_left {
            #[cfg(test)]
            NUMBERS_APPLIED.with(|n| n.set(n.get() + 1));
            Some(pattern.apply(None, &[index]).map(String::from).ok()?)
        } else {
            None
        };
        let (shown, referenced) = match applied {
            Some(shown) if spend_on_reference(&mut self.reference_text_left, shown.len()) => {
                // A reference shows the number without the pattern's prefix
                // and suffix, as typst's does: "Equation 1", not "Equation
                // (1)".
                let referenced = shown
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix(pattern.suffix.as_str()))
                    .unwrap_or(&shown)
                    .to_string();
                (shown, referenced)
            }
            // Longer than its digits (`*` numbers repeat their symbols): the
            // numbers past it are too, so it spends what's left.
            Some(_) => {
                self.reference_text_left = 0;
                (index.to_string(), index.to_string())
            }
            None => (index.to_string(), index.to_string()),
        };
        *last = Some(Labelled::Equation(EquationRef {
            index,
            number: referenced,
            supplement: self.equation_supplement.clone(),
            default_supplement: equation_supplement(self.text_lang.as_deref()),
        }));
        Some((index, shown))
    }

    /// The inline content of `markup`, its ends as they are.
    fn inlines(&mut self, markup: ast::Markup) -> Vec<Inline> {
        let mut out = Vec::new();
        let mut last = None;
        for expr in markup.exprs() {
            self.inline(&mut out, expr, &mut last);
        }
        out
    }

    /// What `[…]` shows (see [`Reader::flow`]), scoped (see
    /// [`Reader::scoped`]).
    fn content_flow(&mut self, block: ast::ContentBlock) -> Item {
        self.scoped(|reader| reader.flow(block.body()))
    }

    /// The inlines of `[…]`, trimmed, scoped (see [`Reader::scoped`]).
    fn content_inlines(&mut self, block: ast::ContentBlock) -> Vec<Inline> {
        self.scoped(|reader| trim(reader.inlines(block.body())))
    }

    /// `read` the inside of a content block: typst scopes the names bound
    /// and the rules set there to it, so past it they mean what they did
    /// before.
    fn scoped<T>(&mut self, read: impl FnOnce(&mut Reader) -> T) -> T {
        let enum_start = self.enum_start;
        self.names.open_scope();
        let out = self.rules_scoped(read);
        self.names.close_scope();
        self.enum_start = enum_start;
        out
    }

    /// `read` the inside of `*…*`, `_…_`, a heading or a list, enum or term
    /// item. Typst scopes the rules set there to it, but not the names
    /// bound: equations past it are numbered as before.
    fn rules_scoped<T>(&mut self, read: impl FnOnce(&mut Reader) -> T) -> T {
        let equation_numbering = self.equation_numbering.clone();
        let equation_supplement = self.equation_supplement.clone();
        let equation_block = self.equation_block;
        let text_lang = self.text_lang.clone();
        let out = read(self);
        self.equation_numbering = equation_numbering;
        self.equation_supplement = equation_supplement;
        self.equation_block = equation_block;
        self.text_lang = text_lang;
        out
    }

    /// Append the inline content of `expr`. Block-level markup met inline
    /// (a paragraph break inside `*…*`) becomes a space.
    fn inline(&mut self, out: &mut Vec<Inline>, expr: Expr, last: &mut Option<Labelled>) {
        if !labels_skip(expr) {
            *last = None;
        }
        match expr {
            Expr::Text(text) => push_text(out, text.get()),
            Expr::Space(space) => {
                if space.to_untyped().leaf_text().contains('\n') {
                    out.push(Inline::SoftBreak);
                } else {
                    push_text(out, " ");
                }
            }
            Expr::Linebreak(_) => out.push(Inline::LineBreak),
            Expr::Parbreak(_) => push_text(out, " "),
            Expr::Escape(escape) => push_text(out, &escape.get().to_string()),
            Expr::Shorthand(shorthand) => push_text(out, &shorthand.get().to_string()),
            // Which quote it is depends on the whole paragraph before it.
            Expr::SmartQuote(quote) => out.push(Inline::Link {
                url: format!("{QUOTE_MARK}{}", if quote.double() { '"' } else { '\'' }),
                title: None,
                content: Vec::new(),
            }),
            Expr::Strong(strong) => {
                let content = self.rules_scoped(|reader| reader.inlines(strong.body()));
                out.extend(styled(Inline::Strong, content));
            }
            Expr::Emph(emph) => {
                let content = self.rules_scoped(|reader| reader.inlines(emph.body()));
                out.extend(styled(Inline::Emph, content));
            }
            Expr::Raw(raw) => out.push(Inline::Code(raw_text(raw))),
            Expr::Link(link) => out.push(Inline::Link {
                url: link.get().to_string(),
                title: None,
                content: vec![Inline::Text(link.get().to_string())],
            }),
            Expr::Ref(reference) => out.push(label_ref(reference.target())),
            Expr::Label(label) => self.attach_label(label.get(), last.take()),
            Expr::Equation(eq) if eq.block() => self.display_equation_inline(out, eq, last),
            Expr::Equation(eq) => out.push(Inline::Math(typst_math::equation(eq))),
            Expr::Str(text) => push_text(out, &text.get()),
            Expr::ContentBlock(block) => out.extend(self.content_flow(block).into_inlines()),
            Expr::FuncCall(call) => out.extend(self.call(call, last).into_inlines()),
            Expr::SetRule(rule) => self.set_rule(rule),
            Expr::LetBinding(binding) => self.let_binding(binding),
            Expr::ModuleImport(import) => self.import(import),
            // Block markup inside inline content: its text.
            Expr::Heading(heading) => {
                out.extend(self.rules_scoped(|reader| reader.inlines(heading.body())));
            }
            Expr::ListItem(item) => {
                out.extend(self.rules_scoped(|reader| reader.inlines(item.body())));
            }
            Expr::EnumItem(item) => {
                out.extend(self.rules_scoped(|reader| reader.inlines(item.body())));
            }
            Expr::TermItem(item) => {
                let (term, description) = self.rules_scoped(|reader| {
                    (
                        reader.inlines(item.term()),
                        reader.inlines(item.description()),
                    )
                });
                out.extend(term_item(term, description));
            }
            // A symbol of typst's own (`#sym.arrow`, `#emoji.rocket`): its
            // text. Other values the reader can't tell.
            Expr::Ident(_) | Expr::FieldAccess(_) => {
                if let Name::Function(path) = self.resolve(expr)
                    && let Some(text) = std_symbol_text(&path)
                {
                    push_text(out, &text);
                }
            }
            // Code and values the reader doesn't run: dropped, and counted
            // where typst would show something. Show rules aren't counted
            // yet.
            _ => {
                if let Some(kind) = Skipped::of(expr) {
                    self.skip(kind, expr);
                }
            }
        }
    }

    fn attach_label(&mut self, label: &str, target: Option<Labelled>) {
        match target {
            Some(Labelled::Heading(index)) => {
                self.heading_labels.insert(label.to_string(), index);
            }
            Some(Labelled::Note(note)) => {
                self.note_labels.insert(label.to_string(), note);
            }
            Some(Labelled::Equation(equation)) => {
                self.equation_labels.insert(label.to_string(), equation);
            }
            None => {}
        }
    }

    /// `#set document(title:, author:, date:)`, `#set text(lang:)`, `#set
    /// enum(start:)` and `#set math.equation(numbering:, supplement:)`,
    /// each a value or `none`. A rule set `if false` sets nothing; one set
    /// on a condition the reader can't tell is taken to hold.
    fn set_rule(&mut self, rule: ast::SetRule) {
        if let Some(Expr::Bool(condition)) = rule.condition()
            && !condition.get()
        {
            return;
        }
        let args = Args::new(rule.args());
        if let Expr::FieldAccess(access) = rule.target()
            && let Expr::Ident(module) = access.target()
            && (module.as_str(), access.field().as_str()) == ("math", "equation")
        {
            return self.equation_settings(&args);
        }
        let Expr::Ident(target) = rule.target() else {
            return;
        };
        match target.as_str() {
            // What typst keeps of the document but doesn't lay out: its
            // equations aren't numbered.
            "document" => self.unnumbered(|reader| reader.document_rule(&args)),
            "text" => {
                if let Some(Expr::Str(lang)) = args.named("lang") {
                    let region = match args.named("region") {
                        Some(Expr::Str(region)) => format!("-{}", region.get().to_uppercase()),
                        _ => String::new(),
                    };
                    let lang = format!("{}{region}", lang.get());
                    self.text_lang = Some(lang.as_str().into());
                    self.meta.lang = Some(lang);
                }
            }
            "enum" => {
                if let Some(Expr::Int(start)) = args.named("start") {
                    self.enum_start = u64::try_from(start.get()).ok();
                }
            }
            _ => {}
        }
    }

    /// The `numbering:`, `supplement:` and `block:` of `math.equation`, set
    /// or given to a call.
    fn equation_settings(&mut self, args: &Args) {
        if let Some(Expr::Bool(block)) = args.named("block") {
            self.equation_block = block.get();
        }
        match args.named("numbering") {
            Some(Expr::Str(pattern)) => self.equation_numbering = pattern.get().parse().ok(),
            Some(Expr::None(_)) => self.equation_numbering = None,
            _ => {}
        }
        match args.named("supplement") {
            Some(Expr::Auto(_)) => self.equation_supplement = None,
            Some(Expr::None(_)) => self.equation_supplement = Some(None),
            // Shown by references, not where it's set.
            Some(supplement @ (Expr::Str(_) | Expr::ContentBlock(_))) => {
                let text = self.unnumbered(|reader| reader.plain_text(supplement));
                self.equation_supplement = Some(Some(text.into()));
            }
            _ => {}
        }
    }

    /// `#math.equation(…)`: an equation, displayed and counted as `$ … $`
    /// is with `block: true`, else inline and uncounted, whatever its body
    /// is written as. A body the reader can't write as math (`[x]`) shows
    /// what it shows, numbered as in text (see
    /// [`Reader::display_equation_inline`]), and the equations in it aren't
    /// counted: the equations after it are numbered as typst numbers them
    /// all the same.
    fn equation_call(&mut self, args: &Args, last: &mut Option<Labelled>) -> Item {
        // What it's given holds for it alone.
        self.rules_scoped(|reader| {
            reader.equation_settings(args);
            let block = reader.equation_block;
            match args.body() {
                Some(Expr::Equation(eq)) if block => {
                    Item::blocks(reader.display_equation(eq, last))
                }
                Some(Expr::Equation(eq)) => {
                    Item::Inlines(vec![Inline::Math(typst_math::equation(eq))])
                }
                _ => {
                    let numbered = if block {
                        reader.number_equation(last)
                    } else {
                        None
                    };
                    let content = reader.unnumbered(|reader| {
                        reader.joined(reader.std_shown_args("math.equation", args))
                    });
                    match numbered {
                        Some((index, shown)) => {
                            let mut out = Vec::new();
                            push_numbered(&mut out, index, &shown, content.into_inlines());
                            Item::blocks(vec![Block::Paragraph(out)])
                        }
                        None => content,
                    }
                }
            }
        })
    }

    /// `#set document(title:, author:, date:, description:)`.
    fn document_rule(&mut self, args: &Args) {
        if let Some(title) = args.named("title").map(|e| self.plain_text(e)) {
            self.meta.title = Some(title).filter(|t| !t.is_empty());
        }
        match args.named("author") {
            Some(Expr::Array(array)) => {
                self.meta.authors = array
                    .items()
                    .filter_map(|item| match item {
                        ast::ArrayItem::Pos(e) => Some(self.plain_text(e)),
                        ast::ArrayItem::Spread(_) => None,
                    })
                    .collect();
            }
            Some(author) => self.meta.authors = vec![self.plain_text(author)],
            None => {}
        }
        if let Some(Expr::Str(date)) = args.named("date") {
            self.meta.date = Some(date.get().to_string());
        }
        if let Some(description) = args.named("description") {
            self.meta.description = Some(self.plain_text(description));
        }
    }

    /// `read`, with display equations unnumbered: for what typst doesn't
    /// lay out, which doesn't count (see [`Reader::unlaid`]).
    fn unnumbered<T>(&mut self, read: impl FnOnce(&mut Self) -> T) -> T {
        let unlaid = std::mem::replace(&mut self.unlaid, true);
        let read = read(self);
        self.unlaid = unlaid;
        read
    }

    /// The text of a string or content argument.
    fn plain_text(&mut self, expr: Expr) -> String {
        match expr {
            Expr::Str(text) => text.get().to_string(),
            Expr::ContentBlock(block) => {
                quoted_text(&self.content_inlines(block)).trim().to_string()
            }
            _ => String::new(),
        }
    }

    /// What `call` shows (see [`Item`]); a note it makes is `last`, for a
    /// label after it.
    fn call(&mut self, call: ast::FuncCall, last: &mut Option<Labelled>) -> Item {
        self.nested(call.to_untyped(), |reader| {
            let level = reader.call_level.replace(reader.nesting);
            let item = reader.read_call(call, last);
            reader.call_level = level;
            item
        })
    }

    fn read_call(&mut self, call: ast::FuncCall, last: &mut Option<Labelled>) -> Item {
        let mut held = Vec::new();
        let (callee, args) = self.unwrapped(call, &Args::default(), &mut held);
        let name = match callee {
            Name::Function(name) => name,
            Name::Setting | Name::Value => return Item::Inlines(Vec::new()),
            other => return self.custom_call(call, &args, other),
        };
        let body = |reader: &mut Reader| match args.body() {
            Some(body) => reader.arg_item(body),
            None => Item::Inlines(Vec::new()),
        };
        match name.as_str() {
            // A block of its own, as typst lays an image out but in a box,
            // which keeps it in the line (see `Reader::boxed`).
            "image" => Item::blocks(
                image(&args)
                    .map(|image| Block::Paragraph(vec![image]))
                    .into_iter()
                    .collect(),
            ),
            "figure" => Item::blocks(self.figure(&args)),
            "link" => Item::Inlines(self.link(&args)),
            "ref" => match args.positional().next() {
                Some(Expr::Label(label)) => Item::Inlines(vec![label_ref(label.get())]),
                _ => Item::Inlines(Vec::new()),
            },
            "footnote" => Item::Inlines(vec![self.footnote(&args, last)]),
            "quote" => self.quote(&args),
            "raw" => raw(&args),
            "heading" => Item::blocks(self.heading_call(&args, last)),
            "list" | "enum" => Item::blocks(self.list(&name, &args).into_iter().collect()),
            // On its own, an item makes a list (see `Reader::flow`).
            "list.item" | "enum.item" => match self.item_call(Expr::FuncCall(call)) {
                Some(item) => self.lone_item(item),
                None => Item::Inlines(Vec::new()),
            },
            "terms" => Item::blocks(self.terms(&args)),
            "terms.item" => {
                let parts: Vec<Expr> = args.positional().collect();
                Item::blocks(vec![self.term(&parts)])
            }
            "table" | "grid" => {
                Item::blocks(self.table(&args).map(Block::Table).into_iter().collect())
            }
            "math.equation" => self.equation_call(&args, last),
            "line" => Item::blocks(vec![Block::ThematicBreak]),
            "linebreak" => Item::Inlines(vec![Inline::LineBreak]),
            // As typst parses it: past the settings it finds by their type
            // (`text(20pt, red)[…]`), its body is the argument left, content
            // or a string, wherever it is. Typst refuses more than one; none
            // is lost.
            "text" => {
                let (shown, rest): (Vec<Expr>, Vec<Expr>) = args
                    .positional()
                    .partition(|&expr| matches!(expr, Expr::Str(_)) || self.is_content(expr));
                // With none shown, its body is code the reader doesn't run;
                // with one, the rest are settings. A value is one either way
                // (`20pt`): typst takes no number for the body.
                if shown.is_empty() {
                    rest.into_iter()
                        .filter(|&expr| !matches!(Skipped::of(expr), Some(Skipped::Value)))
                        .for_each(|expr| self.skip_arg(expr));
                }
                self.joined(shown)
            }
            "emph" => body(self).styled(Inline::Emph),
            "strong" => body(self).styled(Inline::Strong),
            "strike" => body(self).styled(Inline::Strikethrough),
            "super" => body(self).styled(Inline::Superscript),
            "sub" => body(self).styled(Inline::Subscript),
            // Layout wrappers around blocks, transforms included: the
            // content of each argument, in turn.
            "block" | "par" | "align" | "pad" | "columns" | "place" | "rect" | "square"
            | "circle" | "ellipse" | "stack" | "move" | "rotate" | "scale" | "skew" | "repeat" => {
                let shown = self.std_shown_args(&name, &args);
                Item::blocks(
                    shown
                        .into_iter()
                        .flat_map(|e| self.body_blocks(e))
                        .collect(),
                )
            }
            "box" => self.boxed(&args),
            // Breaks and spacing between blocks, and what typst sets apart
            // as blocks: they end the paragraph, and show nothing.
            "v" | "pagebreak" | "colbreak" | "parbreak" | "outline" | "bibliography" => {
                Item::blocks(Vec::new())
            }
            // Spacing in a line, hidden content and generated content:
            // nothing to keep.
            "h" | "hide" | "counter" | "state" | "metadata" => Item::Inlines(Vec::new()),
            // Inline styling (`text`, `underline`, …) and the rest of the
            // standard library: the arguments typst shows.
            _ => self.joined(self.std_shown_args(&name, &args)),
        }
    }

    /// What `call`, to a function the document makes (`callee`) or one the
    /// reader can't see, shows: the arguments it's taken to show (see
    /// [`Reader::custom_shown_args`]), those a preset of it presets first.
    fn custom_call(&mut self, call: ast::FuncCall, args: &Args, callee: Name) -> Item {
        let (presets, callee) = Presets::of(callee);
        let args = &presets.give(args);
        let given = match &callee {
            Name::Defined(signature) => self.custom_shown_args(args, Some(signature)),
            // A method of a value (`names.join(", ")`): what it computes
            // the reader can't tell, and the strings it's given are what it
            // computes with, but the content it's given it shows
            // (`theme.callout[…]`).
            Name::Opaque if self.is_method(call.callee()) => {
                let mut given = self.custom_shown_args(args, None);
                given.retain(|(_, expr)| !matches!(expr, Expr::Str(_)));
                given
            }
            _ => self.custom_shown_args(args, None),
        };
        self.joined(given.into_iter().map(|(_, expr)| expr).collect())
    }

    // The calls of typst's own that make an element of their own, each
    // read by a function of its own: a call takes the stack reading its
    // kind takes, not every kind's, so calls nest `MAX_NESTING` deep on the
    // stack conversions run on.

    /// `#figure(body, caption: …)`: an image, the caption its alt text, or
    /// any other body, the caption a paragraph after it.
    fn figure(&mut self, args: &Args) -> Vec<Block> {
        let mut figure = match args.positional().next() {
            Some(Expr::FuncCall(inner))
                if self.std_name(inner.callee()).as_deref() == Some("image") =>
            {
                let mut image = image(&Args::new(inner.args()));
                // The caption describes the image: it's the alt text
                // unless empty.
                let caption = args.named("caption").map(|c| self.plain_text(c));
                if let (Some(Inline::Image { alt, .. }), Some(caption)) = (&mut image, caption)
                    && !caption.is_empty()
                {
                    *alt = caption;
                }
                let image = image.map(|i| Block::Paragraph(vec![i]));
                return image.into_iter().collect();
            }
            Some(body) => self.body_blocks(body),
            None => Vec::new(),
        };
        // Under anything else, a paragraph of its own, as the HTML
        // reader reads `<figcaption>`.
        if let Some(caption) = args.named("caption") {
            let caption = self.inlines_of(caption);
            if !caption.is_empty() {
                figure.push(Block::Paragraph(vec![Inline::Emph(caption)]));
            }
        }
        figure
    }

    /// `#link(dest)[body]`: to a URL or a label, showing its body, or a
    /// URL itself. One to anything else shows its body.
    fn link(&mut self, args: &Args) -> Vec<Inline> {
        let url = match args.positional().next() {
            Some(Expr::Str(url)) => url.get().to_string(),
            Some(Expr::Label(label)) => format!("{LABEL_LINK}{}", label.get()),
            _ => {
                return match args.body() {
                    Some(body) => self.inlines_of(body),
                    None => Vec::new(),
                };
            }
        };
        // `link(dest)[body]` puts the body second.
        let mut content = match args.positional().nth(1) {
            Some(Expr::ContentBlock(block)) => self.content_inlines(block),
            Some(Expr::Str(text)) => vec![Inline::Text(text.get().to_string())],
            _ => Vec::new(),
        };
        // An email address or a phone number shows without its scheme, as
        // typst shows it.
        if content.is_empty() && !url.starts_with(LABEL_LINK) {
            let shown = ["mailto:", "tel:"]
                .iter()
                .find_map(|scheme| url.strip_prefix(scheme))
                .unwrap_or(&url);
            content = vec![Inline::Text(shown.to_string())];
        }
        vec![Inline::Link {
            url,
            title: None,
            content,
        }]
    }

    /// `#footnote[body]`: the note's mark, its body appended to the notes;
    /// the note is `last`, for a label after it.
    fn footnote(&mut self, args: &Args, last: &mut Option<Labelled>) -> Inline {
        // `footnote(<label>)` marks an existing note again.
        if let Some(Expr::Label(label)) = args.positional().next() {
            return label_ref(label.get());
        }
        // Take the note's number before reading its body, which
        // may hold notes of its own.
        let slot = self.notes.len();
        let label = format!("note-{}", slot + 1);
        self.notes.push(Block::FootnoteDefinition {
            label: label.clone(),
            content: Vec::new(),
        });
        let content = match args.body() {
            Some(body) => self.body_blocks(body),
            None => Vec::new(),
        };
        self.notes[slot] = Block::FootnoteDefinition {
            label: label.clone(),
            content,
        };
        *last = Some(Labelled::Note(label.clone()));
        Inline::FootnoteRef(label)
    }

    /// `#quote[body]`: a block quote with its attribution, or the body in
    /// quotation marks.
    fn quote(&mut self, args: &Args) -> Item {
        if args.flag("block") {
            let mut content = match args.body() {
                Some(body) => self.body_blocks(body),
                None => Vec::new(),
            };
            // Typst sets the attribution under the quote, after a
            // dash; an inline quote shows none.
            if let Some(attribution) = args.named("attribution") {
                let attribution = self.inlines_of(attribution);
                if !attribution.is_empty() {
                    let mut line = vec![Inline::Text("— ".into())];
                    line.extend(attribution);
                    content.push(Block::Paragraph(trim(line)));
                }
            }
            Item::blocks(vec![Block::Quote(content)])
        } else {
            let mut content = vec![Inline::Text("“".into())];
            if let Some(body) = args.body() {
                content.extend(self.inlines_of(body));
            }
            content.push(Inline::Text("”".into()));
            Item::Inlines(content)
        }
    }

    /// `#heading[body]`, at the level typst gives it (see
    /// [`Reader::heading`]).
    fn heading_call(&mut self, args: &Args, last: &mut Option<Labelled>) -> Vec<Block> {
        let number = |name| match args.named(name) {
            Some(Expr::Int(n)) => usize::try_from(n.get()).ok(),
            _ => None,
        };
        // As typst numbers it: `level` if given, else `depth` past
        // `offset`.
        let level = number("level").unwrap_or_else(|| {
            let depth = number("depth").unwrap_or(1);
            depth.saturating_add(number("offset").unwrap_or(0))
        });
        let content = match args.body() {
            Some(body) => self.inlines_of(body),
            None => Vec::new(),
        };
        self.heading(level, content, last)
    }

    /// `#box(…)`: inline, as typst lays a box out, unless what it holds
    /// can't be written inline (see `Item::fits_inline`).
    fn boxed(&mut self, args: &Args) -> Item {
        match self.joined(self.std_shown_args("box", args)) {
            blocks if !blocks.fits_inline() => blocks,
            // Laid out on its own, inside the paragraph (see `Quoter`);
            // an empty one shows nothing.
            item => {
                let content = trim(item.into_inlines());
                Item::Inlines(if content.is_empty() {
                    Vec::new()
                } else {
                    vec![Inline::Link {
                        url: FRAME_MARK.to_string(),
                        title: None,
                        content,
                    }]
                })
            }
        }
    }

    /// What the arguments `exprs` show, in turn, with a space between them.
    fn joined(&mut self, exprs: Vec<Expr>) -> Item {
        exprs
            .into_iter()
            .fold(Item::Inlines(Vec::new()), |joined, expr| {
                let next = self.arg_item(expr);
                joined.then(next)
            })
    }

    /// What the argument `expr` shows: `[…]` as it's written, a call's
    /// content, a string's text, and so on.
    fn arg_item(&mut self, expr: Expr) -> Item {
        match expr {
            Expr::ContentBlock(block) => self.content_flow(block),
            Expr::FuncCall(call) => self.call(call, &mut None),
            Expr::Parenthesized(inner) => self.arg_item(inner.expr()),
            Expr::Raw(raw) if raw.block() => Item::blocks(vec![code_block(raw)]),
            Expr::Equation(eq) if eq.block() => Item::blocks(self.display_equation(eq, &mut None)),
            other => Item::Inlines(self.inlines_of(other)),
        }
    }

    /// The blocks of a content argument (see [`Reader::arg_item`]), its
    /// inline content in paragraphs.
    fn body_blocks(&mut self, expr: Expr) -> Vec<Block> {
        self.arg_item(expr).into_blocks()
    }

    /// `#list(…)` or `#enum(…)` (`name`): an item per positional argument,
    /// as typst takes them. An item of the list's kind (see
    /// [`Reader::list_child`]) is that item, its number the list's if it's
    /// the first; anything else, an item of the other kind and a list
    /// included, is an item's content.
    fn list(&mut self, name: &str, args: &Args) -> Option<Block> {
        let numbered = name == "enum";
        let mut start = match args.named("start") {
            Some(Expr::Int(n)) => u64::try_from(n.get()).ok(),
            _ => None,
        }
        .or(self.enum_start)
        .unwrap_or(1);
        let mut items = Vec::new();
        for expr in args.positional() {
            let content = match self.list_child(expr) {
                Some(item) if item.numbered == numbered => {
                    if items.is_empty()
                        && let Some(number) = item.number
                    {
                        start = number;
                    }
                    item.content
                }
                Some(other) => self.lone_item(other).into_blocks(),
                None => self.body_blocks(expr),
            };
            items.push(ListItem {
                task: None,
                content,
            });
        }
        if items.is_empty() {
            return None;
        }
        let spaced = matches!(args.named("tight"), Some(Expr::Bool(b)) if !b.get());
        let tight = !spaced && !items.iter().any(|i| loosens(&i.content));
        Some(Block::List(List {
            start: numbered.then_some(start),
            tight,
            items,
        }))
    }

    /// The list item `expr`, an argument of `#list(…)` or `#enum(…)`, is,
    /// if it's one: a call to `list.item` or `enum.item` (see
    /// [`Reader::item_call`]), or `[…]` holding that call or an item
    /// written in markup (`[- a]`, `[3. a]`), and nothing else.
    fn list_child(&mut self, expr: Expr) -> Option<ItemCall> {
        if let Some(item) = self.item_call(expr) {
            return Some(item);
        }
        let Expr::ContentBlock(block) = expr else {
            return None;
        };
        let mut exprs = block
            .body()
            .exprs()
            .filter(|expr| !matches!(expr, Expr::Space(_)));
        let (Some(only), None) = (exprs.next(), exprs.next()) else {
            return None;
        };
        self.scoped(|reader| match only {
            Expr::ListItem(item) => Some(ItemCall {
                numbered: false,
                number: None,
                content: reader.blocks(item.body()),
            }),
            Expr::EnumItem(item) => Some(ItemCall {
                numbered: true,
                number: item.number(),
                content: reader.blocks(item.body()),
            }),
            call => reader.item_call(call),
        })
    }

    /// A list of `item` alone, as typst shows an item on its own.
    fn lone_item(&self, item: ItemCall) -> Item {
        let mut flow = Flow::default();
        let start = self.item_start(&item);
        flow.push_item(start, item.content);
        flow.finish()
    }

    /// The item `expr` makes, if it calls `list.item` or `enum.item`.
    fn item_call(&mut self, expr: Expr) -> Option<ItemCall> {
        let Expr::FuncCall(call) = expr else {
            return None;
        };
        let numbered = match self.std_name(call.callee())?.as_str() {
            "list.item" => false,
            "enum.item" => true,
            _ => return None,
        };
        let args = Args::new(call.args());
        // `enum.item(number, body)` puts the number first.
        let number = match args.positional().next() {
            Some(Expr::Int(n)) if numbered && args.positional().count() > 1 => {
                u64::try_from(n.get()).ok()
            }
            _ => None,
        };
        let content = match args.body() {
            Some(body) => self.body_blocks(body),
            None => Vec::new(),
        };
        Some(ItemCall {
            numbered,
            number,
            content,
        })
    }

    /// The number a list starting with `item` starts at, as
    /// [`Flow::push_item`] takes it.
    fn item_start(&self, item: &ItemCall) -> Option<u64> {
        item.numbered
            .then(|| item.number.or(self.enum_start).unwrap_or(1))
    }

    /// `#terms(([term], [description]), terms.item[term][description], [/
    /// term: description])`.
    fn terms(&mut self, args: &Args) -> Vec<Block> {
        let mut out = Vec::new();
        for expr in args.positional() {
            match expr {
                Expr::Array(pair) => {
                    let parts: Vec<Expr> = pair
                        .items()
                        .filter_map(|item| match item {
                            ast::ArrayItem::Pos(e) => Some(e),
                            ast::ArrayItem::Spread(_) => None,
                        })
                        .collect();
                    out.push(self.term(&parts));
                }
                Expr::FuncCall(item)
                    if self.std_name(item.callee()).as_deref() == Some("terms.item") =>
                {
                    let parts: Vec<Expr> = Args::new(item.args()).positional().collect();
                    out.push(self.term(&parts));
                }
                // Content holding term items, written in markup.
                other => out.extend(self.body_blocks(other)),
            }
        }
        out
    }

    /// A term list's item from its `parts`: the term, then its description.
    fn term(&mut self, parts: &[Expr]) -> Block {
        let mut parts = parts.iter().map(|&part| self.inlines_of(part));
        let term = parts.next().unwrap_or_default();
        let description = parts.next().unwrap_or_default();
        Block::Paragraph(term_item(term, description))
    }

    /// The inline content of the argument `expr`, trimmed.
    fn inlines_of(&mut self, expr: Expr) -> Vec<Inline> {
        // `[…]`, the argument most calls are given, is read straight away,
        // without the stack `Reader::inline` takes.
        if let Expr::ContentBlock(block) = expr {
            return trim(self.content_flow(block).into_inlines());
        }
        let mut out = Vec::new();
        self.inline(&mut out, expr, &mut None);
        trim(out)
    }
}

/// `@label` or `footnote(<label>)`: a placeholder until every label is
/// known (see [`Reader::finish`]).
fn label_ref(target: &str) -> Inline {
    Inline::Link {
        url: format!("{LABEL_LINK}{target}"),
        title: None,
        content: Vec::new(),
    }
}

/// Append `content`, numbered equation `index` in text: after the anchor
/// references link to, tag by tag as Markdown gives an anchor in text, and
/// before the number it shows.
fn push_numbered(
    out: &mut Vec<Inline>,
    index: u64,
    shown: &str,
    content: impl IntoIterator<Item = Inline>,
) {
    let raw = |text: String| Inline::Raw {
        format: RawFormat::Html,
        text,
    };
    out.push(raw(format!("<a id=\"{}\">", equation_id(index))));
    out.push(raw("</a>".into()));
    out.extend(content);
    push_text(out, &format!(" {shown}"));
}

/// Whether `text`, an equation's number, goes in a `\tag*` as it is: it
/// holds none of the characters TeX gives a meaning, as a numbering
/// pattern's prefix and suffix may (`"1%"`). Escaped, they'd take the
/// renderers here to math, not text (see
/// [`crate::converter::document::native::write::renderable_tex`]), and
/// `\`, `^` and `~` are commands neither pulldown-latex nor mitex takes in a
/// tag's place.
fn fits_tag(text: &str) -> bool {
    !text.contains(['%', '#', '&', '$', '_', '{', '}', '\\', '^', '~'])
}

/// The id of the anchor before numbered equation `index` (see
/// [`Reader::display_equation`]). Its colon keeps it apart from the
/// headings' ids, which have none (see
/// [`crate::converter::document::native::write::HeadingIds`]).
fn equation_id(index: u64) -> String {
    format!("equation:{index}")
}

/// The id a heading's [`HEADING_MARK`] names, if it has one.
fn heading_mark(content: &[Inline]) -> Option<usize> {
    match content.last() {
        Some(Inline::Link { url, .. }) => url.strip_prefix(HEADING_MARK)?.parse().ok(),
        _ => None,
    }
}

/// Blocks met where only inlines can go (in emphasis, a table cell): their
/// inlines, a space between blocks. A heading there is no heading any
/// more, so it loses its mark (and labels on it reach nothing).
fn flatten(blocks: Vec<Block>) -> Vec<Inline> {
    let mut out = Vec::new();
    for block in blocks {
        let content = match block {
            Block::Paragraph(content) => content,
            Block::Heading { mut content, .. } => {
                if heading_mark(&content).is_some() {
                    content.pop();
                }
                content
            }
            Block::Quote(children) => flatten(children),
            Block::List(list) => flatten(list.items.into_iter().flat_map(|i| i.content).collect()),
            Block::Code { code, .. } => vec![Inline::Code(code)],
            // A numbered equation, as in text (see
            // `Reader::display_equation_inline`): `\tag` isn't allowed
            // inline, so its number follows it.
            Block::Math(tex) => match tex.rsplit_once(" \\tag*{") {
                Some((math, tag)) if tag.ends_with('}') => {
                    // As it is: no number with TeX's own characters goes
                    // in a tag (see `fits_tag`).
                    let shown = &tag[..tag.len() - 1];
                    vec![
                        Inline::Math(math.to_string()),
                        Inline::Text(format!(" {shown}")),
                    ]
                }
                _ => vec![Inline::Math(tex)],
            },
            // An equation's anchor, as text gives one: tag by tag.
            Block::Raw {
                format: RawFormat::Html,
                ref text,
            } if crate::converter::document::native::write::lone_anchor(text).is_some()
                && let Some(open) = text.strip_suffix("</a>") =>
            {
                let raw = |text: &str| Inline::Raw {
                    format: RawFormat::Html,
                    text: text.to_string(),
                };
                vec![raw(open), raw("</a>")]
            }
            Block::Table(table) => flatten(
                table
                    .header
                    .into_iter()
                    .chain(table.rows.into_iter().flatten())
                    .map(Block::Paragraph)
                    .collect(),
            ),
            Block::ThematicBreak | Block::FootnoteDefinition { .. } | Block::Raw { .. } => {
                Vec::new()
            }
        };
        if content.is_empty() {
            continue;
        }
        // An anchor stays right before what it marks.
        let after_anchor = matches!(out.last(), Some(Inline::Raw { text, .. }) if text == "</a>");
        if !out.is_empty() && !after_anchor {
            push_text(&mut out, " ");
        }
        out.extend(content);
    }
    out
}

/// Whether `expr` keeps the element a following `<label>` attaches to:
/// typst passes over spaces and paragraph breaks. A label is let through
/// too, to take that element itself.
fn labels_skip(expr: Expr) -> bool {
    matches!(expr, Expr::Space(_) | Expr::Parbreak(_) | Expr::Label(_))
}

/// Whether a list item holding `content` makes its list loose: when one of
/// its blocks, written on the line after the one before it, as the writers
/// join a tight item's blocks, would run into it (see [`runs_into`]). Any
/// other blocks keep the list tight, as typst draws it. Blocks no writer
/// writes there (a note's, display math showing nothing) are skipped.
fn loosens(content: &[Block]) -> bool {
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

/// A term list's item: its term in bold, then its description.
fn term_item(term: Vec<Inline>, description: Vec<Inline>) -> Vec<Inline> {
    let mut content = styled(Inline::Strong, trim(term));
    content.push(Inline::Text(": ".into()));
    content.extend(description);
    trim(content)
}

/// Function call arguments, by position and name.
#[derive(Clone, Default)]
struct Args<'a> {
    /// Each argument in turn, with its name if it's given by name. Those
    /// spread from an array the reader can tell (`..([a], [b])`; see
    /// [`array_items`]) are positional ones.
    items: Vec<(Option<&'a str>, Expr<'a>)>,
    /// How many of `items` come before the first argument spread from
    /// what the reader can't tell (`..xs`), if there's one: past it, it
    /// can't tell the places of those given by position.
    spread: Option<usize>,
    /// The arguments calls to a table's parts among `items` are given
    /// where they're written, if they're written in a wrapper's body with
    /// its parameters (`table.header(title, [B])`; see `Bound::give`),
    /// by the call.
    calls: Vec<(&'a SyntaxNode, Rc<Args<'a>>)>,
}

impl<'a> Args<'a> {
    /// `more` after these, as a call given both in turn is given them.
    fn append(&mut self, more: Args<'a>) {
        if self.spread.is_none() {
            self.spread = more.spread.map(|spread| spread + self.items.len());
        }
        self.items.extend(more.items);
        self.calls.extend(more.calls);
    }

    fn new(args: ast::Args<'a>) -> Self {
        let mut items = Vec::new();
        let mut unknown = None;
        for arg in args.items() {
            match arg {
                ast::Arg::Pos(expr) => items.push((None, expr)),
                ast::Arg::Named(named) => items.push((Some(named.name().as_str()), named.expr())),
                ast::Arg::Spread(spread) => match array_items(spread.expr()) {
                    Some(spread) => items.extend(spread.into_iter().map(|expr| (None, expr))),
                    None => {
                        unknown.get_or_insert(items.len());
                    }
                },
            }
        }
        Args {
            items,
            spread: unknown,
            calls: Vec::new(),
        }
    }

    /// The arguments `call` is given: those it's given where it's written
    /// (see [`Args::calls`]), if it's among these, else its own.
    fn of(&self, call: ast::FuncCall<'a>) -> Args<'a> {
        let node = call.to_untyped();
        match self.calls.iter().find(|(at, _)| std::ptr::eq(*at, node)) {
            Some((_, args)) => Args::clone(args),
            None => Args::new(call.args()),
        }
    }

    /// How each of `items` is given, with [`Given::Spread`] at `spread`,
    /// which no item stands for.
    fn given(&self) -> Vec<Given<'a>> {
        let mut given: Vec<Given> = self
            .items
            .iter()
            .map(|&(name, _)| name.map_or(Given::Positional, Given::Named))
            .collect();
        if let Some(spread) = self.spread {
            given.insert(spread, Given::Spread);
        }
        given
    }

    fn positional(&self) -> impl Iterator<Item = Expr<'a>> + '_ {
        self.items
            .iter()
            .filter_map(|&(name, expr)| name.is_none().then_some(expr))
    }

    /// The argument given as `name`: the last, as typst takes it (`..c,
    /// colspan: 1`).
    fn named(&self, name: &str) -> Option<Expr<'a>> {
        self.items
            .iter()
            .rev()
            .find_map(|&(given, expr)| (given == Some(name)).then_some(expr))
    }

    fn flag(&self, name: &str) -> bool {
        matches!(self.named(name), Some(Expr::Bool(b)) if b.get())
    }

    /// The content argument: the last positional one (`#f(…)[body]` puts
    /// the trailing block last).
    fn body(&self) -> Option<Expr<'a>> {
        self.positional().last()
    }
}

/// `#image(path, alt: …)`. Typst takes a path starting with `/` from the
/// project's root, which is the input's folder: as a relative URL, it's
/// taken from there too.
fn image(args: &Args) -> Option<Inline> {
    let Some(Expr::Str(path)) = args.positional().next() else {
        return None;
    };
    let alt = match args.named("alt") {
        Some(Expr::Str(alt)) => alt.get().to_string(),
        _ => String::new(),
    };
    Some(Inline::Image {
        url: path.get().trim_start_matches('/').to_string(),
        title: None,
        alt,
    })
}

/// `node` as it's written, on one line.
fn written(node: &SyntaxNode) -> Inline {
    let written = node.full_text();
    let words: Vec<&str> = written.split_whitespace().collect();
    Inline::Text(words.join(" "))
}

/// `#raw(text)`: code, a block of it if it says.
fn raw(args: &Args) -> Item {
    let code = match args.positional().next() {
        Some(Expr::Str(text)) => text.get().to_string(),
        _ => String::new(),
    };
    if args.flag("block") {
        let lang = match args.named("lang") {
            Some(Expr::Str(lang)) => Some(lang.get().to_string()),
            _ => None,
        };
        Item::blocks(vec![Block::Code { lang, code }])
    } else {
        Item::Inlines(vec![Inline::Code(code)])
    }
}

/// A raw block's code: its lines, without the fence's indentation.
fn raw_text(raw: ast::Raw) -> String {
    raw.lines()
        .map(|line| line.get().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A raw block as a code block.
fn code_block(raw: ast::Raw) -> Block {
    Block::Code {
        lang: raw.lang().map(|l| l.get().to_string()),
        code: raw_text(raw),
    }
}

/// `expr` past parentheses.
fn unparenthesized(expr: Expr) -> Expr {
    match expr {
        Expr::Parenthesized(inner) => unparenthesized(inner.expr()),
        expr => expr,
    }
}

/// Append `inline`, merged into the text before it if it's text.
fn push_inline(out: &mut Vec<Inline>, inline: Inline) {
    match inline {
        Inline::Text(text) => push_text(out, &text),
        other => out.push(other),
    }
}

/// `inlines` with adjacent text merged and their spaces collapsed, as
/// typst collapses them. Only spaces collapse: a no-break or ideographic
/// space is text, in typst markup as here.
fn tidy(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut merged: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        push_inline(&mut merged, inline);
    }
    // Runs of spaces (two around a dropped label, say) are one.
    for inline in &mut merged {
        if let Inline::Text(t) = inline {
            while t.contains("  ") {
                *t = t.replace("  ", " ");
            }
        }
    }
    // And the spaces at a break: those around a call that shows nothing
    // between two lines collapse into one, and a line break takes the
    // spaces on either side of it.
    collapse_breaks(merged)
}

/// [`tidy`] `inlines`, without the spaces and breaks at their ends.
fn trim(inlines: Vec<Inline>) -> Vec<Inline> {
    let (_, mut inlines, _) = split_blank_ends(tidy(inlines));
    inlines.retain(|i| !matches!(i, Inline::Text(t) if t.is_empty()));
    inlines
}

/// `content` in `style` (`Inline::Strong`, …), with the spaces and breaks
/// at its ends left outside, where Markdown needs them; typst shows them
/// the same either way. Styling nothing shows nothing.
fn styled(style: fn(Vec<Inline>) -> Inline, content: Vec<Inline>) -> Vec<Inline> {
    let (mut out, content, after) = split_blank_ends(tidy(content));
    if !content.is_empty() {
        match style(content) {
            // Typst's emphasis toggles: emphasized text emphasized again is
            // upright.
            Inline::Emph(mut inner) if matches!(inner.as_slice(), [Inline::Emph(_)]) => {
                if let Some(Inline::Emph(upright)) = inner.pop() {
                    out.extend(upright);
                }
            }
            styled => out.push(styled),
        }
    }
    out.extend(after);
    out
}

/// `inlines` split in three: the spaces and breaks they start with, what
/// they show, and the spaces and breaks they end with.
fn split_blank_ends(mut inlines: Vec<Inline>) -> (Vec<Inline>, Vec<Inline>, Vec<Inline>) {
    let blank = |i: &Inline| match i {
        Inline::SoftBreak | Inline::LineBreak => true,
        Inline::Text(t) => t.chars().all(|c| c == ' '),
        _ => false,
    };
    let start = inlines
        .iter()
        .position(|i| !blank(i))
        .unwrap_or(inlines.len());
    let mut before: Vec<Inline> = inlines.drain(..start).collect();
    let end = inlines.iter().rposition(|i| !blank(i)).map_or(0, |i| i + 1);
    let mut after = inlines.split_off(end);
    if let Some(Inline::Text(t)) = inlines.first_mut()
        && t.starts_with(' ')
    {
        *t = t.trim_start_matches(' ').to_string();
        push_text(&mut before, " ");
    }
    if let Some(Inline::Text(t)) = inlines.last_mut()
        && t.ends_with(' ')
    {
        t.truncate(t.trim_end_matches(' ').len());
        after.insert(0, Inline::Text(" ".into()));
    }
    (before, inlines, after)
}

/// `inlines` without the spaces next to a break: a soft break after
/// another break or before a line break, and the spaces of the texts on
/// either side.
fn collapse_breaks(inlines: Vec<Inline>) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::with_capacity(inlines.len());
    for inline in inlines {
        let after_break = matches!(out.last(), Some(Inline::SoftBreak | Inline::LineBreak));
        match inline {
            Inline::SoftBreak if after_break => {}
            Inline::SoftBreak | Inline::LineBreak => {
                if let Some(Inline::Text(t)) = out.last_mut() {
                    t.truncate(t.trim_end_matches(' ').len());
                    if t.is_empty() {
                        out.pop();
                    }
                }
                if matches!(inline, Inline::LineBreak)
                    && matches!(out.last(), Some(Inline::SoftBreak))
                {
                    out.pop();
                }
                out.push(inline);
            }
            Inline::Text(t) if after_break => {
                let t = t.trim_start_matches(' ');
                if !t.is_empty() {
                    out.push(Inline::Text(t.to_string()));
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::finish::MAX_HEADING_REFERENCES;
    use super::table::MAX_COLUMNS;
    use super::*;
    use crate::converter::document::native::ast::{Align, ListItem, Table, inline_text};
    use crate::converter::document::native::read::markdown;
    use crate::converter::document::native::write;
    use std::cell::Cell;

    // Helpers the reader's other test modules share.

    pub(super) fn text(s: &str) -> Inline {
        Inline::Text(s.into())
    }

    pub(super) fn paragraph_texts(doc: &Document) -> Vec<String> {
        doc.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(content) => Some(inline_text(content)),
                _ => None,
            })
            .collect()
    }

    /// A table's header and rows, as text.
    pub(super) type TableText = (Vec<String>, Vec<Vec<String>>);

    /// The header and rows of each table in `source`, as text.
    pub(super) fn table_texts(source: &str) -> Vec<TableText> {
        tables_of(&read(source))
    }

    /// The header and rows of each table in `doc`, as text.
    fn tables_of(doc: &Document) -> Vec<TableText> {
        let cells = |row: &[Vec<Inline>]| row.iter().map(|c| inline_text(c)).collect();
        doc.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Table(t) => {
                    Some((cells(&t.header), t.rows.iter().map(|r| cells(r)).collect()))
                }
                _ => None,
            })
            .collect()
    }

    /// One table of `rows` of text, with no header, as [`table_texts`]
    /// gives it.
    pub(super) fn one_table(rows: &[&[&str]]) -> Vec<TableText> {
        vec![(Vec::new(), rows.iter().map(|r| strings(r)).collect())]
    }

    pub(super) fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph(content)
    }

    /// The header and rows of each table in `source`, read in `dir`, as
    /// text.
    pub(super) fn tables_in(source: &str, dir: &Path) -> Vec<TableText> {
        tables_of(&read_in(source, dir))
    }

    pub(super) fn strings(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    /// Checks what each call of `cases` shows: `(call, shown)`, each call
    /// read on its own after `definitions`, so a failure names its call.
    /// A call shown as nothing makes no paragraph.
    pub(super) fn assert_calls_show(definitions: &str, cases: &[(&str, &str)]) {
        for &(call, shown) in cases {
            let doc = read(&format!("{definitions}{call}\n"));
            let expected: Vec<&str> = [shown].into_iter().filter(|s| !s.is_empty()).collect();
            assert_eq!(paragraph_texts(&doc), expected, "{call}");
        }
    }

    const SAMPLE: &str = r#"#set document(title: "Sample", author: ("Ann", "Bo"))
#set text(lang: "en", region: "gb")
#set page(margin: 2cm)
#show heading: set text(blue)
#let note(body) = text(red, body)

= Intro <intro>
Some *strong* and _emph_ text with `code`, a #link("https://e.org")[link],
and https://bare.org. "Quoted" -- see @intro and @nowhere.

== Lists
- one
- two
  - nested

+ first
+ second

Between.

5. five
6. six

$ sum_(i=1)^n i = (n(n+1))/2 $

Inline $x^2$ math.#footnote[A note.] <fn-a> Again#footnote(<fn-a>) and @fn-a.

```rust
fn main() {}
```

#figure(image("pic.png"), caption: [A picture])

#quote(block: true)[Quoted block.]

#table(
  columns: 2,
  align: (left, right),
  table.header([*A*], [B]),
  [1], [2],
  [3], [4],
)

#note[Kept text.] #strike[gone] H#sub[2]O #line(length: 100%)
"#;

    #[test]
    fn reads_a_typical_document() {
        let doc = read(SAMPLE);
        assert_eq!(doc.meta.title.as_deref(), Some("Sample"));
        assert_eq!(doc.meta.authors, ["Ann", "Bo"]);
        assert_eq!(doc.meta.lang.as_deref(), Some("en-GB"));
        let md = write::markdown::write(&doc);
        for expected in [
            "# Intro\n",
            "Some **strong** and *emph* text with `code`, a [link](https://e.org),",
            "and <https://bare.org>. “Quoted” – see [Intro](#intro) and @nowhere.",
            "## Lists",
            "- one\n- two\n  - nested",
            "1. first\n2. second",
            "5. five\n6. six",
            "$$\n\\sum_{i=1}^{n} i = \\frac{n\\left( n+1 \\right)}{2}\n$$",
            "Inline $x^{2}$ math.[^1] Again[^1] and [^1].",
            "```rust\nfn main() {}\n```",
            "![A picture](pic.png)",
            "> Quoted block.",
            "| **A** | B |\n| :-- | --: |\n| 1 | 2 |\n| 3 | 4 |",
            "Kept text. ~~gone~~ H~2~O",
            "[^1]: A note.",
        ] {
            assert!(md.contains(expected), "missing {expected:?} in\n{md}");
        }
        assert!(
            md.contains("\n---\n") || md.contains("\n***\n"),
            "rule: {md}"
        );
        assert!(
            !md.contains("margin") && !md.contains("blue"),
            "styling dropped: {md}"
        );
    }

    #[test]
    fn lists_end_at_other_content_and_blank_lines_make_them_loose() {
        let doc = read("- a\n\n- b\n\nText\n\n- c\n");
        assert!(matches!(&doc.blocks[0], Block::List(l) if !l.tight && l.items.len() == 2));
        assert!(matches!(&doc.blocks[1], Block::Paragraph(_)));
        assert!(matches!(&doc.blocks[2], Block::List(l) if l.items.len() == 1));
    }

    fn note(label: &str, content: Vec<Inline>) -> Block {
        Block::FootnoteDefinition {
            label: label.into(),
            content: vec![Block::Paragraph(content)],
        }
    }

    fn note_ref(label: &str) -> Inline {
        Inline::FootnoteRef(label.into())
    }

    #[test]
    fn nested_footnotes_get_their_own_labels() {
        // Typst numbers notes in reading order: the outer note, the note
        // inside it, then the next one.
        let doc = read("A#footnote[Outer #footnote[Inner]] B#footnote[Third].\n");
        assert_eq!(
            doc.blocks,
            [
                Block::Paragraph(vec![
                    text("A"),
                    note_ref("note-1"),
                    text(" B"),
                    note_ref("note-3"),
                    text("."),
                ]),
                note("note-1", vec![text("Outer "), note_ref("note-2")]),
                note("note-2", vec![text("Inner")]),
                note("note-3", vec![text("Third")]),
            ]
        );
    }

    /// A long numbering prefix, repeated for every equation, is copied
    /// within what the reader may copy; past it, equations show their bare
    /// number.
    #[test]
    fn long_equation_numbers_are_copied_within_a_bound() {
        let prefix = "-".repeat(100_000);
        let source = format!(
            "#set math.equation(numbering: \"{prefix}1\")\n{}",
            "$ x $\n\n".repeat(100)
        );
        let doc = read(&source);
        let maths: Vec<&String> = doc
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Math(tex) => Some(tex),
                _ => None,
            })
            .collect();
        assert!(maths[0].ends_with(&format!("{prefix}1}}")));
        assert_eq!(maths[99], "x \\tag*{100}");
        let budget = MAX_REFERENCE_TEXT.max(source.len() * REFERENCE_TEXT_PER_BYTE);
        let copied: usize = maths.iter().map(|tex| tex.len()).sum();
        assert!(copied < budget + prefix.len(), "{copied}");
        // Past the bound, numbers aren't made only to be dropped.
        let with_prefix = maths.iter().filter(|tex| tex.contains(&prefix)).count();
        assert!(with_prefix < 100);
        assert_eq!(NUMBERS_APPLIED.with(Cell::get), with_prefix);
    }

    /// Numbers longer than their digits (`*` repeats its symbols) stop
    /// being made once one doesn't fit.
    #[test]
    fn numbers_longer_than_estimated_spend_the_rest() {
        // The 4000th is over 1000 symbols long; all of them, about 3 MB.
        let source = format!(
            "#set math.equation(numbering: \"*\")\n{}",
            "$ x $\n\n".repeat(4000)
        );
        let doc = read(&source);
        let numbered = doc
            .blocks
            .iter()
            .filter(|block| match block {
                Block::Math(tex) => tex.split_once("\\tag*{").is_some_and(|(_, tag)| {
                    !tag.trim_end_matches('}')
                        .bytes()
                        .all(|b| b.is_ascii_digit())
                }),
                _ => false,
            })
            .count();
        assert!((1..4000).contains(&numbered), "{numbered}");
        // The number that didn't fit is the last one made.
        assert_eq!(NUMBERS_APPLIED.with(Cell::get), numbered + 1);
    }

    /// Equations share the supplement set for them rather than each copy
    /// it, so a long one on many labelled equations isn't copied for each.
    #[test]
    fn labelled_equations_share_their_supplement() {
        let source = format!(
            "#set math.equation(numbering: \"(1)\", supplement: [{}])\n{}",
            "word ".repeat(1000),
            (0..3)
                .map(|i| format!("$ x $ <e{i}>\n\n"))
                .collect::<String>()
        );
        let mut reader = Reader::default();
        let root = typst::syntax::parse(&source);
        reader.blocks(root.cast::<ast::Markup>().unwrap());
        let supplement = |label: &str| match &reader.equation_labels[label].supplement {
            Some(Some(supplement)) => Rc::clone(supplement),
            other => panic!("{label}: {:?}", other.is_some()),
        };
        assert!(Rc::ptr_eq(&supplement("e0"), &supplement("e2")));
        assert!(supplement("e0").starts_with("word word "));
    }

    #[test]
    fn labels_attach_to_the_nearest_element() {
        // The label is on the text before it, not the note: `@eq` is no
        // note reference.
        let md = write::markdown::write(&read("A#footnote[N] more words <eq> and then @eq.\n"));
        assert!(md.contains("more words and then @eq."), "{md}");
        // Spaces and paragraph breaks are skipped.
        let md = write::markdown::write(&read("A#footnote[N]\n\n<eq> and then @eq.\n"));
        assert!(md.contains("and then [^1]."), "{md}");
    }

    #[test]
    fn line_breaks_and_dropped_calls_keep_the_paragraph() {
        // As typst renders them: the space after `\` is part of the break,
        // and the spaces around a call that shows nothing collapse.
        assert_eq!(
            read("Line one \\\nLine two\n").blocks,
            [Block::Paragraph(vec![
                text("Line one"),
                Inline::LineBreak,
                text("Line two"),
            ])]
        );
        assert_eq!(
            read("Para\n#h(1fr)\nstill para\n").blocks,
            [Block::Paragraph(vec![
                text("Para"),
                Inline::SoftBreak,
                text("still para"),
            ])]
        );
        assert_eq!(
            read("a #h(0pt) b \\ c\n").blocks,
            [Block::Paragraph(vec![
                text("a b"),
                Inline::LineBreak,
                text("c")
            ])]
        );
    }

    #[test]
    fn figure_captions_follow_any_body() {
        let caption = |s| Block::Paragraph(vec![Inline::Emph(vec![text(s)])]);
        assert_eq!(
            read(
                "#figure(table(columns: 2, [a], [b]), caption: [Table _cap_])\n\n\
                 #figure(caption: [Block cap])[Body text]\n\n\
                 #figure(image(\"pic.png\"), caption: [Alt])\n"
            )
            .blocks,
            [
                Block::Table(Table {
                    align: vec![Align::Default; 2],
                    header: Vec::new(),
                    rows: vec![vec![vec![text("a")], vec![text("b")]]],
                }),
                Block::Paragraph(vec![Inline::Emph(vec![
                    text("Table "),
                    Inline::Emph(vec![text("cap")]),
                ])]),
                Block::Paragraph(vec![text("Body text")]),
                caption("Block cap"),
                // An image's caption is its alt text.
                Block::Paragraph(vec![Inline::Image {
                    url: "pic.png".into(),
                    title: None,
                    alt: "Alt".into(),
                }]),
            ]
        );
    }

    #[test]
    fn block_content_in_inline_calls_is_kept() {
        let table = |cell: &str| {
            Block::Table(Table {
                align: vec![Align::Default],
                header: Vec::new(),
                rows: vec![vec![vec![text(cell)]]],
            })
        };
        assert_eq!(
            read(
                "Para. #text(size: 9pt)[#table(columns: 1, [t1])] \
                 #custom[#table(columns: 1, [c1])] #box[#table(columns: 1, [b1])] \
                 #scale(80%)[#table(columns: 1, [s1])] end.\n"
            )
            .blocks,
            [
                Block::Paragraph(vec![text("Para.")]),
                table("t1"),
                table("c1"),
                table("b1"),
                table("s1"),
                Block::Paragraph(vec![text("end.")]),
            ]
        );
        // Inline content stays in its paragraph; blocks where only inlines
        // can go keep their text.
        assert_eq!(
            read("x #text(red)[word] *#text[#table(columns: 1, [z])]* y\n").blocks,
            [Block::Paragraph(vec![
                text("x word "),
                Inline::Strong(vec![text("z")]),
                text(" y"),
            ])]
        );
    }

    #[test]
    fn function_forms_keep_every_argument() {
        let item = |s: &str| ListItem {
            task: None,
            content: vec![Block::Paragraph(vec![text(s)])],
        };
        let doc = read(
            "#list([one], [two])\n\n#enum(start: 3, [e1], enum.item[e2])\n\n\
             #terms(([a], [b]), terms.item[c][d])\n\n#stack([s1], [s2])\n\n\
             #custom([Title], \"and\", [Body])\n\n#heading(level: 2)[Sub] <sub>\n\n\
             See @sub and #ref(<sub>).\n",
        );
        assert_eq!(
            doc.blocks[..6],
            [
                Block::List(List {
                    start: None,
                    tight: true,
                    items: vec![item("one"), item("two")],
                }),
                Block::List(List {
                    start: Some(3),
                    tight: true,
                    items: vec![item("e1"), item("e2")],
                }),
                Block::Paragraph(vec![Inline::Strong(vec![text("a")]), text(": b")]),
                Block::Paragraph(vec![Inline::Strong(vec![text("c")]), text(": d")]),
                Block::Paragraph(vec![text("s1")]),
                Block::Paragraph(vec![text("s2")]),
            ]
        );
        let md = write::markdown::write(&doc);
        assert!(md.contains("\n\nTitle and Body\n\n## Sub\n"), "{md}");
        assert!(md.contains("See [Sub](#sub) and [Sub](#sub)."), "{md}");
    }

    #[test]
    fn block_quotes_keep_their_attribution() {
        // As typst renders them: under a block quote, dropped from an
        // inline one.
        assert_eq!(
            read(
                "#quote(block: true, attribution: [The CFO])[Money.]\n\n\
                 A #quote(attribution: [Ann])[inline] quote.\n"
            )
            .blocks,
            [
                Block::Quote(vec![
                    Block::Paragraph(vec![text("Money.")]),
                    Block::Paragraph(vec![text("— The CFO")]),
                ]),
                Block::Paragraph(vec![text("A “inline” quote.")]),
            ]
        );
    }

    #[test]
    fn items_holding_several_blocks_make_the_list_loose() {
        let doc = read("- a\n- b\n\n  continued b\n");
        assert!(
            matches!(&doc.blocks[0], Block::List(l) if !l.tight),
            "{doc:?}"
        );
        let md = write::markdown::write(&doc);
        assert!(md.contains("- b\n\n  continued b"), "{md}");
        // A nested list is no second paragraph.
        let doc = read("- a\n- b\n  - nested\n");
        assert!(
            matches!(&doc.blocks[0], Block::List(l) if l.tight),
            "{doc:?}"
        );
    }

    #[test]
    fn block_breaks_end_the_paragraph_and_only_spaces_collapse() {
        // As typst renders them: spacing and breaks between blocks end the
        // paragraph, inline or on a line of their own.
        let split = [para(vec![text("a")]), para(vec![text("b")])];
        for call in [
            "#v(1em)",
            "#parbreak()",
            "#pagebreak()",
            "#pagebreak(weak: true)",
            "#colbreak()",
        ] {
            assert_eq!(read(&format!("a\n{call}\nb\n")).blocks, split, "{call}");
            assert_eq!(read(&format!("a {call} b\n")).blocks, split, "{call}");
        }
        // Typst collapses spaces only: no-break and ideographic spaces stay.
        assert_eq!(
            read("nbsp \\ ~indent\n").blocks,
            [para(vec![
                text("nbsp"),
                Inline::LineBreak,
                text("\u{a0}indent")
            ])]
        );
        assert_eq!(
            read("Word~\nnext\n").blocks,
            [para(vec![
                text("Word\u{a0}"),
                Inline::SoftBreak,
                text("next")
            ])]
        );
        assert_eq!(
            read("　　第一句\n　　第二句\n").blocks,
            [para(vec![
                text("　　第一句"),
                Inline::SoftBreak,
                text("　　第二句")
            ])]
        );
        assert_eq!(
            read("中文　\n中文\n").blocks,
            [para(vec![text("中文　"), Inline::SoftBreak, text("中文")])]
        );
        // A break at the end of styled text breaks the line after it.
        assert_eq!(
            read("*bold \\ *\nnext\n").blocks,
            [para(vec![
                Inline::Strong(vec![text("bold")]),
                Inline::LineBreak,
                text("next"),
            ])]
        );
        // And styled text or a box that shows nothing leaves the line whole.
        for call in [
            "#box[]",
            "#box(width: 1em)",
            "#box[#h(1em)]",
            "#emph[]",
            "*#h(1fr)*",
            "#super[]",
        ] {
            assert_eq!(
                read(&format!("a\n{call}\nb\n")).blocks,
                [para(vec![text("a"), Inline::SoftBreak, text("b")])],
                "{call}"
            );
        }
        assert_eq!(
            read("a \\\n#box[]\nb\n").blocks,
            [para(vec![text("a"), Inline::LineBreak, text("b")])]
        );
    }

    #[test]
    fn content_given_to_calls_continues_the_paragraph_around_them() {
        // As typst lays it out: the content `#text[…]` and the like show
        // breaks the paragraph around the call only where it breaks.
        assert_eq!(
            read("Para. #text(size: 9pt)[lead #table(columns: 2, [t1], [t2]) trail] end.\n").blocks,
            [
                para(vec![text("Para. lead")]),
                Block::Table(Table {
                    align: vec![Align::Default; 2],
                    header: Vec::new(),
                    rows: vec![vec![vec![text("t1")], vec![text("t2")]]],
                }),
                para(vec![text("trail end.")]),
            ]
        );
        assert_eq!(
            read("Para. #text(red)[x\n\ny] end.\n").blocks,
            [para(vec![text("Para. x")]), para(vec![text("y end.")])]
        );
        let item = |s: &str| ListItem {
            task: None,
            content: vec![para(vec![text(s)])],
        };
        assert_eq!(
            read("See #custom[this:\n- a\n- b]\n").blocks,
            [
                para(vec![text("See this:")]),
                Block::List(List {
                    start: None,
                    tight: true,
                    items: vec![item("a"), item("b")],
                }),
            ]
        );
        // Styles carry over to each paragraph.
        assert_eq!(
            read("#strong[P1\n\nP2] and _more_\n").blocks,
            [
                para(vec![Inline::Strong(vec![text("P1")])]),
                para(vec![
                    Inline::Strong(vec![text("P2")]),
                    text(" and "),
                    Inline::Emph(vec![text("more")]),
                ]),
            ]
        );
    }

    #[test]
    fn function_forms_follow_typst_for_every_child() {
        let item = |s: &str| ListItem {
            task: None,
            content: vec![para(vec![text(s)])],
        };
        let list = |start, items| {
            Block::List(List {
                start,
                tight: true,
                items,
            })
        };
        let term =
            |t: &str, d: &str| para(vec![Inline::Strong(vec![text(t)]), text(&format!(": {d}"))]);
        // A child written as a list item in markup is that item.
        assert_eq!(
            read("#list([- a], [- b])\n\n#enum([+ c], [d])\n").blocks,
            [
                list(None, vec![item("a"), item("b")]),
                list(Some(1), vec![item("c"), item("d")]),
            ]
        );
        // An item's number is the number typst shows.
        assert_eq!(
            read("#enum(enum.item(5)[five], [six])\n\n#enum(start: 3, enum.item(7)[seven])\n")
                .blocks,
            [
                list(Some(5), vec![item("five"), item("six")]),
                list(Some(7), vec![item("seven")]),
            ]
        );
        // Term lists take term items written in markup.
        assert_eq!(
            read("#terms([/ Term: Desc])\n\n#terms[/ a: b][/ c: d]\n").blocks,
            [term("Term", "Desc"), term("a", "b"), term("c", "d")]
        );
        // Items on their own are lists, those next to each other one list.
        assert_eq!(
            read("#list.item[x]\n#list.item[y]\n\n#enum.item(3)[z]\n\n#terms.item[T][D]\n").blocks,
            [
                list(None, vec![item("x"), item("y")]),
                list(Some(3), vec![item("z")]),
                term("T", "D"),
            ]
        );
        // A heading's level counts its offset, and an empty heading shows
        // nothing.
        assert_eq!(
            read("#heading(offset: 1)[x]\n\n#heading[]\n\n=\n").blocks,
            [Block::Heading {
                level: 2,
                content: vec![text("x")],
            }]
        );
    }

    #[test]
    fn only_items_holding_paragraphs_that_would_run_together_loosen_the_list() {
        let tight = |source: &str| match read(source).blocks.as_slice() {
            [Block::List(list), ..] => list.tight,
            other => panic!("{other:?}"),
        };
        for source in [
            "- Install:\n  ```sh\n  cargo install x\n  ```\n- Run:\n  ```sh\n  x\n  ```\n",
            "- a\n  $ x^2 $\n- b\n",
            "- a\n  #quote(block: true)[q]\n- b\n",
            "- a\n  = H\n- b\n",
            "- a\n  #table(columns: 1, [x])\n- b\n",
            "- ```\n  code\n  ```\n  after\n- b\n",
        ] {
            assert!(tight(source), "{source}");
        }
        for source in [
            "- a\n- b\n\n  continued b\n",
            "- a\n  #line()\n- b\n",
            "- a\n  #table(columns: 1, [x])\n  after\n- b\n",
            "- a\n  #quote(block: true)[q]\n  after\n- b\n",
        ] {
            assert!(!tight(source), "{source}");
        }
    }

    #[test]
    fn boxes_stay_in_their_paragraph() {
        // As typst lays a box out: inline, whatever it holds, so blocks in
        // it that have an inline form (a paragraph, a rule, spacing) keep
        // the paragraph around it whole.
        for (source, expected) in [
            (
                "Sign on the dotted line: #box(width: 1fr, repeat[.])\n",
                "Sign on the dotted line: .",
            ),
            (
                "Total #box(width: 1fr, line(length: 100%)) 42\n",
                "Total 42",
            ),
            (
                "An icon #box(height: 1em, align(horizon)[x]) inline, a pair \
                 #box(stack(dir: ltr, [a], [b])) and #box(rotate(90deg)[r]) text.\n",
                "An icon x inline, a pair a b and r text.",
            ),
            (
                "a #box[#v(1em)] b #box(height: 2em)[#align(bottom)[low]] c\n",
                "a b low c",
            ),
            (
                "Raised #box(move(dy: -2pt)[up]) word, #box[two\n\nparts] and #box[$ x $].\n",
                "Raised up word, two parts and x.",
            ),
        ] {
            assert_eq!(paragraph_texts(&read(source)), [expected], "{source}");
        }
        let item = |s: &str| ListItem {
            task: None,
            content: vec![para(vec![text(s)])],
        };
        assert_eq!(
            read("- Name: #box(width: 3cm, repeat[\\_])\n- Date:\n").blocks,
            [Block::List(List {
                start: None,
                tight: true,
                items: vec![item("Name: _"), item("Date:")],
            })]
        );
        assert_eq!(
            read("A#footnote[Sign: #box(width: 1fr, repeat[.])]\n").blocks[1..],
            [note("note-1", vec![text("Sign: .")])]
        );
    }

    #[test]
    fn blocks_of_every_kind_in_inline_calls_are_kept() {
        let doc = read(
            "A #text(fill: red)[#quote(block: true)[Q]] B #custom[```rust\nfn x() {}\n```] \
             C #box[#figure(table(columns: 1, [f]), caption: [Cap])] D\n",
        );
        assert_eq!(
            doc.blocks,
            [
                para(vec![text("A")]),
                Block::Quote(vec![para(vec![text("Q")])]),
                para(vec![text("B")]),
                Block::Code {
                    lang: Some("rust".into()),
                    code: "fn x() {}".into(),
                },
                para(vec![text("C")]),
                Block::Table(Table {
                    align: vec![Align::Default],
                    header: Vec::new(),
                    rows: vec![vec![vec![text("f")]]],
                }),
                para(vec![Inline::Emph(vec![text("Cap")])]),
                para(vec![text("D")]),
            ]
        );
        // Emphasis toggles, as typst's does: emphasized again, the
        // caption is upright.
        assert_eq!(
            read("#emph[#figure(table(columns: 1, [f]), caption: [Cap])] #emph[#emph[x]]\n").blocks
                [1..],
            [para(vec![text("Cap")]), para(vec![text("x")])]
        );
    }

    #[test]
    fn text_takes_its_body_wherever_it_is() {
        // As typst parses `text`: the settings it finds by their type put
        // aside, its body is the argument left, wherever it is. Typst
        // refuses a second one, which is kept all the same.
        assert_eq!(
            paragraph_texts(&read(
                "#text(\"hi\", red) #text([body first], 14pt) #text(20pt, rgb(\"#00f\"))[big] \
                 #text(\"Arial\")[both]\n"
            )),
            ["hi body first big Arial both"]
        );
    }

    #[test]
    fn list_children_are_items_as_typst_takes_them() {
        let item = |s: &str| ListItem {
            task: None,
            content: vec![para(vec![text(s)])],
        };
        let list = |start, items| {
            Block::List(List {
                start,
                tight: true,
                items,
            })
        };
        let nested = |start, inner: &str, outer: &str| {
            list(
                None,
                vec![
                    ListItem {
                        task: None,
                        content: vec![list(start, vec![item(inner)])],
                    },
                    item(outer),
                ],
            )
        };
        // A lone item in markup keeps its number, and an item of the other
        // kind, or a list, is content: a list of its own.
        assert_eq!(
            read(
                "#enum([3. a], [b])\n\n#enum(start: 3, [+ c], [d])\n\n\
                 #list(enum.item[e], [f])\n\n#list(list([g]), [h])\n\n\
                 #list(..([s1], [s2]))\n"
            )
            .blocks,
            [
                list(Some(3), vec![item("a"), item("b")]),
                list(Some(3), vec![item("c"), item("d")]),
                nested(Some(1), "e", "f"),
                nested(None, "g", "h"),
                list(None, vec![item("s1"), item("s2")]),
            ]
        );
        // Content given by name to the document's own functions shows.
        assert_eq!(
            paragraph_texts(&read("#custom(title: [T], width: 2pt)[Body]\n")),
            ["T Body"]
        );
    }

    /// Blocks of each kind a list item holds, by name.
    fn sample_blocks() -> Vec<(&'static str, Block)> {
        let list = |start, content: Vec<Block>| {
            Block::List(List {
                start,
                tight: true,
                items: vec![ListItem {
                    task: None,
                    content,
                }],
            })
        };
        let table = |header: Vec<Vec<Inline>>| {
            Block::Table(Table {
                align: vec![Align::Default],
                header,
                rows: vec![vec![vec![text("t")]]],
            })
        };
        let p = |s: &str| para(vec![text(s)]);
        let code = |s: &str| Block::Code {
            lang: None,
            code: s.into(),
        };
        vec![
            ("paragraph", p("p")),
            ("code", code("c")),
            (
                "heading",
                Block::Heading {
                    level: 2,
                    content: vec![text("h")],
                },
            ),
            ("quote", Block::Quote(vec![p("q")])),
            ("quote of code", Block::Quote(vec![code("qc")])),
            ("list", list(None, vec![p("l")])),
            ("list from 1", list(Some(1), vec![p("l")])),
            ("list from 3", list(Some(3), vec![p("l")])),
            ("empty item", list(None, Vec::new())),
            ("list of code", list(None, vec![code("lc")])),
            (
                "nested list",
                list(None, vec![p("l"), list(None, vec![p("n")])]),
            ),
            ("list of a table", list(None, vec![table(Vec::new())])),
            ("quote of a table", Block::Quote(vec![table(Vec::new())])),
            ("quote of math", Block::Quote(vec![Block::Math("y".into())])),
            ("table", table(Vec::new())),
            ("headed table", table(vec![vec![text("th")]])),
            ("math", Block::Math("x".into())),
            ("rule", Block::ThematicBreak),
        ]
    }

    #[test]
    fn tight_items_stay_tight_only_where_their_blocks_stay_apart() {
        // Markdown is the strictest writer: a tight item's blocks, a line
        // apart there, read back as they would a blank line apart exactly
        // when they don't loosen the list.
        let read_back = |content: &[Block], tight: bool| {
            let item = |content: Vec<Block>| ListItem {
                task: None,
                content,
            };
            let doc = Document {
                meta: Meta::default(),
                blocks: vec![Block::List(List {
                    start: None,
                    tight,
                    items: vec![item(content.to_vec()), item(vec![para(vec![text("z")])])],
                })],
            };
            let mut blocks = markdown::read(&write::markdown::write(&doc)).blocks;
            walk::for_each_block(&mut blocks, &mut |block| {
                if let Block::List(list) = block {
                    list.tight = true;
                }
            });
            blocks
        };
        for (first, a) in sample_blocks() {
            for (second, b) in sample_blocks() {
                let pair = [a.clone(), b.clone()];
                assert_eq!(
                    loosens(&pair),
                    read_back(&pair, true) != read_back(&pair, false),
                    "{first} then {second}"
                );
            }
        }
    }

    #[test]
    fn items_given_as_calls_loosen_their_list_too() {
        let tight = |source: &str| match read(source).blocks.as_slice() {
            [Block::List(list), ..] => list.tight,
            other => panic!("{other:?}"),
        };
        for source in [
            "#list([a], [b\n\nc])\n",
            "#enum(tight: true)[a][b\n\ncontinued b]\n",
            "#list.item[a]\n#list.item[b\n\nc]\n",
            "- #quote(block: true)[q]\n  #quote(block: true)[r]\n- b\n",
            "- #table(columns: 1, [x])\n  #table(columns: 1, [y])\n- b\n",
            "- #quote(block: true)[q]\n  #table(columns: 1, [x])\n- b\n",
            "- a\n  - n\n  #table(columns: 1, [x])\n- b\n",
            "- a\n  3. x\n  4. y\n- b\n",
            "- a\n  -\n- b\n",
        ] {
            assert!(!tight(source), "{source}");
        }
        for source in ["#list([a], [b])\n", "#list([a], [b\n- n])\n"] {
            assert!(tight(source), "{source}");
        }
    }

    #[test]
    fn elements_read_as_typst_lays_them_out() {
        // Hidden content shows nothing, and shapes, paragraphs and images
        // are blocks of their own, but for an image in a box.
        let doc = read(
            "A #hide[secret] B\n\nC #circle[circled] #par[a par] D\n\n\
             E #image(\"/img/pic.png\") F #box(image(\"pic.png\")) G\n",
        );
        let image = |url: &str| Inline::Image {
            url: url.to_string(),
            title: None,
            alt: String::new(),
        };
        assert_eq!(
            doc.blocks,
            [
                para(vec![text("A B")]),
                para(vec![text("C")]),
                para(vec![text("circled")]),
                para(vec![text("a par")]),
                para(vec![text("D")]),
                para(vec![text("E")]),
                // From the project's root, the input's folder.
                para(vec![image("img/pic.png")]),
                para(vec![text("F "), image("pic.png"), text(" G")]),
            ]
        );
        // Symbols show their text, and links to an email address or a
        // phone number show it without the scheme.
        let doc = read(
            "#sym.arrow.r.double #emoji.rocket #link(\"mailto:me@x.org\") \
             #link(\"tel:+123\")\n",
        );
        assert_calls_show(
            "",
            &[
                ("#text(sym.arrow)", "→"),
                ("#underline(sym.arrow)", "→"),
                ("#text(red, sym.star.filled)", "★"),
            ],
        );
        let link = |url: &str, shown: &str| Inline::Link {
            url: url.to_string(),
            title: None,
            content: vec![text(shown)],
        };
        assert_eq!(
            doc.blocks,
            [para(vec![
                text("⇒ 🚀 "),
                link("mailto:me@x.org", "me@x.org"),
                text(" "),
                link("tel:+123", "+123"),
            ])]
        );
    }

    #[test]
    fn deep_nesting_stays_within_the_stack() {
        // Typst's parser nests 256 deep, and conversions run on threads
        // with 2 MiB stacks: past what typst shows, the rest is kept as
        // it's written.
        for shape in [
            "custom(",
            "list(",
            "quote(block: true)[",
            "strong[",
            "footnote[",
            "text[",
            "table(columns: 1)[",
        ] {
            let source = match shape.strip_suffix('[') {
                Some(_) => format!("{}x{}", format!("#{shape}").repeat(255), "]".repeat(255)),
                None => format!("#{}[x]{}", shape.repeat(255), ")".repeat(255)),
            };
            let md = std::thread::Builder::new()
                .stack_size(2 << 20)
                .spawn(move || write::markdown::write(&read(&source)))
                .unwrap()
                .join()
                .unwrap();
            assert!(md.contains('x'), "{shape}: {md}");
        }
    }

    #[test]
    fn long_chains_stay_within_the_stack() {
        // Typst parses a chain of operators of any length, as long as
        // freeing the syntax tree it makes, which nests as deep as the
        // chain is long, fits the stack. A chain of headings each referring
        // to the next has no nesting at all.
        let columns = vec!["(auto,)"; 4000].join(" + ");
        let blocks = read_on_worker_stack(format!("#table(columns: {columns}, [a], [b])\n"));
        let [Block::Table(table)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(table.align.len(), MAX_COLUMNS);
        let blocks = read_on_worker_stack(format!(
            "#table(columns: (auto,) {}, [a], [b])\n",
            "* 1 ".repeat(4000)
        ));
        assert!(matches!(blocks.as_slice(), [Block::Table(_)]), "{blocks:?}");

        // Past `MAX_HEADING_REFERENCES`, a reference shows its label, so
        // no heading's text grows with the chain.
        let chain: String = (0..5000)
            .map(|i| format!("= H{i} @h{} <h{i}>\n\n", i + 1))
            .collect();
        let blocks = read_on_worker_stack(format!("#set heading(numbering: \"1.\")\n{chain}"));
        let headings: Vec<String> = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Heading { content, .. } => Some(inline_text(content)),
                _ => None,
            })
            .collect();
        assert_eq!(headings.len(), 5000);
        let longest = headings.iter().map(String::len).max().unwrap();
        assert!(longest < 40 * MAX_HEADING_REFERENCES, "{longest}");
        assert!(headings[0].starts_with("H0 H1 H2 "), "{}", headings[0]);
        assert!(headings[4999].ends_with("@h5000"), "{}", headings[4999]);

        // A value made of the one before it, again and again, nests no
        // deeper than `MAX_NESTING`, so freeing it fits the stack too.
        for line in [
            "#let g = g.with()\n",
            "#let d = (k: d)\n",
            "#let a = (a,)\n",
        ] {
            let blocks = read_on_worker_stack(format!("{}#g[Text.]\n", line.repeat(20000)));
            assert_eq!(blocks, [para(vec![text("Text.")])], "{line}");
        }
    }

    /// The blocks of `source`, read on a thread with the 2 MiB stack
    /// conversions run on.
    fn read_on_worker_stack(source: String) -> Vec<Block> {
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || read(&source).blocks)
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn calls_nest_one_level_each() {
        // Typst lays `#text[…]` out as deep as it parses it, 126 deep, and
        // `#box[…]` 70 deep: the content a call is given is no level of its
        // own.
        let table = Block::Table(Table {
            align: vec![Align::Default],
            header: Vec::new(),
            rows: vec![vec![vec![text("t")]]],
        });
        let nest = |shape: &str, depth, inside: &str| {
            format!(
                "A {}{inside}{} B\n",
                format!("#{shape}[").repeat(depth),
                "]".repeat(depth)
            )
        };
        for depth in [64, 100, 126] {
            assert_eq!(
                read_on_worker_stack(nest("text", depth, "#table(columns: 1, [t])")),
                [para(vec![text("A")]), table.clone(), para(vec![text("B")])],
                "{depth} deep"
            );
        }
        assert_eq!(
            read_on_worker_stack(nest("box", 70, "x")),
            [para(vec![text("A x B")])]
        );
    }
}
