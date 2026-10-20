//! Typst markup → [`Document`], for documents written as text: headings,
//! paragraphs and line breaks, lists and term lists (as markup or calls),
//! emphasis, code, math (converted to TeX), links, labels and references,
//! footnotes, images, figures and their captions, quotes and their
//! attributions, and tables. The document's `#set document(…)` and `#set
//! text(lang: …)` become its metadata, and `#set enum(start: …)` numbers
//! its lists.
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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use typst::foundations::{
    Array, CastInfo, Content, Decimal, Func, NativeParamInfo, Str, Symbol, Type, Value,
};
use typst::syntax::ast::{self, AstNode, Expr};
use typst::syntax::{RootedPath, SyntaxNode};

use super::markdown::push_text;
use super::{typst_math, walk};
use crate::converter::document::native::ast::{
    Align, Block, Document, Inline, List, ListItem, Meta, Table, inline_text,
};
use crate::converter::document::native::typst as native_typst;
use crate::converter::document::native::write::{self, DocumentIds, Footnotes};

/// [`read_in`] for a document with no folder: files it imports aren't read.
#[cfg(test)]
pub fn read(source: &str) -> Document {
    Reader::default().read(source)
}

/// Read `source`, a typst file in `dir`. The `.typ` files it imports from
/// inside `dir` are read for what they define (see [`Reader::module`]).
pub fn read_in(source: &str, dir: &Path) -> Document {
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

// While the document is being read, links whose URL starts with one of
// these mark what can only be settled once more of it is known. No real
// URL starts with a control character, so no link is taken for a mark.
// None may survive `read`: every writer would emit it as it is.

/// Prefix of a link to a label; resolved once every label is known (see
/// [`Reader::finish`]).
const LABEL_LINK: char = '\u{1}';
/// Prefix of the empty link that ends each heading, naming the heading's
/// id (see [`Reader::finish`]).
const HEADING_MARK: char = '\u{2}';
/// Prefix of the empty link standing in for a smart quote, `"` or `'`,
/// until its paragraph is complete (see [`Quoter`]).
const QUOTE_MARK: char = '\u{3}';
/// The URL of the link around a box's content until its paragraph's
/// quotes are settled: typst lays a box out on its own (see [`Quoter`]).
const FRAME_MARK: char = '\u{4}';

/// How deep imports of imports are followed (see [`Reader::module`]).
const MAX_IMPORT_DEPTH: usize = 8;
/// The largest imported file read (see [`Reader::module`]).
const MAX_IMPORT_BYTES: u64 = 1 << 20;
/// How deep calls and markup (a content block, a list item) are read
/// inside each other, a call and the content blocks it's given one level
/// (see [`Reader::nested`]): as deep as typst parses calls given content
/// (126 deep, `#text[#text[…]]`), and shallow enough for the 2 MiB stack
/// conversions run on, which the 256 levels typst's parser allows other
/// calls (`#f(f(…))`) would overflow.
const MAX_NESTING: usize = 128;
/// The most values an array bound by `#let` is followed with (see
/// [`Reader::value`]): no short source (`#let a = (..a, ..a)`, again and
/// again) makes the reader build a huge one.
const MAX_ARRAY: usize = 1 << 10;
/// What the reader may spend unwrapping calls to the document's wrappers
/// of tables and their parts (see [`Reader::unwrapped`]), in bytes: of the
/// wrappers read, and of the arguments they give (see [`argument_cost`]).
/// It's this, or [`UNWRAPPED_PER_BYTE`] for each byte of a longer
/// document, which may call its wrappers in every row: no short source
/// (`#let w1() = table.cell[#w0() #w0()]`, or `#let w1(..c) = w0(..c,
/// ..c)`, again and again) makes the reader read or make a huge one.
const MAX_UNWRAPPED: usize = 1 << 22;
/// What the reader may spend unwrapping calls for each byte of a long
/// document (see [`MAX_UNWRAPPED`]).
const UNWRAPPED_PER_BYTE: usize = 32;
/// How deep the body of a function the document defines is read for what
/// it shows (see [`Reader::shown_names`] and [`Reader::may_show`]): deeper
/// than any body is written but for chains typst parses past its nesting
/// limit (`x.f().g()…`), past which each name is taken to show, so the
/// reader fits the 2 MiB stack conversions run on. Chains of operators
/// (`a + b + …`) are read along without going deeper.
const MAX_BODY_DEPTH: usize = 4 * MAX_NESTING;

/// What references to headings may copy in all, in bytes: the heading's
/// text they show and the link to it (see [`reference_cost`]). It's this,
/// or [`REFERENCE_TEXT_PER_BYTE`] for each byte of a longer document. Past
/// it, a reference is its label's text: no short source (a long heading
/// referred to again and again) makes the reader copy a huge text.
const MAX_REFERENCE_TEXT: usize = 1 << 20;
/// What references may copy for each byte of a long document (see
/// [`MAX_REFERENCE_TEXT`]).
const REFERENCE_TEXT_PER_BYTE: usize = 32;
/// What each inline of a heading's text a reference copies costs, past its
/// text's bytes (see [`reference_cost`]): a writer may write one at length
/// (math as MathML).
const REFERENCE_INLINE_COST: usize = 64;
/// How many headings deep a heading shows the text of the headings it
/// references (see [`HeadingTexts`]), past which a reference shows its
/// label: deeper than documents refer from heading to heading, and shallow
/// enough that a chain of such references neither overflows the stack nor
/// makes each heading's text as long as the chain.
const MAX_HEADING_REFERENCES: usize = 16;

/// The parts of a table or grid, called as `table.header(…)` and so on.
const TABLE_PARTS: &[&str] = &["header", "footer", "cell", "hline", "vline"];
/// The names a function the reader can't see is taken to show what it's
/// given by, strings included: those of the text a box or a template
/// sets (`showybox(title: "Hint")`). By any other name, it takes a
/// string, or a value the reader can't tell, for a setting (see
/// [`Reader::shown_by_name`]).
const SHOWN_NAMES: &[&str] = &[
    "title", "subtitle", "caption", "body", "content", "text", "header", "footer",
];
/// The most columns a table is read with (see [`Reader::table`]).
const MAX_COLUMNS: usize = 1000;
/// The most cells the tables of a document grow by past the cells they're
/// given: for their cells' spans (see [`grid_rows`]), and for columns left
/// empty at their end (see [`Reader::table`]). With [`MAX_COLUMNS`], no
/// short source (`columns: 99999999`, `rowspan: 99999999`, `#table(columns:
/// 1000, [a])`, again and again) makes the reader build huge tables.
const MAX_GROWN_CELLS: usize = 1 << 20;

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
    /// Headings read so far: the next one's id.
    headings_read: usize,
    /// What the names bound by `#import` and `#let` stand for; a name not
    /// here is the standard library's, if it has one (see
    /// [`Reader::resolve`]).
    names: Names,
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
}

/// The names a typst file binds (see [`Reader::module`]).
type Module = Rc<HashMap<String, Name>>;

/// What a name bound by `#import` or `#let` stands for, as far as the
/// reader can tell without running code. Cloning one is cheap.
#[derive(Clone)]
enum Name {
    /// A function of the standard library, by its name there: `header`
    /// after `#import table: header` is `table.header` (see
    /// [`Reader::resolve`]).
    Function(String),
    /// A function the document makes to call a table, a grid or one of
    /// their parts with arguments of its own (see [`Wrapper`]): `#let
    /// wide(body) = table.cell(colspan: 2, body)`, or
    /// `table.cell.with(colspan: 2)`.
    Wrapper(Rc<Wrapper>),
    /// One of a table's parts ([`TABLE_PARTS`]), made by a call, with the
    /// cells it was made with: `toprule` after `#let toprule =
    /// table.hline(…)` is a line, and `hdr` after `#let hdr =
    /// table.header([A], [B])` a header of two cells.
    Part {
        part: &'static str,
        cells: Rc<[Cell]>,
    },
    /// An imported file's names: `lib` after `#import "lib.typ"`.
    Module(Module),
    /// An array of values, for destructuring: `#let parts = (table.header,
    /// table.hline)`. It holds no more than [`MAX_ARRAY`], and values nest
    /// in it no more than [`MAX_NESTING`] deep, `depth`.
    Array { values: Rc<[Name]>, depth: usize },
    /// A dictionary of values, by key, nesting as an array's do.
    Dict {
        entries: Rc<HashMap<String, Name>>,
        depth: usize,
    },
    /// What the document makes itself that the reader can't tell more of
    /// (what a function it defines or can't see returns, a method of that):
    /// none of typst's own, it shows the content it's given.
    Opaque,
    /// A value one of typst's own functions makes (`state("x")`, `str(n)`),
    /// or an array or a dictionary the document makes: what its methods
    /// compute from what they're given (`.update(…)`, `.push(…)`,
    /// `.join(…)`) the reader can't tell, and they show nothing.
    Value,
    /// A function the document defines: it shows the content and strings
    /// it's given for the parameters it shows.
    Defined(Rc<Signature>),
    /// A preset of a function the document defines or one the reader can't
    /// see (`greet.with([Ann])`): it's given the arguments it presets
    /// before those it's called with (see [`Preset`]).
    Preset(Rc<Preset>),
    /// A function the document makes that shows nothing: one returning a
    /// setting, such as a length, an alignment or a color (`#let gap(x) =
    /// 1em`; see [`Reader::may_show`]).
    Setting,
    /// What's imported from where the reader can't read (a package, a file
    /// outside the input's folder): it may be anything, a table's part
    /// included.
    Unknown,
}

impl Name {
    /// How deep values nest in it: arrays and dictionaries in one another,
    /// and presets of presets (see [`MAX_NESTING`]).
    fn depth(&self) -> usize {
        match self {
            Name::Array { depth, .. } | Name::Dict { depth, .. } => *depth,
            Name::Preset(preset) => preset.depth,
            _ => 0,
        }
    }

    /// What `self.field` stands for.
    fn member(self, field: &str) -> Name {
        match self {
            Name::Function(name) if name == "std" => Name::Function(field.to_string()),
            Name::Function(name) => Name::Function(format!("{name}.{field}")),
            Name::Module(names) => names.get(field).cloned().unwrap_or(Name::Unknown),
            // An entry, or a method of the dictionary's own.
            Name::Dict { entries, .. } => entries.get(field).cloned().unwrap_or_else(|| {
                match std_value(&format!("dictionary.{field}")) {
                    Some(_) => Name::Value,
                    None => Name::Opaque,
                }
            }),
            Name::Array { .. } | Name::Value => Name::Value,
            Name::Unknown => Name::Unknown,
            Name::Wrapper(_)
            | Name::Part { .. }
            | Name::Opaque
            | Name::Defined(_)
            | Name::Preset(_)
            | Name::Setting => Name::Opaque,
        }
    }
}

/// A function the document makes that calls a table, a grid, one of
/// their parts or another such function, and does nothing else: one it
/// defines (`#let numhead(..c) = table.header([\#], ..c)`; see
/// [`Reader::returned`]) or a preset (`table.cell.with(colspan: 2)`).
/// Called, it calls that with arguments of its own (see
/// [`Wrapper::arguments`]).
struct Wrapper {
    /// The function as it's written: a closure, or a call to `.with`.
    node: SyntaxNode,
    /// What it calls, as that stood where the function was made: a
    /// function of typst's own, or another wrapper.
    calls: Name,
    /// The function of typst's own it ends up calling (`table.header`).
    function: String,
    /// How many wrappers it calls through, itself included: no more than
    /// [`MAX_NESTING`].
    depth: usize,
}

impl Wrapper {
    /// The function written as `node` calling what `calls` stands for, if
    /// that ends up calling a table or a grid, or one of their parts unless
    /// rules style it (`styled`): typst takes a styled part for a cell.
    /// Past [`MAX_NESTING`] wrappers deep, it's the function it ends up
    /// calling, its arguments its caller's.
    fn of(node: &SyntaxNode, calls: Name, styled: bool) -> Option<Name> {
        let (function, depth) = match &calls {
            Name::Function(name) => (name.clone(), 1),
            Name::Wrapper(wrapper) => (wrapper.function.clone(), wrapper.depth + 1),
            _ => return None,
        };
        let wraps = matches!(function.as_str(), "table" | "grid")
            || (!styled && table_part(&function).is_some());
        if !wraps {
            return None;
        }
        if depth > MAX_NESTING {
            return Some(Name::Function(function));
        }
        Some(Name::Wrapper(Rc::new(Wrapper {
            node: node.clone(),
            calls,
            function,
            depth,
        })))
    }

    /// What the preset `preset`, `f.with(…)`, stands for, `f` standing for
    /// `presets`: a wrapper of a table, a grid or one of their parts, or a
    /// preset of a function the document defines or one the reader can't
    /// see, each keeping the arguments it presets, or what `f` does.
    fn preset(preset: ast::FuncCall, presets: Name) -> Name {
        match presets {
            Name::Defined(_) | Name::Preset(_) | Name::Opaque | Name::Unknown => {
                // Past `MAX_NESTING` presets of presets (`#let g =
                // g.with()`, again and again), it's the one before.
                let depth = presets.depth() + 1;
                if depth > MAX_NESTING {
                    return presets;
                }
                Name::Preset(Rc::new(Preset {
                    node: preset.to_untyped().clone(),
                    of: presets,
                    depth,
                }))
            }
            _ => Wrapper::of(preset.to_untyped(), presets.clone(), false).unwrap_or(presets),
        }
    }

    /// The arguments the function gives what it calls when it's given
    /// `given`. A preset gives those it presets, then `given`. A function
    /// the document defines gives those of the call its body makes, as
    /// typst passes them on, its own kept (see [`Bound::give`]). Each
    /// argument it gives spends its cost (see [`argument_cost`]) of
    /// `budget`: past what that pays for, it gives no more. `part` tells
    /// the calls to a table's parts (see [`Reader::table_part`]).
    fn arguments<'a>(
        &'a self,
        mut given: Args<'a>,
        budget: &mut usize,
        part: &dyn Fn(Expr<'a>) -> bool,
    ) -> Args<'a> {
        let closure = match self.node.cast::<Expr>() {
            Some(Expr::Closure(closure)) => closure,
            Some(Expr::FuncCall(preset)) => {
                let mut args = Args::new(preset.args());
                args.append(given);
                return args;
            }
            _ => return given,
        };
        let Some((call, _)) = returned_call(closure.body()) else {
            return given;
        };
        let calls = std::mem::take(&mut given.calls);
        let mut bound = Bound::new(closure, &given);
        bound.calls = calls;
        let mut args = bound.give(call.args(), budget, part, 0);
        args.calls.extend(bound.calls);
        args
    }
}

/// What a wrapper's parameters are given in a call to it (see
/// [`Wrapper::arguments`]).
struct Bound<'a> {
    /// What each parameter but the sink is given, if anything.
    params: HashMap<&'a str, Option<Expr<'a>>>,
    /// The name of its argument sink, if it has one with a name.
    sink: Option<&'a str>,
    /// What the sink takes: the arguments given by position past the
    /// parameters, and those given by name no parameter takes.
    sink_args: Vec<(Option<&'a str>, Expr<'a>)>,
    /// The parameters each name the `let`s in its body bind uses.
    locals: HashMap<&'a str, Vec<&'a str>>,
    /// What each name a `let` in its body binds on its own (`let c =
    /// …`) is bound to, the last binding of each.
    inits: HashMap<&'a str, Expr<'a>>,
    /// What the calls among the arguments given are given where they're
    /// written (see [`Args::calls`]): they stay theirs wherever the
    /// wrapper gives them.
    calls: Vec<(&'a SyntaxNode, Rc<Args<'a>>)>,
}

/// What an argument a wrapper gives stands for, written with its
/// parameters, as far as the reader can tell without running code (see
/// [`Bound::view`]).
enum View<'a> {
    /// Arguments, by position and by name: what its argument sink takes.
    Args(Vec<(Option<&'a str>, Expr<'a>)>),
    /// An array's items (`c.pos()`).
    Array(Vec<Expr<'a>>),
    /// A dictionary's entries, by key (`c.named()`).
    Dict(Vec<(&'a str, Expr<'a>)>),
    /// One value: what a parameter is given, or one picked of an array
    /// (`c.pos().at(1)`).
    One(Expr<'a>),
    /// What a parameter given nothing, or a place past an array's end,
    /// stands for.
    Nothing,
}

impl<'a> View<'a> {
    /// What it gives as an argument by position: itself, or of arguments
    /// and arrays, the items by position, each an argument, as the reader
    /// takes what's made of them (`table.cell(c.pos().join())` shows them
    /// all).
    fn positional(self) -> Vec<(Option<&'a str>, Expr<'a>)> {
        match self {
            View::Args(items) => items
                .into_iter()
                .filter(|(name, _)| name.is_none())
                .collect(),
            View::Array(items) => items.into_iter().map(|expr| (None, expr)).collect(),
            View::One(expr) => vec![(None, expr)],
            View::Dict(_) | View::Nothing => Vec::new(),
        }
    }

    /// What spreading it gives: arguments as they are given, an array's
    /// items by position (see [`spread_items`]), and a dictionary's by
    /// name.
    fn spread(self) -> Vec<(Option<&'a str>, Expr<'a>)> {
        match self {
            View::Args(items) => items,
            View::Array(items) => items.into_iter().map(|expr| (None, expr)).collect(),
            View::Dict(entries) => entries
                .into_iter()
                .map(|(key, expr)| (Some(key), expr))
                .collect(),
            View::One(expr) => spread_items(expr).collect(),
            View::Nothing => Vec::new(),
        }
    }

    /// Its items, if it's an array the reader can tell.
    fn items(self) -> Option<Vec<Expr<'a>>> {
        match self {
            View::Array(items) => Some(items),
            View::One(expr) => array_items(expr),
            View::Args(_) | View::Dict(_) | View::Nothing => None,
        }
    }

    /// What its method `method`, given `args`, makes of it, if the reader
    /// can tell: of arguments, those given by position (`pos`) or by name
    /// (`named`), or the one at a place or of a name (`at`); of a
    /// dictionary, the entry of a key, or its values; of an array, the
    /// item at a place (`at`, `first`, `last`), a slice of them, or them
    /// reversed (`rev`) or each made into something (`map`), which is
    /// taken for what it's made of. Places count from the end if negative,
    /// and one there's nothing at is the `default` given, if any.
    fn method(self, method: &str, args: &Args<'a>) -> Option<View<'a>> {
        let mut given = args.positional();
        let (first, second) = (given.next(), given.next());
        let found = |found: Option<Expr<'a>>| {
            found
                .or_else(|| args.named("default"))
                .map_or(View::Nothing, View::One)
        };
        // The entry of the key given first, the last of that key.
        let entry = |entries: Vec<(&'a str, Expr<'a>)>| {
            let Some(Expr::Str(key)) = first else {
                return None;
            };
            let entry = entries
                .into_iter()
                .rfind(|&(name, _)| name == key.get().as_str());
            Some(found(entry.map(|(_, expr)| expr)))
        };
        let view = match (self, method) {
            // Of arguments, those given by name or by position, or one of
            // them, by its name or its place.
            (View::Args(items), "named") => View::Dict(
                items
                    .into_iter()
                    .filter_map(|(name, expr)| Some((name?, expr)))
                    .collect(),
            ),
            (View::Args(items), "at") if matches!(first, Some(Expr::Str(_))) => {
                return View::Args(items)
                    .method("named", args)?
                    .method(method, args);
            }
            (View::Args(items), "pos" | "at") => {
                let items = items
                    .into_iter()
                    .filter_map(|(name, expr)| name.is_none().then_some(expr));
                let positional = View::Array(items.collect());
                if method == "at" {
                    return positional.method(method, args);
                }
                positional
            }
            (View::Dict(entries), "at") => return entry(entries),
            (View::Dict(entries), "values") => {
                View::Array(entries.into_iter().map(|(_, expr)| expr).collect())
            }
            (View::Args(_) | View::Dict(_), _) => return None,
            (view, method) => {
                let items = view.items()?;
                let len = items.len();
                match method {
                    "at" => found(place(int(first?)?, len).map(|at| items[at])),
                    "first" => found(items.first().copied()),
                    "last" => found(items.last().copied()),
                    "slice" => {
                        let start = slice_place(int(first?)?, len);
                        let end = match (second, args.named("count")) {
                            (Some(end), _) => slice_place(int(end)?, len),
                            (None, Some(count)) => {
                                let count = usize::try_from(int(count)?).ok()?;
                                start.saturating_add(count).min(len)
                            }
                            (None, None) => len,
                        };
                        View::Array(items[start..end.max(start)].to_vec())
                    }
                    "rev" => View::Array(items.into_iter().rev().collect()),
                    "map" => View::Array(items),
                    _ => return None,
                }
            }
        };
        Some(view)
    }
}

/// The integer `expr` is written as, if it's one: `2`, `-1`.
fn int(expr: Expr) -> Option<i64> {
    let mut expr = expr;
    let mut negative = false;
    loop {
        match unparenthesized(expr) {
            Expr::Int(n) => {
                return if negative {
                    n.get().checked_neg()
                } else {
                    Some(n.get())
                };
            }
            Expr::Unary(unary) if unary.op() == ast::UnOp::Neg => {
                negative = !negative;
                expr = unary.expr();
            }
            Expr::Unary(unary) if unary.op() == ast::UnOp::Pos => expr = unary.expr(),
            _ => return None,
        }
    }
}

/// The place `index` picks of an array of `len` items, counted from the
/// end if negative, if it's in the array.
fn place(index: i64, len: usize) -> Option<usize> {
    let back = usize::try_from(index.unsigned_abs()).ok()?;
    if index < 0 {
        len.checked_sub(back)
    } else {
        Some(back).filter(|&at| at < len)
    }
}

/// The place `index` names as an end of a slice of an array of `len`
/// items, counted from the end if negative, kept to the array.
fn slice_place(index: i64, len: usize) -> usize {
    let back = usize::try_from(index.unsigned_abs()).unwrap_or(usize::MAX);
    if index < 0 {
        len.saturating_sub(back)
    } else {
        back.min(len)
    }
}

impl<'a> Bound<'a> {
    /// The parameters of `closure` given `given`, as typst binds them:
    /// those by position in turn, those by name by their name.
    fn new(closure: ast::Closure<'a>, given: &Args<'a>) -> Self {
        let mut bound = Bound {
            params: HashMap::new(),
            sink: None,
            sink_args: Vec::new(),
            locals: HashMap::new(),
            inits: HashMap::new(),
            calls: Vec::new(),
        };
        let mut named = Vec::new();
        let mut positional = 0;
        for param in closure.params().children() {
            match param {
                ast::Param::Pos(ast::Pattern::Normal(Expr::Ident(name))) => {
                    let value = given.positional().nth(positional);
                    bound.params.insert(name.as_str(), value);
                    positional += 1;
                }
                // Destructured: what its names are given, the reader can't
                // tell.
                ast::Param::Pos(pattern) => {
                    for name in pattern.bindings() {
                        bound.params.insert(name.as_str(), None);
                    }
                    positional += 1;
                }
                ast::Param::Named(param) => {
                    let name = param.name().as_str();
                    let value = given.named(name).unwrap_or(param.expr());
                    bound.params.insert(name, Some(value));
                    named.push(name);
                }
                ast::Param::Spread(spread) => {
                    bound.sink = spread.sink_ident().map(|ident| ident.as_str());
                }
            }
        }
        let mut position = 0;
        for &(name, expr) in &given.items {
            let taken = match name {
                None => {
                    position += 1;
                    position > positional
                }
                Some(name) => !named.contains(&name),
            };
            if taken {
                bound.sink_args.push((name, expr));
            }
        }
        bound.bind_locals(closure.body().to_untyped());
        bound
    }

    /// Note the parameters each name a `let` in `node` binds uses, in
    /// turn, through the names bound before it, and what each name it
    /// binds on its own is bound to.
    fn bind_locals(&mut self, node: &'a SyntaxNode) {
        if let Some(binding) = node.cast::<ast::LetBinding>() {
            let uses = binding
                .init()
                .map_or_else(Vec::new, |init| self.params_used(init.to_untyped()));
            for name in binding.kind().bindings() {
                self.locals.insert(name.as_str(), uses.clone());
                self.inits.remove(name.as_str());
            }
            if let ast::LetBindingKind::Normal(ast::Pattern::Normal(Expr::Ident(name))) =
                binding.kind()
                && let Some(init) = binding.init()
            {
                self.inits.insert(name.as_str(), init);
            }
        }
        for child in node.children() {
            self.bind_locals(child);
        }
    }

    fn is_param(&self, name: &str) -> bool {
        self.params.contains_key(name) || self.sink == Some(name)
    }

    /// What `args`, written in the wrapper's body, give, as typst passes
    /// them on: each stands for what it's written as (see
    /// [`Bound::view`]), a parameter for what it's given (a named one's
    /// default if nothing), its argument sink (`..c`) for all that takes,
    /// by position and by name, and a method of them for what it picks of
    /// that (`..c.named()`, `c.pos().at(1)`). A call to a table's part
    /// (`part`) using parameters (`table.header(title, [B])`) is given as
    /// it's written, with what its own arguments give so (see
    /// [`Args::calls`]), [`MAX_NESTING`] calls deep. Another argument using
    /// parameters otherwise (`[#title: #sub]`, `..for x in c.pos() {…}`)
    /// stands for what they're given by position, as the reader runs no
    /// code, and a named one is left out. Each spends its cost of `budget`
    /// (see [`Wrapper::arguments`]).
    fn give(
        &self,
        args: ast::Args<'a>,
        budget: &mut usize,
        part: &dyn Fn(Expr<'a>) -> bool,
        depth: usize,
    ) -> Args<'a> {
        let mut out = Args::default();
        for arg in args.items() {
            let more = match arg {
                ast::Arg::Pos(expr) => match (self.view(expr), unparenthesized(expr)) {
                    (Some(view), _) => view.positional(),
                    (None, Expr::FuncCall(call)) if depth < MAX_NESTING && part(expr) => {
                        let mut own = self.give(call.args(), budget, part, depth + 1);
                        own.calls.extend(self.calls.iter().cloned());
                        out.calls.push((call.to_untyped(), Rc::new(own)));
                        vec![(None, expr)]
                    }
                    (None, _) => self.approximated(expr, View::positional),
                },
                ast::Arg::Named(named) => match self.view(named.expr()) {
                    Some(View::One(expr)) => vec![(Some(named.name().as_str()), expr)],
                    _ => Vec::new(),
                },
                ast::Arg::Spread(spread) => match self.view(spread.expr()) {
                    Some(view) => view.spread(),
                    None => self.approximated(spread.expr(), |view| match view {
                        View::One(expr) => spread_items(expr).collect(),
                        view => view.positional(),
                    }),
                },
            };
            if !spend(budget, &mut out.items, more) {
                break;
            }
        }
        out
    }

    /// What the parameter `param` stands for: what the sink takes, or what
    /// it's given.
    fn base(&self, param: &str) -> View<'a> {
        if self.sink == Some(param) {
            return View::Args(self.sink_args.clone());
        }
        match self.params.get(param) {
            Some(&Some(expr)) => View::One(expr),
            _ => View::Nothing,
        }
    }

    /// What `expr`, written in the wrapper's body, stands for, if the
    /// reader can tell (see [`View`]): a parameter what it's given (see
    /// [`Bound::base`]), a name a `let` binds what it's bound to, a method
    /// of these what it makes of them (see [`View::method`]), through a
    /// chain of them without a level of the stack each, and what uses no
    /// parameter itself.
    fn view(&self, expr: Expr<'a>) -> Option<View<'a>> {
        self.picked(expr).or_else(|| {
            let uses = self.params_used(expr.to_untyped());
            uses.is_empty().then_some(View::One(expr))
        })
    }

    /// What `expr` stands for as a chain of methods of a parameter or of
    /// what uses none (see [`Bound::view`]).
    fn picked(&self, expr: Expr<'a>) -> Option<View<'a>> {
        // The methods called, outermost first, with their arguments.
        let mut methods = Vec::new();
        // The `let`s followed, each once: one using the name it binds
        // (`let c = c.pos()`) uses what the name stood for before.
        let mut followed = HashSet::new();
        let mut at = expr;
        let view = loop {
            match unparenthesized(at) {
                Expr::FuncCall(call) if let Expr::FieldAccess(access) = call.callee() => {
                    methods.push((access.field().as_str(), call.args()));
                    at = access.target();
                }
                Expr::Ident(ident) => {
                    let name = ident.as_str();
                    if let Some(&init) = self.inits.get(name)
                        && followed.insert(name)
                    {
                        at = init;
                    } else if self.is_param(name)
                        && (followed.contains(name) || !self.locals.contains_key(name))
                    {
                        break self.base(name);
                    } else if self.params_used(at.to_untyped()).is_empty() {
                        break View::One(at);
                    } else {
                        return None;
                    }
                }
                other if self.params_used(other.to_untyped()).is_empty() => break View::One(other),
                _ => return None,
            }
        };
        methods
            .into_iter()
            .rev()
            .try_fold(view, |view, (method, args)| {
                view.method(method, &Args::new(args))
            })
    }

    /// What `expr`, using parameters in a way the reader can't tell, is
    /// taken to give: what each parameter it uses stands for (see
    /// [`Bound::base`]), in turn, as `give` gives that.
    fn approximated(
        &self,
        expr: Expr<'a>,
        give: impl Fn(View<'a>) -> Vec<(Option<&'a str>, Expr<'a>)>,
    ) -> Vec<(Option<&'a str>, Expr<'a>)> {
        self.params_used(expr.to_untyped())
            .into_iter()
            .flat_map(|param| give(self.base(param)))
            .collect()
    }

    /// The parameters `node` uses, in turn: those it names, and those
    /// the names it uses that `let`s bound use.
    fn params_used(&self, node: &'a SyntaxNode) -> Vec<&'a str> {
        let mut names = Vec::new();
        used_names(node, &mut names);
        let mut params = Vec::new();
        for name in names {
            let uses = match self.locals.get(name) {
                Some(uses) => uses.as_slice(),
                None if self.is_param(name) => std::slice::from_ref(&name),
                None => &[],
            };
            for &param in uses {
                if !params.contains(&param) {
                    params.push(param);
                }
            }
        }
        params
    }
}

/// What an argument a wrapper gives costs of what the reader may spend
/// unwrapping calls (see [`MAX_UNWRAPPED`]): the bytes it takes to hold
/// and to read.
fn argument_cost(expr: Expr) -> usize {
    std::mem::size_of::<(Option<&str>, Expr)>() + expr.to_untyped().len()
}

/// Add to `items` those of `more` that `budget` pays for, in turn, each
/// spending its cost (see [`argument_cost`]); whether it pays for all.
fn spend<'a>(
    budget: &mut usize,
    items: &mut Vec<(Option<&'a str>, Expr<'a>)>,
    more: Vec<(Option<&'a str>, Expr<'a>)>,
) -> bool {
    for item in more {
        let Some(left) = budget.checked_sub(argument_cost(item.1)) else {
            *budget = 0;
            return false;
        };
        *budget = left;
        items.push(item);
    }
    true
}

/// The names `node` uses, in turn, as typst reads them: not the fields it
/// reads (`x.title`) or the names it gives arguments (`title: x`).
fn used_names<'a>(node: &'a SyntaxNode, names: &mut Vec<&'a str>) {
    match node.cast::<Expr>() {
        Some(Expr::Ident(ident)) => names.push(ident.as_str()),
        Some(Expr::MathIdent(ident)) => names.push(ident.as_str()),
        Some(Expr::FieldAccess(access)) => used_names(access.target().to_untyped(), names),
        _ => {
            for child in node.children() {
                match child.cast::<ast::Named>() {
                    Some(named) => used_names(named.expr().to_untyped(), names),
                    None => used_names(child, names),
                }
            }
        }
    }
}

/// The arguments spreading `expr` gives, as [`Args::new`] takes them:
/// the items of an array the reader can tell, by position.
fn spread_items<'a>(expr: Expr<'a>) -> impl Iterator<Item = (Option<&'a str>, Expr<'a>)> {
    array_items(expr)
        .unwrap_or_default()
        .into_iter()
        .map(|expr| (None, expr))
}

/// What a function the document defines shows of what's given for one of
/// its parameters, as far as the reader can tell from its body (see
/// [`Signature`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Shows {
    Nothing,
    /// Content, but not a string, which it takes for a setting there: what
    /// it gives by name to a function the reader can't see, but by a name
    /// for what that shows (`showybox(frame: f, …)`, not `title:`; see
    /// [`SHOWN_NAMES`]), or to a method of a value it can't tell.
    Content,
    /// Content and strings, as text.
    All,
}

impl Shows {
    /// Whether it shows an argument that's a string if `string`, and else
    /// content.
    fn admits(self, string: bool) -> bool {
        match self {
            Shows::Nothing => false,
            Shows::Content => !string,
            Shows::All => true,
        }
    }
}

/// A preset, `f.with(…)`, of a function the document defines or one the
/// reader can't see (see [`Name::Preset`]).
struct Preset {
    /// The call to `.with`, whose arguments the function is given first.
    node: SyntaxNode,
    /// What it presets: that function, or a preset of it.
    of: Name,
    /// How many presets deep it is, itself included: no more than
    /// [`MAX_NESTING`].
    depth: usize,
}

/// The presets a call goes through to reach the function it calls, the
/// outermost first (see [`Presets::of`]).
struct Presets(Vec<Rc<Preset>>);

impl Presets {
    /// The presets `name` is, and the function they preset past them all:
    /// `name` itself, and no presets, if it's no preset.
    fn of(name: Name) -> (Presets, Name) {
        let mut presets = Vec::new();
        let mut name = name;
        while let Name::Preset(preset) = name {
            name = preset.of.clone();
            presets.push(preset);
        }
        (Presets(presets), name)
    }

    /// The arguments the function they preset is given when they're called
    /// with `args`: the innermost preset's first, then the others', then
    /// `args`.
    fn give<'p>(&'p self, args: &Args<'p>) -> Args<'p> {
        let mut given = Args::default();
        for preset in self.0.iter().rev() {
            if let Some(call) = preset.node.cast::<ast::FuncCall>() {
                given.append(Args::new(call.args()));
            }
        }
        given.append(args.clone());
        given
    }

    /// How many arguments they give by position, if the reader can tell
    /// (none spread from what it can't).
    fn positional(&self) -> Option<usize> {
        let empty = Args::default();
        let given = self.give(&empty);
        given.spread.is_none().then(|| given.positional().count())
    }
}

/// What a function the document defines shows of each of its parameters,
/// as far as the reader can tell from its body (see
/// [`Reader::shown_names`]), so of the strings and content given to it.
struct Signature {
    /// What it shows of each positional parameter, in turn.
    positional: Vec<Shows>,
    /// What it shows of the positional arguments its argument sink
    /// (`..args`) takes, if it has one.
    sink: Option<Sink>,
    /// Its named parameters, and what it shows of each.
    named: Vec<(String, Shows)>,
}

/// What a function the document defines shows of the positional arguments
/// its argument sink takes (see [`Signature`]).
enum Sink {
    /// The same of all of them.
    All(Shows),
    /// All of those at the places its body picks (`args.pos().at(1)`),
    /// counted from the end if negative (`args.pos().last()`), and none of
    /// the others: their count (`args.pos().len()`) picks none.
    Picks(Vec<i64>),
}

impl Signature {
    /// What the function shows of its `n`th positional argument of
    /// `count`, if the reader can tell how many it's given.
    fn shows(&self, n: usize, count: Option<usize>) -> Shows {
        if let Some(&shows) = self.positional.get(n) {
            return shows;
        }
        // Its place among those the sink takes, and how many it takes.
        let at = n - self.positional.len();
        let taken = count.map(|count| count.saturating_sub(self.positional.len()));
        match &self.sink {
            None => Shows::Nothing,
            Some(Sink::All(shows)) => *shows,
            Some(Sink::Picks(picks)) => {
                let picked = picks.iter().any(|&pick| match usize::try_from(pick) {
                    Ok(pick) => pick == at,
                    // From the end: any of them, if the reader can't tell how
                    // many there are.
                    Err(_) => taken.is_none_or(|taken| {
                        let back = usize::try_from(pick.unsigned_abs()).ok();
                        back.and_then(|back| taken.checked_sub(back)) == Some(at)
                    }),
                });
                if picked { Shows::All } else { Shows::Nothing }
            }
        }
    }

    /// What the function shows of a positional argument given where the
    /// reader can't tell its place (past arguments spread from what it
    /// can't tell): the most it shows of any.
    fn shows_any(&self) -> Shows {
        let sink = match &self.sink {
            None => Shows::Nothing,
            Some(Sink::All(shows)) => *shows,
            Some(Sink::Picks(picks)) if picks.is_empty() => Shows::Nothing,
            Some(Sink::Picks(_)) => Shows::All,
        };
        self.positional.iter().copied().fold(sink, Shows::max)
    }

    /// What the function shows of the argument given as `name`: what it
    /// shows of its parameter of that name, else what it shows of its sink,
    /// but for a string, which a sink takes by name for a setting, as
    /// typst's functions do (`..args` passed on to `text`), unless by a
    /// name for what it shows ([`SHOWN_NAMES`]: `..args` passed on to
    /// `showybox`, given `title: "Hint"`).
    fn shows_named(&self, name: &str) -> Shows {
        match self.named.iter().find(|(param, _)| param == name) {
            Some(&(_, shows)) => shows,
            None => match self.sink {
                Some(Sink::All(shows)) if SHOWN_NAMES.contains(&name) => shows,
                Some(Sink::All(shows)) => shows.min(Shows::Content),
                _ => Shows::Nothing,
            },
        }
    }

    /// The signature of the preset `f.with(…)` of the function, given
    /// `given` arguments by position, if the reader can tell how many: it
    /// takes the parameters past those, and what the sink took past them.
    /// Of an unknown count, any argument it's given may go to any of them.
    fn preset(&self, given: Option<usize>) -> Signature {
        let named = self.named.clone();
        let Some(given) = given else {
            return Signature {
                positional: Vec::new(),
                sink: Some(Sink::All(self.shows_any())),
                named,
            };
        };
        let positional = self.positional.get(given..).unwrap_or_default().to_vec();
        let taken = i64::try_from(given.saturating_sub(self.positional.len())).unwrap_or(i64::MAX);
        let sink = self.sink.as_ref().map(|sink| match sink {
            Sink::All(shows) => Sink::All(*shows),
            // Those counted from the start move up, and those the preset
            // takes are gone.
            Sink::Picks(picks) => Sink::Picks(
                picks
                    .iter()
                    .filter_map(|&pick| match pick {
                        pick if pick < 0 => Some(pick),
                        pick => Some(pick - taken).filter(|&pick| pick >= 0),
                    })
                    .collect(),
            ),
        });
        Signature {
            positional,
            sink,
            named,
        }
    }
}

/// What a function's body shows of the names it uses, as far as the
/// reader can tell (see [`Reader::shown_names`]).
#[derive(Default)]
struct Shown<'a> {
    /// What it shows of each name it shows, the most of anywhere it uses
    /// it.
    names: HashMap<&'a str, Shows>,
    /// The places it picks of what the names of argument sinks take, by
    /// name (see [`Sink::Picks`]).
    picks: HashMap<&'a str, Vec<i64>>,
}

impl<'a> Shown<'a> {
    fn add(&mut self, name: &'a str, shows: Shows) {
        if shows > Shows::Nothing {
            let entry = self.names.entry(name).or_insert(shows);
            *entry = (*entry).max(shows);
        }
    }

    /// What the body shows of `name`: all of it if it picks a place of it.
    fn of(&self, name: &str) -> Shows {
        let shows = self.names.get(name).copied().unwrap_or(Shows::Nothing);
        if self.picks.contains_key(name) {
            Shows::All
        } else {
            shows
        }
    }
}

/// The names a function binds in its body, while [`Reader::may_show`] reads
/// it, with whether what each is bound to may show something, innermost
/// binding last: a name is found at once, however many there are.
#[derive(Default)]
struct Locals<'a> {
    bound: HashMap<&'a str, Vec<bool>>,
}

impl<'a> Locals<'a> {
    fn get(&self, name: &str) -> Option<bool> {
        self.bound.get(name)?.last().copied()
    }

    fn bind(&mut self, name: &'a str, shows: bool) {
        self.bound.entry(name).or_default().push(shows);
    }

    /// Undo the bindings of `names`, as the block binding them ends.
    fn unbind(&mut self, names: &[&'a str]) {
        for name in names {
            if let Some(bound) = self.bound.get_mut(name) {
                bound.pop();
            }
        }
    }
}

/// How an argument of a call is given (see [`std_targets`]).
#[derive(Clone, Copy)]
enum Given<'a> {
    Named(&'a str),
    Positional,
    /// Spread from what the reader can't tell: it may give any arguments.
    Spread,
}

/// What the parameter of one of typst's functions an argument goes to does
/// with it, as far as the reader can tell (see [`std_targets`]).
#[derive(Clone, Copy)]
struct Target {
    /// It may take content, which typst shows, a string as text.
    content: bool,
    /// It may show a string as text, though it takes no content (see
    /// [`shows_its_string`]).
    string: bool,
}

impl Target {
    /// Where an argument the reader can't place goes: anywhere.
    const ANY: Target = Target {
        content: true,
        string: true,
    };

    /// Whether what's given there may be shown.
    fn shows(self) -> bool {
        self.content || self.string
    }
}

/// The names bound by `#import` and `#let` so far, scoped as typst scopes
/// them: those a content block binds are its own, so past it the names
/// mean what they did before (see [`Reader::scoped`]). Leaving a scope
/// undoes only what it bound, however many names there are.
#[derive(Default)]
struct Names {
    bound: HashMap<String, Name>,
    /// For each scope open, innermost last: the names bound in it, and
    /// what each stood for before.
    shadowed: Vec<Vec<(String, Option<Name>)>>,
}

impl Names {
    fn get(&self, name: &str) -> Option<&Name> {
        self.bound.get(name)
    }

    fn insert(&mut self, name: String, value: Name) {
        let before = self.bound.insert(name.clone(), value);
        if let Some(scope) = self.shadowed.last_mut() {
            scope.push((name, before));
        }
    }

    fn open_scope(&mut self) {
        self.shadowed.push(Vec::new());
    }

    fn close_scope(&mut self) {
        let scope = self.shadowed.pop().unwrap_or_default();
        for (name, before) in scope.into_iter().rev() {
            match before {
                Some(value) => self.bound.insert(name, value),
                None => self.bound.remove(&name),
            };
        }
    }
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

/// What a table holds, in order (see [`Reader::table`]).
enum Piece {
    Cell(Cell),
    /// A header's cells.
    Header(Vec<Cell>),
    /// A footer's cells.
    Footer(Vec<Cell>),
}

/// A table cell: its content and the columns and rows it spans (see
/// [`grid_rows`]).
#[derive(Clone)]
struct Cell {
    content: Vec<Inline>,
    colspan: usize,
    rowspan: usize,
}

/// A typst element a following `<label>` attaches to.
#[derive(Clone)]
enum Labelled {
    Heading(usize),
    Note(String),
}

impl Reader {
    /// The document `source` holds, read to the end (see
    /// [`Reader::finish`]).
    fn read(mut self, source: &str) -> Document {
        self.unwrapping_left = MAX_UNWRAPPED.max(source.len().saturating_mul(UNWRAPPED_PER_BYTE));
        self.grown_cells_left = MAX_GROWN_CELLS;
        self.reference_text_left =
            MAX_REFERENCE_TEXT.max(source.len().saturating_mul(REFERENCE_TEXT_PER_BYTE));
        let root = typst::syntax::parse(source);
        let blocks = match root.cast::<ast::Markup>() {
            Some(markup) => self.blocks(markup),
            None => Vec::new(),
        };
        self.finish(blocks)
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
                    let content = self.blocks(item.body());
                    flow.push_item(None, content);
                }
                Expr::EnumItem(item) => {
                    let content = self.blocks(item.body());
                    let number = item.number().or(self.enum_start).unwrap_or(1);
                    flow.push_item(Some(number), content);
                }
                Expr::Space(_) if flow.list.is_some() => {}
                Expr::Parbreak(_) => flow.parbreak(),
                Expr::Heading(heading) => {
                    // The parser ends a heading at a label: its label
                    // follows it.
                    let content = self.inlines(heading.body());
                    let heading = self.heading(heading.depth().get(), content, &mut last);
                    flow.push_blocks(heading);
                }
                Expr::Equation(eq) if eq.block() => {
                    flow.push_blocks(vec![Block::Math(typst_math::equation(eq))]);
                }
                Expr::Raw(raw) if raw.block() => flow.push_blocks(vec![code_block(raw)]),
                Expr::TermItem(item) => {
                    let term = self.inlines(item.term());
                    let description = self.inlines(item.description());
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
        let out = read(self);
        self.names.close_scope();
        self.enum_start = enum_start;
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
                let content = self.inlines(strong.body());
                out.extend(styled(Inline::Strong, content));
            }
            Expr::Emph(emph) => {
                let content = self.inlines(emph.body());
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
            Expr::Equation(eq) => out.push(Inline::Math(typst_math::equation(eq))),
            Expr::Str(text) => push_text(out, &text.get()),
            Expr::ContentBlock(block) => out.extend(self.content_flow(block).into_inlines()),
            Expr::FuncCall(call) => out.extend(self.call(call, last).into_inlines()),
            Expr::SetRule(rule) => self.set_rule(rule),
            Expr::LetBinding(binding) => self.let_binding(binding),
            Expr::ModuleImport(import) => self.import(import),
            // Block markup inside inline content: its text.
            Expr::Heading(heading) => out.extend(self.inlines(heading.body())),
            Expr::ListItem(item) => out.extend(self.inlines(item.body())),
            Expr::EnumItem(item) => out.extend(self.inlines(item.body())),
            Expr::TermItem(item) => {
                let term = self.inlines(item.term());
                let description = self.inlines(item.description());
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
            // Code (`show`, loops) and values: dropped.
            _ => {}
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
            None => {}
        }
    }

    /// `#set document(title:, author:, date:)`, `#set text(lang:)` and
    /// `#set enum(start:)`.
    fn set_rule(&mut self, rule: ast::SetRule) {
        let Expr::Ident(target) = rule.target() else {
            return;
        };
        let args = Args::new(rule.args());
        match target.as_str() {
            "document" => {
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
            "text" => {
                if let Some(Expr::Str(lang)) = args.named("lang") {
                    let region = match args.named("region") {
                        Some(Expr::Str(region)) => format!("-{}", region.get().to_uppercase()),
                        _ => String::new(),
                    };
                    self.meta.lang = Some(format!("{}{region}", lang.get()));
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
            "line" => Item::blocks(vec![Block::ThematicBreak]),
            "linebreak" => Item::Inlines(vec![Inline::LineBreak]),
            // As typst parses it: past the settings it finds by their type
            // (`text(20pt, red)[…]`), its body is the argument left, content
            // or a string, wherever it is. Typst refuses more than one; none
            // is lost.
            "text" => {
                let shown = args
                    .positional()
                    .filter(|&expr| matches!(expr, Expr::Str(_)) || self.is_content(expr))
                    .collect();
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

    /// The positional arguments typst shows of a call to `name`, one of its
    /// own functions, by the parameters they go to (see [`std_targets`]):
    /// a string, where it may be shown, and what may be content (see
    /// [`Reader::is_content`]), where content or a string may be. Where
    /// only a string is shown, a value made of one shows that string (see
    /// [`Reader::made_of`]). A function returning neither content nor
    /// text shows nothing (`rgb("#f00")`).
    fn std_shown_args<'a>(&self, name: &str, args: &Args<'a>) -> Vec<Expr<'a>> {
        let Some(func) = std_function(name) else {
            return Vec::new();
        };
        if !func.returns().is_none_or(makes_content) {
            return Vec::new();
        }
        let given = args.given();
        let mut targets = std_targets(&func, &given);
        // No argument stands for the spread.
        if let Some(spread) = args.spread {
            targets.remove(spread);
        }
        args.items
            .iter()
            .zip(targets)
            .filter_map(|(&(name, expr), target)| {
                if name.is_some() || !target.shows() {
                    return None;
                }
                if target.string
                    && !target.content
                    && let Some(string) = self.made_of(expr)
                {
                    return Some(string);
                }
                let shown = matches!(expr, Expr::Str(_)) || self.is_content(expr);
                shown.then_some(expr)
            })
            .collect()
    }

    /// The string `expr` is made of, if it's a call to one of typst's
    /// functions making a value of what's given first that shows no text
    /// itself (`label("x")`, `bytes("x")`), given that string, or another
    /// such call: shown as text (`str(label("x"))`), it's that string.
    fn made_of<'a>(&self, expr: Expr<'a>) -> Option<Expr<'a>> {
        let mut expr = expr;
        loop {
            let Expr::FuncCall(call) = unparenthesized(expr) else {
                return None;
            };
            let func = match self.resolve(call.callee()) {
                Name::Function(name) => std_function(&name)?,
                _ => return None,
            };
            let first = func
                .params()
                .filter_map(|p| p.to_native())
                .find(|p| p.positional);
            let converts =
                first.is_some_and(shows_its_string) && !func.returns().is_none_or(makes_content);
            if !converts {
                return None;
            }
            expr = Args::new(call.args()).positional().next()?;
            if let Expr::Str(_) = expr {
                return Some(expr);
            }
        }
    }

    /// The arguments a function the document makes, or one the reader
    /// can't see, is taken to show, in turn: strings and content (see
    /// [`Reader::is_content`]). Of a function the document defines, those
    /// given for the parameters its body shows (see [`Signature`]); of
    /// others, those given by position, but for strings before the first
    /// content given so, where functions take their settings
    /// (`#badge("#f00")[hot]`), and those given by name it's taken to
    /// show (see [`Reader::shown_by_name`]). Each comes with its name, if
    /// it's given by name.
    fn custom_shown_args<'a>(
        &self,
        args: &Args<'a>,
        signature: Option<&Signature>,
    ) -> Vec<(Option<&'a str>, Expr<'a>)> {
        let first_content = args
            .items
            .iter()
            .position(|&(name, expr)| name.is_none() && self.is_content(expr));
        // How many it's given by position, unless some are spread from what
        // the reader can't tell, past which it can't tell their places.
        let count = args.spread.is_none().then(|| args.positional().count());
        let mut shown = Vec::new();
        let mut positional_index = 0;
        for (item_index, &(name, expr)) in args.items.iter().enumerate() {
            let string = matches!(expr, Expr::Str(_));
            let showable = string || self.is_content(expr);
            let shows = match (signature, name) {
                (Some(signature), Some(name)) => signature.shows_named(name).admits(string),
                (Some(signature), None) => {
                    // Past a spread it can't tell, it may go to any parameter.
                    let past_spread = args.spread.is_some_and(|spread| item_index >= spread);
                    let shows = if past_spread {
                        signature.shows_any()
                    } else {
                        signature.shows(positional_index, count)
                    };
                    shows.admits(string)
                }
                (None, Some(name)) => self.shown_by_name(name, expr),
                (None, None) => !string || first_content.is_none_or(|first| item_index > first),
            };
            if name.is_none() {
                positional_index += 1;
            }
            if showable && shows {
                shown.push((name, expr));
            }
        }
        shown
    }

    /// Whether the argument `expr` may be content: `[…]`, raw text, an
    /// equation, a symbol of typst's own, or a call to a function that may
    /// make content (one of typst's own whose result may be content or
    /// text, or any other but one returning a setting, [`Name::Setting`],
    /// or a method of a value typst makes, such as a color's
    /// `black.lighten(20%)`).
    fn is_content(&self, expr: Expr) -> bool {
        match expr {
            Expr::ContentBlock(_) | Expr::Raw(_) | Expr::Equation(_) => true,
            Expr::Parenthesized(inner) => self.is_content(inner.expr()),
            // A symbol of typst's own, which shows its text (`sym.arrow`).
            Expr::Ident(_) | Expr::FieldAccess(_) => matches!(
                self.resolve(expr),
                Name::Function(path) if std_symbol_text(&path).is_some()
            ),
            Expr::FuncCall(call) => match self.resolve(call.callee()) {
                // A name of typst's own that's no function is a method of
                // one of its values (`black.lighten`).
                Name::Function(name) => {
                    std_function(&name).is_some_and(|func| func.returns().is_none_or(makes_content))
                }
                Name::Setting | Name::Value => false,
                _ => true,
            },
            _ => false,
        }
    }

    /// Whether a function the reader can't see shows `expr`, given to it
    /// by `name`: anything that may be content or text (see
    /// [`Reader::is_content`]) by a name for what it shows
    /// ([`SHOWN_NAMES`]), and else only content the reader can tell is
    /// some (`footer: [Foot]`, `note: emph[n]`). By any other name, it
    /// takes a string, or what a function the reader can't tell either
    /// returns, for a setting (`midrule(color: "gray")`, `toprule(stroke:
    /// thick())`, `badge(fill: theme.color("primary"))[hot]`).
    fn shown_by_name(&self, name: &str, expr: Expr) -> bool {
        if SHOWN_NAMES.contains(&name) {
            return matches!(expr, Expr::Str(_)) || self.is_content(expr);
        }
        let tells = match unparenthesized(expr) {
            Expr::FuncCall(call) => {
                let (_, callee) = Presets::of(self.resolve(call.callee()));
                !matches!(callee, Name::Unknown | Name::Opaque)
            }
            _ => true,
        };
        tells && self.is_content(expr)
    }

    /// Whether a function returning `body` may show something: anything
    /// but a value that's neither content nor text, as far as the reader
    /// can tell without running code (`1em`, `top + right`, `rgb(x)`, a
    /// value of typst's own such as `center`, or a code block ending in
    /// one). `locals` are the names bound in the function (see [`Locals`]):
    /// each value is looked at once, where it's bound, so a chain of values
    /// each using the one before twice (`let b = a + a`) takes no longer
    /// than it is long. Past [`MAX_BODY_DEPTH`] levels `depth`, anything
    /// may show something.
    fn may_show<'a>(&self, body: Expr<'a>, locals: &mut Locals<'a>, depth: usize) -> bool {
        if depth > MAX_BODY_DEPTH {
            return true;
        }
        let depth = depth + 1;
        match body {
            Expr::None(_)
            | Expr::Auto(_)
            | Expr::Bool(_)
            | Expr::Int(_)
            | Expr::Float(_)
            | Expr::Numeric(_) => false,
            Expr::Parenthesized(inner) => self.may_show(inner.expr(), locals, depth),
            Expr::Unary(unary) => self.may_show(unary.expr(), locals, depth),
            // A chain of operators (`a + b + c`) nests to the left: it's
            // read along without a level of the stack each.
            Expr::Binary(mut binary) => loop {
                if self.may_show(binary.rhs(), locals, depth) {
                    return true;
                }
                match binary.lhs() {
                    Expr::Binary(lhs) => binary = lhs,
                    lhs => return self.may_show(lhs, locals, depth),
                }
            },
            Expr::FuncCall(_) => self.is_content(body),
            // Bound in the function: to what it was bound to then.
            Expr::Ident(ident) => locals
                .get(ident.as_str())
                .unwrap_or_else(|| self.std_value_shows(body)),
            Expr::FieldAccess(_) => self.std_value_shows(body),
            Expr::CodeBlock(block) => {
                let mut bound = Vec::new();
                let mut shows = false;
                for expr in block.body().exprs() {
                    match expr {
                        Expr::LetBinding(binding) => match binding.kind() {
                            ast::LetBindingKind::Normal(ast::Pattern::Normal(Expr::Ident(
                                name,
                            ))) => {
                                let value = binding
                                    .init()
                                    .is_none_or(|value| self.may_show(value, locals, depth));
                                locals.bind(name.as_str(), value);
                                bound.push(name.as_str());
                            }
                            kind => {
                                for name in kind.bindings() {
                                    locals.bind(name.as_str(), true);
                                    bound.push(name.as_str());
                                }
                            }
                        },
                        // Once something may show, the rest can't change
                        // that.
                        expr if self.may_show(expr, locals, depth) => {
                            shows = true;
                            break;
                        }
                        _ => {}
                    }
                }
                locals.unbind(&bound);
                shows
            }
            Expr::Conditional(conditional) => {
                self.may_show(conditional.if_body(), locals, depth)
                    || conditional
                        .else_body()
                        .is_some_and(|body| self.may_show(body, locals, depth))
            }
            _ => true,
        }
    }

    /// Whether the name `expr` may show something: unless it's a value of
    /// typst's own that's neither content nor text (`center`, `red`).
    fn std_value_shows(&self, expr: Expr) -> bool {
        match self.resolve(expr) {
            Name::Function(name) => std_value(&name).is_none_or(|value| {
                matches!(value, Value::Content(_) | Value::Str(_) | Value::Symbol(_))
            }),
            _ => true,
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
            Expr::Equation(eq) if eq.block() => {
                Item::blocks(vec![Block::Math(typst_math::equation(eq))])
            }
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

    /// `#table(columns: …, align: …, table.header(…), [cell], …,
    /// table.footer(…))`, without its lines; `None` if it has no cells. As
    /// typst lays it out, a header or footer fills rows of its own, and only
    /// a header that opens the table heads it.
    fn table(&mut self, args: &Args) -> Option<Table> {
        let mut pieces = Vec::new();
        for expr in args.positional() {
            let expr = unparenthesized(expr);
            match self.table_part(expr) {
                Some("header") => pieces.push(Piece::Header(self.section(expr, args))),
                Some("footer") => pieces.push(Piece::Footer(self.section(expr, args))),
                Some("hline" | "vline") => {}
                _ => pieces.extend(self.cells(expr, args).into_iter().map(Piece::Cell)),
            }
        }
        let columns = match args.named("columns") {
            // typst's default, `()`, is one column, as is one track size.
            None | Some(Expr::Auto(_) | Expr::Numeric(_)) => 1,
            Some(Expr::Int(n)) => usize::try_from(n.get()).unwrap_or(1),
            Some(expr) => match array_items(expr) {
                Some(tracks) => tracks.len(),
                // A value the reader can't tell (`columns: cols`): as wide
                // as an opening header.
                None => match pieces.first() {
                    Some(Piece::Header(cells)) => {
                        cells.iter().map(|cell| cell.colspan.min(MAX_COLUMNS)).sum()
                    }
                    _ => 0,
                },
            },
        }
        .clamp(1, MAX_COLUMNS);
        let mut align: Vec<Align> = match args.named("align") {
            Some(expr) => match (array_items(expr), expr) {
                (Some(items), _) => items.into_iter().map(alignment).collect(),
                // An array spreading a value the reader can't tell: the
                // alignments it can.
                (None, Expr::Array(array)) => array
                    .items()
                    .map(|item| match item {
                        ast::ArrayItem::Pos(e) => alignment(e),
                        ast::ArrayItem::Spread(_) => Align::Default,
                    })
                    .collect(),
                (None, single) => vec![alignment(single); columns],
            },
            None => Vec::new(),
        };
        align.resize(columns, Align::Default);

        let mut pieces = pieces.into_iter().peekable();
        let mut header_rows = match pieces.next_if(|p| matches!(p, Piece::Header(_))) {
            Some(Piece::Header(cells)) => grid_rows(cells, columns, &mut self.grown_cells_left),
            _ => Vec::new(),
        }
        .into_iter();
        let mut header = header_rows.next().unwrap_or_default();
        let mut rows: Vec<_> = header_rows.collect();
        let mut body = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Cell(cell) => body.push(cell),
                Piece::Header(cells) | Piece::Footer(cells) => {
                    let body_so_far = std::mem::take(&mut body);
                    rows.extend(grid_rows(body_so_far, columns, &mut self.grown_cells_left));
                    rows.extend(grid_rows(cells, columns, &mut self.grown_cells_left));
                }
            }
        }
        rows.extend(grid_rows(body, columns, &mut self.grown_cells_left));
        if header.is_empty() && rows.is_empty() {
            return None;
        }
        // Columns empty in every row at the table's end (a form's column to
        // fill in, or a table given fewer cells than columns) are as many
        // cells grown; past what's left to grow by, they're left out.
        let empty = |x: usize| {
            header.get(x).is_none_or(Vec::is_empty) && rows.iter().all(|row| row[x].is_empty())
        };
        let empty_columns = (1..columns).rev().take_while(|&x| empty(x)).count();
        let cost = empty_columns * (rows.len() + 1);
        match self.grown_cells_left.checked_sub(cost) {
            Some(left) => self.grown_cells_left = left,
            None => {
                let kept = columns - empty_columns;
                align.truncate(kept);
                header.truncate(kept);
                rows.iter_mut().for_each(|row| row.truncate(kept));
            }
        }
        Some(Table {
            align,
            header,
            rows,
        })
    }

    /// The cells of a table's header or footer, without its lines: those
    /// of a call, given what it's given among `given` (see
    /// [`Reader::unwrapped`]), or those one made beforehand (`#let th =
    /// table.header(…)`, placed as `th`) was made with.
    fn section<'a>(&mut self, expr: Expr<'a>, given: &Args<'a>) -> Vec<Cell> {
        let Expr::FuncCall(call) = expr else {
            return match self.resolve(expr) {
                Name::Part { cells, .. } => cells.to_vec(),
                _ => Vec::new(),
            };
        };
        let mut held = Vec::new();
        let (_, args) = self.unwrapped(call, given, &mut held);
        let mut cells = Vec::new();
        for expr in args.positional() {
            let expr = unparenthesized(expr);
            if !matches!(self.table_part(expr), Some("hline" | "vline")) {
                cells.extend(self.cells(expr, &args));
            }
        }
        cells
    }

    /// The cells `expr`, among `given`, makes in a table: one, or those of
    /// the part it names, made beforehand (`#let c = table.cell(…)`, placed
    /// as `c`), unless it calls a function the reader can't see
    /// ([`Name::Unknown`]), or a preset of one. That may make cells, a
    /// header or a line, so if what it shows (see
    /// [`Reader::custom_shown_args`]) is all given by position and holds
    /// content, each argument it shows is a cell, as a header's would be,
    /// and no text is lost. Shown anything by name (a box's `title: […]`),
    /// it's one cell of all it shows, as is a call given only strings and
    /// other values. Given nothing but settings by name, it's a line, as
    /// is a name the reader can't see, placed on its own (`toprule`).
    fn cells<'a>(&mut self, expr: Expr<'a>, given: &Args<'a>) -> Vec<Cell> {
        match expr {
            Expr::FuncCall(call) => {
                let (presets, callee) = Presets::of(self.resolve(call.callee()));
                if !matches!(callee, Name::Unknown) {
                    return vec![self.cell(expr, given)];
                }
                let args = presets.give(&Args::new(call.args()));
                let shown = self.custom_shown_args(&args, None);
                if shown.iter().all(|&(name, _)| name.is_none())
                    && shown.iter().any(|&(_, expr)| self.is_content(expr))
                {
                    return shown
                        .into_iter()
                        .map(|(_, expr)| self.cell(expr, given))
                        .collect();
                }
                if shown.is_empty() && args.positional().next().is_none() {
                    return Vec::new();
                }
            }
            Expr::Ident(_) | Expr::FieldAccess(_) => match self.resolve(expr) {
                Name::Part { cells, .. } => return cells.to_vec(),
                Name::Unknown => return Vec::new(),
                _ => {}
            },
            _ => {}
        }
        vec![self.cell(expr, given)]
    }

    /// The cell `expr`, among `given`, makes (see [`Reader::cells`]).
    fn cell<'a>(&mut self, expr: Expr<'a>, given: &Args<'a>) -> Cell {
        // `table.cell(colspan: 2, …)[content]`, by any name. A wrapper may
        // give it more than its body (see `Wrapper::arguments`): it shows
        // all it's given.
        if let Expr::FuncCall(call) = expr
            && self.table_part(expr) == Some("cell")
        {
            let mut held = Vec::new();
            let (_, args) = self.unwrapped(call, given, &mut held);
            let mut content = Vec::new();
            for expr in args.positional() {
                let more = self.inlines_of(expr);
                join_run(&mut content, more);
            }
            return Cell {
                colspan: span(&args, "colspan"),
                rowspan: span(&args, "rowspan"),
                content,
            };
        }
        Cell {
            content: self.inlines_of(expr),
            colspan: 1,
            rowspan: 1,
        }
    }

    /// Which of a table's parts ([`TABLE_PARTS`]) `expr` is, if any: a
    /// call to `table.header`, `grid.hline` and so on, by any name, or a
    /// name for a part made beforehand.
    fn table_part(&self, expr: Expr) -> Option<&'static str> {
        match expr {
            Expr::Parenthesized(inner) => self.table_part(inner.expr()),
            Expr::FuncCall(call) => match self.resolve(call.callee()) {
                Name::Function(name) => table_part(&name),
                Name::Wrapper(wrapper) => table_part(&wrapper.function),
                _ => None,
            },
            Expr::Ident(_) | Expr::FieldAccess(_) => match self.resolve(expr) {
                Name::Part { part, .. } => Some(part),
                _ => None,
            },
            _ => None,
        }
    }

    /// What `callee` stands for, past parentheses: what the names the
    /// document bound make of it, else the standard library's function of
    /// that name (without `std.`). A name that's neither may come from a
    /// wildcard import the reader couldn't read: it's [`Name::Unknown`]. A
    /// method of what's no name is the method of a value typst makes (see
    /// [`Reader::method`]). A preset made with `.with(…)` is what it
    /// presets (see [`Wrapper::preset`]), through a chain of them without a
    /// level of the stack each.
    fn resolve(&self, callee: Expr) -> Name {
        let mut presets = Vec::new();
        let mut callee = callee;
        while let Some((preset, target)) = preset_call(callee) {
            presets.push(preset);
            callee = target;
        }
        // The innermost preset presets what's called first.
        presets
            .into_iter()
            .rev()
            .fold(self.resolve_name(callee), |name, preset| {
                Wrapper::preset(preset, name)
            })
    }

    /// What `callee`, no preset, stands for (see [`Reader::resolve`]).
    fn resolve_name(&self, callee: Expr) -> Name {
        let Some(name) = name_path(callee) else {
            return self.method(callee);
        };
        let mut segments = name.split('.');
        let head = segments.next().unwrap_or_default();
        if let Some(bound) = self.names.get(head) {
            return segments.fold(bound.clone(), Name::member);
        }
        let path = name.strip_prefix("std.").unwrap_or(&name);
        let head = path.split('.').next().unwrap_or_default();
        // `std` itself is the module (see [`Name::member`]).
        if path == "std" || native_typst::library().global.scope().get(head).is_some() {
            Name::Function(path.to_string())
        } else {
            Name::Unknown
        }
    }

    /// What `call` calls (see [`Reader::resolve`]) and the arguments it's
    /// called with there: past the wrappers it calls through (see
    /// [`Wrapper`]), the function of typst's own they end up calling, with
    /// the arguments they give it. It's given what it's given where it's
    /// written, if it's among the calls `given` holds (see
    /// [`Args::calls`]). `held` keeps the wrappers those are read from.
    /// Once what the reader may spend so is spent (see [`MAX_UNWRAPPED`]),
    /// a wrapper is the function it ends up calling, given what it's
    /// given, and one giving more than what's left pays for gives only
    /// what it does.
    fn unwrapped<'a>(
        &mut self,
        call: ast::FuncCall<'a>,
        given: &Args<'a>,
        held: &'a mut Vec<Rc<Wrapper>>,
    ) -> (Name, Args<'a>) {
        let mut callee = self.resolve(call.callee());
        while let Name::Wrapper(wrapper) = callee {
            callee = wrapper.calls.clone();
            held.push(wrapper);
        }
        let mut args = given.of(call);
        let size: usize = held.iter().map(|wrapper| wrapper.node.len()).sum();
        let Some(mut left) = self.unwrapping_left.checked_sub(size) else {
            return (callee, args);
        };
        let held: &'a Vec<Rc<Wrapper>> = held;
        let part = |expr| self.table_part(expr).is_some();
        for wrapper in held {
            if left == 0 {
                break;
            }
            args = wrapper.arguments(args, &mut left, &part);
        }
        self.unwrapping_left = left;
        (callee, args)
    }

    /// What the method `callee`, of what's no name, stands for: of a value
    /// one of typst's own functions makes, through a chain of methods
    /// (`state("x").update`, `str(n).split(",").join`), a method of typst's
    /// own (see [`Name::Value`]). Any other (`"abc".rev`, `make().note`) is
    /// the document's own.
    fn method(&self, callee: Expr) -> Name {
        let mut callee = callee;
        loop {
            let Expr::FieldAccess(access) = unparenthesized(callee) else {
                return Name::Opaque;
            };
            let Expr::FuncCall(call) = unparenthesized(access.target()) else {
                return Name::Opaque;
            };
            // The call making the value: to a name, or a method again.
            callee = call.callee();
            if name_path(callee).is_some() || preset_call(callee).is_some() {
                return match self.resolve(callee) {
                    Name::Function(_) | Name::Value => Name::Value,
                    _ => Name::Opaque,
                };
            }
        }
    }

    /// Whether `callee`, past parentheses and presets, is a method of a
    /// value (`theme.note`): a field of what's neither a module nor a
    /// dictionary, whose fields are the functions they hold (`lib.note`,
    /// `d.note`).
    fn is_method(&self, callee: Expr) -> bool {
        match function_expr(callee) {
            Expr::FieldAccess(access) => !matches!(
                self.resolve(access.target()),
                Name::Module(_) | Name::Dict { .. }
            ),
            _ => false,
        }
    }

    /// The standard library's name for the function `callee` calls, if it's
    /// one of typst's own (see [`Reader::resolve`]).
    fn std_name(&self, callee: Expr) -> Option<String> {
        match self.resolve(callee) {
            Name::Function(name) => Some(name),
            _ => None,
        }
    }

    /// `#import table: header, hline as hl`, `#import grid: *`, `#import
    /// "lib.typ" as lib: theader` and the like: the names it binds stand
    /// for what they import, a file's names where the reader can read the
    /// file (see [`Reader::module`]), and hide what they meant before.
    fn import(&mut self, import: ast::ModuleImport) {
        let source = match import.source() {
            Expr::Str(path) => match self.module(&path.get()) {
                Some(names) => Name::Module(names),
                None => Name::Unknown,
            },
            source => self.resolve(source),
        };
        if let Some(name) = import.new_name() {
            self.names.insert(name.as_str().to_string(), source.clone());
        }
        match import.imports() {
            Some(ast::Imports::Items(items)) => {
                for item in items.iter() {
                    let name = item
                        .path()
                        .iter()
                        .fold(source.clone(), |name, field| name.member(field.as_str()));
                    self.names
                        .insert(item.bound_name().as_str().to_string(), name);
                }
            }
            Some(ast::Imports::Wildcard) => match source {
                Name::Module(names) => {
                    for (name, value) in names.iter() {
                        self.names.insert(name.clone(), value.clone());
                    }
                }
                Name::Function(name) if matches!(name.as_str(), "table" | "grid") => {
                    for part in TABLE_PARTS {
                        let function = Name::Function(format!("{name}.{part}"));
                        self.names.insert(part.to_string(), function);
                    }
                }
                _ => {}
            },
            None => {
                if import.new_name().is_none()
                    && let Ok(name) = import.bare_name()
                {
                    self.names.insert(name.to_string(), source);
                }
            }
        }
    }

    /// The names the typst file at `path`, imported by the file being
    /// read, binds (see [`Reader::names`]): the file is read on its own for
    /// them, once. As typst resolves it, `path` is relative to the file
    /// being read, or to the root if it starts with `/`. `None` for a
    /// package, a file outside the root (or with no root), what isn't a
    /// file (a pipe would never end), one too large or too deep in imports
    /// to read, and one importing itself.
    fn module(&mut self, path: &str) -> Option<Module> {
        let root = self.root.clone()?;
        if path.starts_with('@') {
            return None;
        }
        let file = match path.strip_prefix('/') {
            Some(path) => root.join(path),
            None => self.dir.as_ref()?.join(path),
        };
        let file = file.canonicalize().ok()?;
        if let Some(names) = self.modules.get(&file) {
            // `None` while the file is being read: it imports itself.
            return names.clone();
        }
        let metadata = std::fs::metadata(&file).ok()?;
        if self.depth >= MAX_IMPORT_DEPTH
            || !file.starts_with(&root)
            || !metadata.is_file()
            || metadata.len() > MAX_IMPORT_BYTES
        {
            return None;
        }
        let source = std::fs::read_to_string(&file).ok()?;
        self.modules.insert(file.clone(), None);
        let mut reader = Reader {
            root: Some(root),
            dir: file.parent().map(Path::to_path_buf),
            depth: self.depth + 1,
            // It's read where it's imported, as deep as the import.
            nesting: self.nesting,
            modules: std::mem::take(&mut self.modules),
            unwrapping_left: self.unwrapping_left,
            grown_cells_left: self.grown_cells_left,
            ..Reader::default()
        };
        let parsed = typst::syntax::parse(&source);
        if let Some(markup) = parsed.cast::<ast::Markup>() {
            reader.blocks(markup);
        }
        let names = Rc::new(reader.names.bound);
        self.modules = reader.modules;
        self.unwrapping_left = reader.unwrapping_left;
        self.grown_cells_left = reader.grown_cells_left;
        self.modules.insert(file, Some(names.clone()));
        Some(names)
    }

    /// `#let`: the reader runs no code, but it follows what the names it
    /// binds stand for, where it can tell (see [`Reader::value`] and
    /// [`Reader::std_wrapper`]), in destructuring too (see
    /// [`Reader::bind`]).
    fn let_binding(&mut self, binding: ast::LetBinding) {
        // The value first: it may use the names it's bound to (`#let hline
        // = hline.with(…)`).
        let value = binding
            .init()
            .map_or(Name::Opaque, |value| self.value(value));
        match binding.kind() {
            ast::LetBindingKind::Closure(name) => {
                // A wrapper of a table keeps the arguments it gives, under
                // any name.
                let value = match value {
                    Name::Wrapper(_) => value,
                    value => self
                        .std_wrapper(name.as_str(), binding.init())
                        .unwrap_or(value),
                };
                self.names.insert(name.as_str().to_string(), value);
            }
            ast::LetBindingKind::Normal(pattern) => self.bind(pattern, value),
        }
    }

    /// The function of typst's own a function named `name` and defined as
    /// `init` stands for, if it's named after it and returns a call to it
    /// (see [`returned_call`]): one wrapping it under its own name, as
    /// `#let link(dest, body) = std.link(dest, underline(body))` does,
    /// takes what it takes.
    fn std_wrapper(&self, name: &str, init: Option<Expr>) -> Option<Name> {
        let Some(Expr::Closure(closure)) = init else {
            return None;
        };
        let (call, _) = returned_call(closure.body())?;
        match self.resolve(call.callee()) {
            Name::Function(called) if called == name => Some(Name::Function(called)),
            _ => None,
        }
    }

    /// Bind the names in `pattern` to what `value` stands for.
    fn bind(&mut self, pattern: ast::Pattern, value: Name) {
        match pattern {
            ast::Pattern::Normal(Expr::Ident(name)) => {
                self.names.insert(name.as_str().to_string(), value);
            }
            ast::Pattern::Parenthesized(inner) => self.bind(inner.pattern(), value),
            ast::Pattern::Destructuring(pattern) => self.destructure(pattern, value),
            ast::Pattern::Normal(_) | ast::Pattern::Placeholder(_) => {}
        }
    }

    /// `#let (header, hline) = (table.header, table.hline)`: each item of
    /// `pattern` binds what's at its place in an array `value`, counted
    /// from the end past a `..` (`(a, ..rest, z)`), or what a dictionary
    /// holds under its key (`(header: h)`) or, for a bare name, under that
    /// name. Of a value the reader can't see into, the names stand for what
    /// it can't tell.
    fn destructure(&mut self, pattern: ast::Destructuring, value: Name) {
        if !matches!(value, Name::Array { .. } | Name::Dict { .. }) {
            let unknown = match value {
                Name::Unknown => Name::Unknown,
                Name::Value => Name::Value,
                _ => Name::Opaque,
            };
            for name in pattern.bindings() {
                self.names
                    .insert(name.as_str().to_string(), unknown.clone());
            }
            return;
        }
        let items: Vec<_> = pattern.items().collect();
        let spread = items
            .iter()
            .position(|item| matches!(item, ast::DestructuringItem::Spread(_)));
        for (i, item) in items.iter().enumerate() {
            match *item {
                ast::DestructuringItem::Pattern(pattern) => {
                    let found = match (&value, pattern) {
                        (Name::Array { values, .. }, _) => {
                            let at = match spread {
                                Some(spread) if i > spread => {
                                    values.len().checked_sub(items.len() - i)
                                }
                                _ => Some(i),
                            };
                            at.and_then(|at| values.get(at)).cloned()
                        }
                        (Name::Dict { entries, .. }, ast::Pattern::Normal(Expr::Ident(name))) => {
                            entries.get(name.as_str()).cloned()
                        }
                        _ => None,
                    };
                    self.bind(pattern, found.unwrap_or(Name::Opaque));
                }
                ast::DestructuringItem::Named(named) => {
                    let found = match &value {
                        Name::Dict { entries, .. } => entries.get(named.name().as_str()).cloned(),
                        _ => None,
                    };
                    self.bind(named.pattern(), found.unwrap_or(Name::Opaque));
                }
                ast::DestructuringItem::Spread(spread) => {
                    if let Some(sink) = spread.sink_ident() {
                        self.names.insert(sink.as_str().to_string(), Name::Opaque);
                    }
                }
            }
        }
    }

    /// What the value `expr`, bound by `#let`, stands for (see [`Name`]):
    /// a function of the standard library (`table`) or a preset of one
    /// (`table.hline.with(…)`; see [`Wrapper::preset`]), a function the
    /// document makes (see [`Reader::returned`]), a part
    /// made by a call (`table.hline(…)`), with the cells it's made with, an
    /// array or dictionary of such values, of no more than [`MAX_ARRAY`],
    /// what another of typst's functions makes (see [`Name::Value`]), or a
    /// name for any of them. Anything else is the document's own.
    fn value(&mut self, expr: Expr) -> Name {
        let mut presets = Vec::new();
        let mut expr = expr;
        while let Some((preset, target)) = preset_call(expr) {
            presets.push(preset);
            expr = target;
        }
        let value = self.made(expr);
        presets
            .into_iter()
            .rev()
            .fold(value, |value, preset| Wrapper::preset(preset, value))
    }

    /// What `expr`, no preset, makes (see [`Reader::value`]).
    fn made(&mut self, expr: Expr) -> Name {
        match unparenthesized(expr) {
            Expr::Array(array) => {
                let mut values = Vec::new();
                for item in array.items() {
                    match item {
                        ast::ArrayItem::Pos(expr) => values.push(self.value(expr)),
                        ast::ArrayItem::Spread(spread) => match self.value(spread.expr()) {
                            Name::Array { values: more, .. } => values.extend(more.iter().cloned()),
                            // The places past it the reader can't tell.
                            _ => return Name::Opaque,
                        },
                    }
                    if values.len() > MAX_ARRAY {
                        return Name::Opaque;
                    }
                }
                // A value made of the one before (`#let a = (a,)`, again
                // and again) nests no deeper than `MAX_NESTING`.
                let depth = 1 + values.iter().map(Name::depth).max().unwrap_or(0);
                if depth > MAX_NESTING {
                    return Name::Opaque;
                }
                Name::Array {
                    values: values.into(),
                    depth,
                }
            }
            Expr::Dict(dict) => {
                let mut entries = HashMap::new();
                for item in dict.items() {
                    match item {
                        ast::DictItem::Named(named) => {
                            let value = self.value(named.expr());
                            entries.insert(named.name().as_str().to_string(), value);
                        }
                        ast::DictItem::Keyed(keyed) => {
                            if let Expr::Str(key) = keyed.key() {
                                let value = self.value(keyed.expr());
                                entries.insert(key.get().to_string(), value);
                            }
                        }
                        ast::DictItem::Spread(spread) => {
                            if let Name::Dict { entries: more, .. } = self.value(spread.expr()) {
                                entries.extend(more.iter().map(|(k, v)| (k.clone(), v.clone())));
                            }
                        }
                    }
                    if entries.len() > MAX_ARRAY {
                        return Name::Opaque;
                    }
                }
                let depth = 1 + entries.values().map(Name::depth).max().unwrap_or(0);
                if depth > MAX_NESTING {
                    return Name::Opaque;
                }
                Name::Dict {
                    entries: Rc::new(entries),
                    depth,
                }
            }
            Expr::Closure(closure) => self.returned(closure),
            call @ Expr::FuncCall(inner) => {
                let Some(part) = self.table_part(call) else {
                    return match self.resolve(inner.callee()) {
                        Name::Function(_) | Name::Value => Name::Value,
                        _ => Name::Opaque,
                    };
                };
                let cells = match part {
                    "header" | "footer" => self.section(call, &Args::default()),
                    "cell" => vec![self.cell(call, &Args::default())],
                    // A line.
                    _ => Vec::new(),
                };
                Name::Part {
                    part,
                    cells: cells.into(),
                }
            }
            name @ (Expr::Ident(_) | Expr::FieldAccess(_)) => self.resolve(name),
            _ => Name::Opaque,
        }
    }

    /// What the function `closure` stands for: a wrapper of a table, a grid
    /// or one of their parts, if all it does is call one, or another such
    /// wrapper (see [`returned_call`]), with no rules styling a part, which
    /// typst takes for a cell once styled. Anything else is the document's
    /// own (see [`Reader::defined`]).
    fn returned(&self, closure: ast::Closure) -> Name {
        if let Some((call, styled)) = returned_call(closure.body()) {
            let calls = self.resolve(call.callee());
            if let Some(wrapper) = Wrapper::of(closure.to_untyped(), calls, styled) {
                return wrapper;
            }
        }
        self.defined(closure)
    }

    /// What the function `closure`, of the document's own, stands for: one
    /// returning a setting shows nothing (see [`Reader::may_show`]), and
    /// any other shows the strings and content given for the parameters its
    /// body shows (see [`Reader::shown_names`]).
    fn defined(&self, closure: ast::Closure) -> Name {
        let body = closure.body();
        let mut shown = Shown::default();
        self.shown_names(body.to_untyped(), Shows::All, &mut shown, 0);
        let mut signature = Signature {
            positional: Vec::new(),
            sink: None,
            named: Vec::new(),
        };
        let mut locals = Locals::default();
        for param in closure.params().children() {
            match param {
                ast::Param::Pos(pattern) => {
                    let names = pattern.bindings();
                    let shows = names.iter().map(|name| shown.of(name.as_str())).max();
                    signature.positional.push(shows.unwrap_or(Shows::Nothing));
                    for name in names {
                        locals.bind(name.as_str(), true);
                    }
                }
                ast::Param::Named(named) => {
                    let name = named.name();
                    let shows = shown.of(name.as_str());
                    signature.named.push((name.as_str().to_string(), shows));
                    locals.bind(name.as_str(), true);
                }
                ast::Param::Spread(spread) => {
                    let sink = spread.sink_ident();
                    let shows = match sink {
                        Some(name) => match shown.names.get(name.as_str()) {
                            Some(&shows) => Sink::All(shows),
                            None => shown
                                .picks
                                .get(name.as_str())
                                .map_or(Sink::All(Shows::Nothing), |picks| {
                                    Sink::Picks(picks.clone())
                                }),
                        },
                        None => Sink::All(Shows::Nothing),
                    };
                    signature.sink = Some(shows);
                    if let Some(name) = sink {
                        locals.bind(name.as_str(), true);
                    }
                }
            }
        }
        if self.may_show(body, &mut locals, 0) {
            Name::Defined(Rc::new(signature))
        } else {
            Name::Setting
        }
    }

    /// Add to `shown` what `node`, in the body of a function the document
    /// defines, shows of the names it uses (see [`Signature`]), `shows`
    /// being what's shown of what's there. A call shows what it's given as
    /// the function it calls shows it (see [`Reader::call_arg_shows`]), and no
    /// more, unless it makes no content (`rgb(x)`), as does a set rule,
    /// with what it's given (`set text(lang: x)` shows nothing). Content
    /// and math show all they hold, and names are not shown where they're
    /// compared (`x == "a"`), tested (`if x …`), bound by a `let`, or
    /// fields (`x.title`). Of an argument sink, what it picks by place (see
    /// [`sink_pick`]) is all it uses there. Past [`MAX_BODY_DEPTH`] levels
    /// `depth`, each name used is taken to show (see [`every_name`]).
    fn shown_names<'a>(
        &self,
        node: &'a SyntaxNode,
        shows: Shows,
        shown: &mut Shown<'a>,
        depth: usize,
    ) {
        if depth > MAX_BODY_DEPTH {
            every_name(node, shows, shown);
            return;
        }
        let depth = depth + 1;
        if let Some(Expr::FuncCall(call)) = node.cast::<Expr>()
            && let Some((sink, pick)) = sink_pick(call)
        {
            shown.picks.entry(sink).or_default().extend(pick);
            return;
        }
        match node.cast::<Expr>() {
            Some(Expr::Ident(ident)) => shown.add(ident.as_str(), shows),
            Some(Expr::MathIdent(ident)) => shown.add(ident.as_str(), shows),
            Some(expr @ Expr::FuncCall(call)) => {
                self.shown_names(call.callee().to_untyped(), shows, shown, depth);
                if self.is_content(expr) {
                    self.call_shown_names(call.callee(), call.args(), shown, depth);
                }
            }
            Some(Expr::SetRule(rule)) => {
                self.call_shown_names(rule.target(), rule.args(), shown, depth);
            }
            // A chain of operators (`a + b + c`) nests to the left: it's
            // read along without a level of the stack each.
            Some(Expr::Binary(mut binary)) => loop {
                if is_test(binary.op()) {
                    break;
                }
                self.shown_names(binary.rhs().to_untyped(), shows, shown, depth);
                match binary.lhs() {
                    Expr::Binary(lhs) => binary = lhs,
                    lhs => {
                        self.shown_names(lhs.to_untyped(), shows, shown, depth);
                        break;
                    }
                }
            },
            Some(Expr::Conditional(conditional)) => {
                self.shown_names(conditional.if_body().to_untyped(), shows, shown, depth);
                if let Some(body) = conditional.else_body() {
                    self.shown_names(body.to_untyped(), shows, shown, depth);
                }
            }
            Some(Expr::ContentBlock(_) | Expr::Equation(_)) => {
                for child in node.children() {
                    self.shown_names(child, Shows::All, shown, depth);
                }
            }
            Some(Expr::FieldAccess(access)) => {
                self.shown_names(access.target().to_untyped(), shows, shown, depth);
            }
            Some(Expr::LetBinding(binding)) => {
                if let Some(init) = binding.init() {
                    self.shown_names(init.to_untyped(), shows, shown, depth);
                }
            }
            _ => {
                for child in node.children() {
                    // A dictionary's key, or a name an argument is given
                    // by, is no name used.
                    let child = match child.cast::<ast::Named>() {
                        Some(named) => named.expr().to_untyped(),
                        None => child,
                    };
                    self.shown_names(child, shows, shown, depth);
                }
            }
        }
    }

    /// Add to `shown` what a call to `callee` (or a set rule of it) given
    /// `args`, in a function's body, shows of the names they use (see
    /// [`Reader::call_arg_shows`]).
    fn call_shown_names<'a>(
        &self,
        callee: Expr,
        args: ast::Args<'a>,
        shown: &mut Shown<'a>,
        depth: usize,
    ) {
        let args: Vec<ast::Arg> = args.items().collect();
        let shows = self.call_arg_shows(callee, &args);
        for (arg, shows) in args.into_iter().zip(shows) {
            let value = match arg {
                ast::Arg::Pos(expr) => expr,
                ast::Arg::Named(named) => named.expr(),
                ast::Arg::Spread(spread) => spread.expr(),
            };
            if shows > Shows::Nothing {
                self.shown_names(value.to_untyped(), shows, shown, depth);
            }
        }
    }

    /// What a call to `callee` shows of each of `args`. Of typst's own
    /// functions, all of what goes to a parameter that may show it (see
    /// [`std_targets`]), and nothing else (an angle, a color, an image's
    /// path, a language given by name: `text(lang: …)`). Of the document's
    /// own functions, as their signature has it (see [`Signature`]), and
    /// of a preset, `f.with(…)`, as `f` shows it. A method of one of
    /// typst's values shows nothing (see [`Name::Value`]), and one of what
    /// the reader can't tell content alone, as [`Reader::custom_call`] shows
    /// it. Others show what they're given by position, and content given
    /// by name, a string there taken for a setting but by a name for what
    /// they show ([`SHOWN_NAMES`]).
    fn call_arg_shows(&self, callee: Expr, args: &[ast::Arg]) -> Vec<Shows> {
        let given: Vec<Given> = args
            .iter()
            .map(|arg| match arg {
                ast::Arg::Pos(_) => Given::Positional,
                ast::Arg::Named(named) => Given::Named(named.name().as_str()),
                ast::Arg::Spread(_) => Given::Spread,
            })
            .collect();
        let callee = match unparenthesized(callee) {
            Expr::FieldAccess(access) if access.field().as_str() == "with" => access.target(),
            callee => callee,
        };
        let unknown = |given: &Given| match given {
            Given::Named(name) if SHOWN_NAMES.contains(name) => Shows::All,
            Given::Named(_) => Shows::Content,
            Given::Positional | Given::Spread => Shows::All,
        };
        let (presets, function) = Presets::of(self.resolve(callee));
        match function {
            Name::Function(name) => match std_function(&name) {
                Some(func) => std_targets(&func, &given)
                    .into_iter()
                    .map(|target| {
                        if target.shows() {
                            Shows::All
                        } else {
                            Shows::Nothing
                        }
                    })
                    .collect(),
                // A method of one of typst's values (`black.lighten`).
                None => vec![Shows::Nothing; args.len()],
            },
            Name::Defined(signature) => {
                // A preset gives the first parameters (see `Signature::preset`).
                let signature = if presets.0.is_empty() {
                    signature
                } else {
                    Rc::new(signature.preset(presets.positional()))
                };
                // How many it's given by position, and where, unless some
                // are spread.
                let spread = given.iter().any(|g| matches!(g, Given::Spread));
                let positional = given.iter().filter(|g| matches!(g, Given::Positional));
                let count = (!spread).then(|| positional.count());
                let mut position = Some(0);
                given
                    .iter()
                    .map(|given| match given {
                        Given::Named(name) => signature.shows_named(name),
                        Given::Spread => {
                            position = None;
                            let named = signature.named.iter().map(|&(_, shows)| shows);
                            named.fold(signature.shows_any(), Shows::max)
                        }
                        Given::Positional => match position {
                            Some(n) => {
                                position = Some(n + 1);
                                signature.shows(n, count)
                            }
                            None => signature.shows_any(),
                        },
                    })
                    .collect()
            }
            Name::Setting | Name::Value => vec![Shows::Nothing; args.len()],
            Name::Opaque if self.is_method(callee) => vec![Shows::Content; args.len()],
            _ => given.iter().map(unknown).collect(),
        }
    }

    /// The document, from its `blocks` and the notes read with them, its
    /// marks settled. It takes the reader, so nothing is read after: no
    /// mark is left for the writers to see.
    ///
    /// A bare reference to a note's label becomes the note's mark, and
    /// label links point at the ids the writers will give the headings. A
    /// bare reference to a heading shows the heading's text. A link to a
    /// label on anything else, or on a heading no writer writes (in a note
    /// nothing references), keeps only its text (or the label's name, for
    /// a bare `@label`). Smart quotes are settled last, as they see the
    /// text references show.
    fn finish(mut self, mut blocks: Vec<Block>) -> Document {
        blocks.append(&mut self.notes);
        let mut doc = Document {
            meta: std::mem::take(&mut self.meta),
            blocks,
        };
        // Notes first: the references to them decide which notes are
        // written, and in what order, so which headings get ids, and in
        // what order.
        walk::for_each_inlines(&mut doc.blocks, &mut |run| {
            for inline in run.iter_mut() {
                if let Inline::Link { url, content, .. } = inline
                    && content.is_empty()
                    && let Some(label) = url.strip_prefix(LABEL_LINK)
                    && !self.heading_labels.contains_key(label)
                    && let Some(note) = self.note_labels.get(label)
                {
                    *inline = Inline::FootnoteRef(note.clone());
                }
            }
        });
        // Then the written headings' text, which their ids are made of,
        // with the references in it settled.
        let (_, note_bodies) = Footnotes::collect(&doc);
        let mut texts = HeadingTexts {
            labels: &self.heading_labels,
            unsettled: HashMap::new(),
            settled: HashMap::new(),
            text_left: self.reference_text_left,
        };
        let mut marks = Vec::new();
        write::for_each_heading(&doc.blocks, &note_bodies, &mut |_, content| {
            if let Some(mark) = heading_mark(content) {
                marks.push(mark);
                texts.unsettled.insert(mark, content.to_vec());
            }
        });
        for mark in marks {
            texts.settle(mark, 0);
        }
        walk::for_each_block(&mut doc.blocks, &mut |block| {
            if let Block::Heading { content, .. } = block
                && let Some(mark) = heading_mark(content)
                && let Some(settled) = texts.settled.remove(&mark)
            {
                *content = settled.content;
            }
        });
        let mut text_left = texts.text_left;
        // Id and text by heading mark, met in the order the writers give
        // out ids.
        let (_, note_bodies) = Footnotes::collect(&doc);
        let mut ids = DocumentIds::new(&doc.blocks, &note_bodies);
        let mut headings: HashMap<usize, ReferenceTarget> = HashMap::new();
        write::for_each_heading(&doc.blocks, &note_bodies, &mut |part, content| {
            let id = ids.next(part, content);
            if let Some(mark) = heading_mark(content) {
                let text = reference_text(content);
                let cost = reference_cost(&text);
                headings.insert(mark, ReferenceTarget { id, text, cost });
            }
        });
        walk::for_each_block(&mut doc.blocks, &mut |block| {
            if let Block::Heading { content, .. } = block
                && heading_mark(content).is_some()
            {
                content.pop();
            }
        });
        walk::resolve_links(&mut doc.blocks, LABEL_LINK, &mut |label, content| {
            let heading = self
                .heading_labels
                .get(label)
                .and_then(|id| headings.get(id));
            if let Some(target) = heading {
                // The link and the text it shows, if it has none of its
                // own; past what references may copy, its label's text.
                let shows_text = content.is_empty();
                let cost = target.id.len() + if shows_text { target.cost } else { 0 };
                if spend_on_reference(&mut text_left, cost) {
                    if shows_text {
                        *content = target.text.clone();
                    }
                    return Some(target.id.clone());
                }
            }
            if content.is_empty() {
                *content = vec![Inline::Text(format!("@{label}"))];
            }
            None
        });
        walk::for_each_run(&mut doc.blocks, &mut |run| Quoter::default().run(run));
        doc
    }
}

/// The written headings' content, by their mark ([`HEADING_MARK`]), with
/// the bare references in it settled (see [`HeadingTexts::settle`]).
struct HeadingTexts<'a> {
    /// Heading marks by the typst `<label>` on them.
    labels: &'a HashMap<String, usize>,
    /// Headings not settled yet.
    unsettled: HashMap<usize, Vec<Inline>>,
    /// Settled headings.
    settled: HashMap<usize, SettledHeading>,
    /// What's left of what references may copy, headings' text and links
    /// to them (see [`MAX_REFERENCE_TEXT`]).
    text_left: usize,
}

impl HeadingTexts<'_> {
    /// Heading `mark`, settled first, `level` headings deep. `None` for a
    /// heading that isn't written, is being settled (a reference back to it
    /// from its own text), or is past [`MAX_HEADING_REFERENCES`] levels.
    fn settle(&mut self, mark: usize, level: usize) -> Option<&SettledHeading> {
        if self.settled.contains_key(&mark) {
            return self.settled.get(&mark);
        }
        if level >= MAX_HEADING_REFERENCES {
            return None;
        }
        let mut content = self.unsettled.remove(&mark)?;
        let mut depth = 0;
        // A bare reference shows the text of the heading it names, settled
        // in turn, unless that would show headings more than
        // `MAX_HEADING_REFERENCES` deep; else its label's name.
        walk::for_each_nested(&mut content, &mut |run| {
            for inline in run.iter_mut() {
                if let Inline::Link {
                    url,
                    content: shown,
                    ..
                } = inline
                    && shown.is_empty()
                    && let Some(label) = url.strip_prefix(LABEL_LINK)
                {
                    let text = self.labels.get(label).and_then(|&target| {
                        let settled = self.settle(target, level + 1)?;
                        let (cost, target_depth) = (settled.cost, settled.depth);
                        if level + 1 + target_depth > MAX_HEADING_REFERENCES
                            || !spend_on_reference(&mut self.text_left, cost)
                        {
                            return None;
                        }
                        Some((self.settled[&target].text.clone(), target_depth))
                    });
                    match text {
                        Some((text, target_depth)) => {
                            *shown = text;
                            depth = depth.max(target_depth + 1);
                        }
                        None => *inline = Inline::Text(format!("@{label}")),
                    }
                }
            }
        });
        let text = reference_text(&content);
        let cost = reference_cost(&text);
        let settled = SettledHeading {
            content,
            text,
            cost,
            depth,
        };
        Some(self.settled.entry(mark).or_insert(settled))
    }
}

/// Whether what's `left` of what references may copy pays for `cost`
/// (see [`MAX_REFERENCE_TEXT`]), spending it if so.
fn spend_on_reference(left: &mut usize, cost: usize) -> bool {
    match left.checked_sub(cost) {
        Some(rest) => {
            *left = rest;
            true
        }
        None => false,
    }
}

/// What a reference showing `text`, a heading's, costs of what references
/// may copy: its bytes, and [`REFERENCE_INLINE_COST`] for each inline, those
/// nested in others included.
fn reference_cost(text: &[Inline]) -> usize {
    let mut inlines = 0;
    let mut runs = vec![text];
    while let Some(run) = runs.pop() {
        inlines += run.len();
        for inline in run {
            match inline {
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => runs.push(c),
                _ => {}
            }
        }
    }
    inline_text(text).len() + inlines * REFERENCE_INLINE_COST
}

/// A written heading, as references to it show it (see
/// [`Reader::finish`]).
struct ReferenceTarget {
    /// The id the writers give it.
    id: String,
    /// Its text as a reference shows it (see [`reference_text`]), and
    /// what a reference showing it costs (see [`reference_cost`]).
    text: Vec<Inline>,
    cost: usize,
}

/// A heading's content with the references in it settled (see
/// [`HeadingTexts`]).
struct SettledHeading {
    content: Vec<Inline>,
    /// Its text as a reference shows it (see [`reference_text`]), and
    /// what a reference showing it costs (see [`reference_cost`]).
    text: Vec<Inline>,
    cost: usize,
    /// How many headings deep it shows the text of headings it references.
    depth: usize,
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

/// The id a heading's [`HEADING_MARK`] names, if it has one.
fn heading_mark(content: &[Inline]) -> Option<usize> {
    match content.last() {
        Some(Inline::Link { url, .. }) => url.strip_prefix(HEADING_MARK)?.parse().ok(),
        _ => None,
    }
}

/// The text of `inlines`, their quotes settled as a paragraph of their own.
fn quoted_text(inlines: &[Inline]) -> String {
    let mut run = inlines.to_vec();
    Quoter::default().run(&mut run);
    inline_text(&run)
}

/// What a bare reference to a heading with `content` shows: its text, as
/// [`quoted_text`] has it, but with its math still math, which as text
/// would show its TeX.
fn reference_text(content: &[Inline]) -> Vec<Inline> {
    fn push_shown(out: &mut Vec<Inline>, inlines: &[Inline]) {
        for inline in inlines {
            match inline {
                Inline::Math(tex) => out.push(Inline::Math(tex.clone())),
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => push_shown(out, c),
                other => push_text(out, &inline_text(std::slice::from_ref(other))),
            }
        }
    }
    let mut run = content.to_vec();
    Quoter::default().run(&mut run);
    let mut shown = Vec::new();
    push_shown(&mut shown, &run);
    shown
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
            Block::Math(tex) => vec![Inline::Math(tex)],
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
        if !out.is_empty() {
            push_text(&mut out, " ");
        }
        out.extend(content);
    }
    out
}

/// The call to `.with` `expr` is, past parentheses, if it's one, and the
/// function it presets: `table.hline` for `(table.hline.with(stroke:
/// 2pt))`.
fn preset_call(expr: Expr) -> Option<(ast::FuncCall, Expr)> {
    let Expr::FuncCall(call) = unparenthesized(expr) else {
        return None;
    };
    match call.callee() {
        Expr::FieldAccess(access) if access.field().as_str() == "with" => {
            Some((call, access.target()))
        }
        _ => None,
    }
}

/// The function `expr` names, past parentheses and `.with(…)` presets:
/// `table.hline` for `(table.hline.with(stroke: 2pt))`.
fn function_expr(expr: Expr) -> Expr {
    let mut expr = expr;
    loop {
        expr = match expr {
            Expr::Parenthesized(inner) => inner.expr(),
            Expr::FuncCall(call) => match call.callee() {
                Expr::FieldAccess(access) if access.field().as_str() == "with" => access.target(),
                _ => return expr,
            },
            _ => return expr,
        };
    }
}

/// Whether the operator `op` compares or tests what it's given, showing
/// none of it (`x == "a"`, `a and b`).
fn is_test(op: ast::BinOp) -> bool {
    matches!(
        op,
        ast::BinOp::Eq
            | ast::BinOp::Neq
            | ast::BinOp::Lt
            | ast::BinOp::Leq
            | ast::BinOp::Gt
            | ast::BinOp::Geq
            | ast::BinOp::And
            | ast::BinOp::Or
            | ast::BinOp::In
            | ast::BinOp::NotIn
    )
}

/// Add each name `node` uses to `shown`, as showing `shows`, for where it's
/// too deep to read what it shows (see [`Reader::shown_names`]): the nodes
/// are walked without a level of the stack each.
fn every_name<'a>(node: &'a SyntaxNode, shows: Shows, shown: &mut Shown<'a>) {
    let mut nodes = vec![node];
    while let Some(node) = nodes.pop() {
        match node.cast::<Expr>() {
            Some(Expr::Ident(ident)) => shown.add(ident.as_str(), shows),
            Some(Expr::MathIdent(ident)) => shown.add(ident.as_str(), shows),
            _ => nodes.extend(node.children()),
        }
    }
}

/// The name of the argument sink `call` picks a positional argument of,
/// and the place it picks (see [`Sink::Picks`]), if it's
/// `args.pos().first()`, `.last()` or `.at(n)`, or none for their count,
/// `args.pos().len()`.
fn sink_pick(call: ast::FuncCall<'_>) -> Option<(&str, Option<i64>)> {
    let Expr::FieldAccess(method) = call.callee() else {
        return None;
    };
    let Expr::FuncCall(positional) = method.target() else {
        return None;
    };
    let Expr::FieldAccess(access) = positional.callee() else {
        return None;
    };
    let Expr::Ident(sink) = access.target() else {
        return None;
    };
    if access.field().as_str() != "pos" || positional.args().items().next().is_some() {
        return None;
    }
    let args = Args::new(call.args());
    let mut given = args.positional();
    let pick = match (method.field().as_str(), given.next(), given.next()) {
        ("first", None, _) => Some(0),
        ("last", None, _) => Some(-1),
        ("at", Some(Expr::Int(n)), None) => Some(n.get()),
        ("len", None, _) => None,
        _ => return None,
    };
    Some((sink.as_str(), pick))
}

/// The call a function's `body` returns, when that's all it does: `f(…)`,
/// `[#f(…)]` or `{ f(…) }`, after `let`s (`{ let c = …; f(..c) }`) and
/// `set` and `show` rules, and whether such rules style it.
fn returned_call(body: Expr) -> Option<(ast::FuncCall, bool)> {
    /// The one expression of `exprs` that isn't a space, a `let` or a
    /// rule, and whether a rule was among them.
    fn only<'a>(exprs: impl Iterator<Item = Expr<'a>>) -> Option<(Expr<'a>, bool)> {
        let (mut only, mut styled) = (None, false);
        for expr in exprs {
            match expr {
                Expr::Space(_) | Expr::Parbreak(_) | Expr::LetBinding(_) => {}
                Expr::SetRule(_) | Expr::ShowRule(_) => styled = true,
                expr if only.is_none() => only = Some(expr),
                _ => return None,
            }
        }
        Some((only?, styled))
    }
    let (inner, styled) = match body {
        Expr::FuncCall(call) => return Some((call, false)),
        Expr::Parenthesized(inner) => return returned_call(inner.expr()),
        Expr::CodeBlock(block) => only(block.body().exprs())?,
        Expr::ContentBlock(block) => only(block.body().exprs())?,
        _ => return None,
    };
    let (call, inner_styled) = returned_call(inner)?;
    Some((call, styled || inner_styled))
}

/// Which of a table's parts ([`TABLE_PARTS`]) the function `name` is.
fn table_part(name: &str) -> Option<&'static str> {
    let (parent, part) = name.split_once('.')?;
    if !matches!(parent, "table" | "grid") {
        return None;
    }
    TABLE_PARTS.iter().copied().find(|p| *p == part)
}

/// `cells` in rows of `columns`, as typst places them: each in the next
/// slot no cell before it covers, with the other slots a cell spans left
/// empty. Every row is full. A span past the last column or into a
/// covered slot, which typst refuses, covers the free slots it reaches.
/// Each row a span adds spends `columns` cells of `grown_cells_left`, and
/// each slot it covers in the cell's own row one; once that's spent, spans
/// add no rows and cover one column (see [`MAX_GROWN_CELLS`]).
fn grid_rows(
    cells: Vec<Cell>,
    columns: usize,
    grown_cells_left: &mut usize,
) -> Vec<Vec<Vec<Inline>>> {
    // A slot is `None` until a cell covers it.
    let mut grid: Vec<Vec<Option<Vec<Inline>>>> = Vec::new();
    let free = |grid: &[Vec<Option<Vec<Inline>>>], y: usize, x: usize| {
        grid.get(y).is_none_or(|row| row[x].is_none())
    };
    let (mut y, mut x) = (0, 0);
    for cell in cells {
        loop {
            if x == columns {
                (y, x) = (y + 1, 0);
            }
            if free(&grid, y, x) {
                break;
            }
            x += 1;
        }
        let width = (x..columns).take_while(|&x| free(&grid, y, x)).count();
        let colspan = cell.colspan.min(width);
        let colspan = match grown_cells_left.checked_sub(colspan - 1) {
            Some(left) => {
                *grown_cells_left = left;
                colspan
            }
            None => 1,
        };
        // The cell's own row, and the rows past the grid's that its span
        // adds as far as they're paid for.
        let rows = grid.len().max(y + 1);
        let bottom = y
            .saturating_add(cell.rowspan)
            .min(rows + *grown_cells_left / columns)
            .max(y + 1);
        *grown_cells_left -= bottom.saturating_sub(rows) * columns;
        if grid.len() < bottom {
            grid.resize(bottom, vec![None; columns]);
        }
        for row in &mut grid[y..bottom] {
            for slot in &mut row[x..x + colspan] {
                slot.get_or_insert_with(Vec::new);
            }
        }
        grid[y][x] = Some(cell.content);
        x += colspan;
    }
    grid.into_iter()
        .map(|row| row.into_iter().map(Option::unwrap_or_default).collect())
        .collect()
}

/// The items of the array `expr` makes, if the reader can tell them
/// without running code: `(auto, 1fr)`, `(auto,) * 3`, `3 * (auto,)` or
/// `(auto,) * 2 + (1fr,)`, through parentheses and spreads of such
/// arrays. No more than [`MAX_COLUMNS`] are kept. A chain of operators
/// nests as deep as it's long (`a + b + c` is `(a + b) + c`), and typst
/// parses one of any length, so its spine is walked along, not down the
/// stack.
fn array_items(expr: Expr) -> Option<Vec<Expr>> {
    // What the chain's operators do to the array it starts with, the
    // outermost first.
    let mut steps = Vec::new();
    let mut expr = expr;
    let mut items = loop {
        expr = match expr {
            Expr::Binary(binary) => match (binary.op(), binary.lhs(), binary.rhs()) {
                (ast::BinOp::Add, lhs, rhs) => {
                    steps.push(ArrayStep::Add(rhs));
                    lhs
                }
                (ast::BinOp::Mul, Expr::Int(times), array)
                | (ast::BinOp::Mul, array, Expr::Int(times)) => {
                    steps.push(ArrayStep::Repeat(times.get()));
                    array
                }
                _ => return None,
            },
            Expr::Parenthesized(inner) => break array_items(inner.expr())?,
            Expr::Array(array) => break array_literal_items(array)?,
            _ => return None,
        };
    };
    for step in steps.into_iter().rev() {
        match step {
            ArrayStep::Add(array) => items.extend(array_items(array)?),
            ArrayStep::Repeat(times) => {
                let times = usize::try_from(times).ok()?;
                let len = items.len().saturating_mul(times).min(MAX_COLUMNS);
                items = items.into_iter().cycle().take(len).collect();
            }
        }
        items.truncate(MAX_COLUMNS);
    }
    items.truncate(MAX_COLUMNS);
    Some(items)
}

/// What an operator in a chain does to the array before it (see
/// [`array_items`]).
enum ArrayStep<'a> {
    /// Adds the items of this array after its own.
    Add(Expr<'a>),
    /// Repeats its items this many times.
    Repeat(i64),
}

/// The items of the array `array` writes out, its spreads' included (see
/// [`array_items`]).
fn array_literal_items(array: ast::Array) -> Option<Vec<Expr>> {
    let mut items = Vec::new();
    for item in array.items() {
        match item {
            ast::ArrayItem::Pos(item) => items.push(item),
            ast::ArrayItem::Spread(spread) => items.extend(array_items(spread.expr())?),
        }
    }
    Some(items)
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
    /// its parameters (`table.header(title, [B])`; see [`Bound::give`]),
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

/// The name `expr` is written as, past parentheses (`table.header` for
/// `(table).header`), if it's a name: an identifier, or a field of a name,
/// however many fields deep.
fn name_path(expr: Expr) -> Option<String> {
    let mut fields = Vec::new();
    let mut expr = expr;
    let head = loop {
        match expr {
            Expr::Ident(ident) => break ident.as_str(),
            Expr::Parenthesized(inner) => expr = inner.expr(),
            Expr::FieldAccess(access) => {
                fields.push(access.field().as_str());
                expr = access.target();
            }
            _ => return None,
        }
    };
    let mut path = head.to_string();
    for field in fields.iter().rev() {
        path.push('.');
        path.push_str(field);
    }
    Some(path)
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

/// `left`, `center`, `right` (also within `left + horizon`).
fn alignment(expr: Expr) -> Align {
    let text = expr.to_untyped().full_text();
    if text.contains("left") || text.contains("start") {
        Align::Left
    } else if text.contains("center") {
        Align::Center
    } else if text.contains("right") || text.contains("end") {
        Align::Right
    } else {
        Align::Default
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

/// The columns or rows (`name`) a `table.cell(…)` call given `args` spans:
/// one unless it says.
fn span(args: &Args, name: &str) -> usize {
    match args.named(name) {
        Some(Expr::Int(n)) => usize::try_from(n.get()).map_or(1, |n| n.max(1)),
        _ => 1,
    }
}

/// The standard library's function `name` (`text`, `color.rgb`), a type's
/// constructor included (`str`).
fn std_function(name: &str) -> Option<Func> {
    match std_value(name)? {
        Value::Func(func) => Some(func.clone()),
        Value::Type(ty) => ty.constructor().ok(),
        _ => None,
    }
}

/// The standard library's value `name` (`center`, `text`, `color.rgb`).
fn std_value(name: &str) -> Option<&'static Value> {
    let mut segments = name.split('.');
    let mut value = native_typst::library()
        .global
        .scope()
        .get(segments.next()?)?
        .read();
    for segment in segments {
        let scope = match value {
            Value::Func(func) => func.scope()?,
            Value::Type(ty) => ty.scope(),
            Value::Module(module) => module.scope(),
            _ => return None,
        };
        value = scope.get(segment)?.read();
    }
    Some(value)
}

/// The text of the symbol of typst's own `path` names, modifiers included
/// (`sym.arrow.r`, `emoji.rocket`), if it names one.
fn std_symbol_text(path: &str) -> Option<String> {
    let mut parts = path.split('.');
    let mut value = native_typst::library()
        .global
        .scope()
        .get(parts.next()?)?
        .read();
    // A module of symbols, like `sym`, has them by name.
    while let Value::Module(module) = value {
        value = module.scope().get(parts.next()?)?.read();
    }
    let Value::Symbol(symbol) = value else {
        return None;
    };
    let symbol = parts.try_fold(symbol.clone(), |symbol, modifier| {
        symbol.modified((), modifier).ok()
    })?;
    Some(symbol.get().to_string())
}

/// Whether a value `info` describes may be of type `ty`.
fn may_be(info: &CastInfo, ty: Type) -> bool {
    match info {
        CastInfo::Any => true,
        CastInfo::Value(value, _) => value.ty() == ty,
        CastInfo::Type(t) => *t == ty,
        CastInfo::Union(infos) => infos.iter().any(|info| may_be(info, ty)),
    }
}

/// Whether `info` lists the type `ty` itself: not only some of its values
/// (`"binary"`, of `math.class`'s classes), or anything.
fn lists(info: &CastInfo, ty: Type) -> bool {
    match info {
        CastInfo::Type(t) => *t == ty,
        CastInfo::Union(infos) => infos.iter().any(|info| lists(info, ty)),
        CastInfo::Any | CastInfo::Value(..) => false,
    }
}

/// Whether a function returning what `info` describes may show something:
/// content or text (a string, a symbol, or a number, which typst shows as
/// its digits: `int("12")`).
fn makes_content(info: &CastInfo) -> bool {
    [
        Type::of::<Content>(),
        Type::of::<Str>(),
        Type::of::<Symbol>(),
        Type::of::<i64>(),
        Type::of::<f64>(),
        Type::of::<Decimal>(),
    ]
    .into_iter()
    .any(|ty| may_be(info, ty))
}

/// Whether what's given for `param`, a parameter of one of typst's
/// functions, is shown as content: it may be content, or, given by
/// position, an array of it (`math.mat((a, b))`), unless that's of paths.
fn takes_content(param: &NativeParamInfo) -> bool {
    may_be(&param.input, Type::of::<Content>())
        || (param.positional
            && may_be(&param.input, Type::of::<Array>())
            && !may_be(&param.input, Type::of::<RootedPath>()))
}

/// Whether `param`, the parameter one of typst's functions lists first,
/// shows a string given to it as text, though it may take no content: as
/// the text it shows (`text`, `raw`), a link's destination, or what a value
/// made of it shows (`str("x")`, `int("12")`, `str.trim(s)`). Not one
/// taking the string for a path or a pattern, which may be a function
/// (`read("a.txt")`, `numbering("1.", n)`), or for one of the settings it
/// names (`math.class("binary", …)`).
fn shows_its_string(param: &NativeParamInfo) -> bool {
    lists(&param.input, Type::of::<Str>())
        && !may_be(&param.input, Type::of::<Func>())
        && !may_be(&param.input, Type::of::<RootedPath>())
}

/// Where each argument of a call to `func`, one of typst's own functions,
/// given as `given` has them, goes, as typst matches them to its
/// parameters (see [`Target`]): by name, or in turn by position, the last
/// parameter taking all past it if it's variadic. A parameter that may be
/// left out takes the first argument of its type (`align(body)` leaves the
/// alignment out), so given as many arguments short of all of them, an
/// argument may go to any parameter that many past its place. Past those
/// typst lists (`text(red, body)`), or a spread, the reader can't tell.
/// The parameter listed first shows a string given to it (see
/// [`shows_its_string`]) of a function returning what may show (see
/// [`makes_content`]), unless an argument goes to one taking content: a
/// link given its body shows that, not its destination.
fn std_targets(func: &Func, given: &[Given]) -> Vec<Target> {
    let params: Vec<_> = func.params().filter_map(|p| p.to_native()).collect();
    let listed: Vec<_> = params.iter().filter(|p| p.positional).collect();
    let count = given
        .iter()
        .filter(|given| matches!(given, Given::Positional))
        .count();
    let spread = given.iter().any(|given| matches!(given, Given::Spread));
    let slack = if spread {
        listed.len()
    } else {
        listed.len().saturating_sub(count)
    };
    // The parameter at place `n`, with its place in the list.
    let at = |n: usize| match listed.get(n) {
        Some(&param) => Some((n, param)),
        None => listed
            .last()
            .filter(|param| param.variadic)
            .map(|&param| (listed.len() - 1, param)),
    };
    let named = |name: &str| params.iter().find(|param| param.name == name);
    // Each argument's place: how many are given by position before it.
    let places: Vec<usize> = given
        .iter()
        .scan(0, |positional, given| {
            let place = *positional;
            *positional += usize::from(matches!(given, Given::Positional));
            Some(place)
        })
        .collect();
    // Whether an argument goes to a parameter taking content for sure.
    let to_body = given
        .iter()
        .zip(&places)
        .any(|(&given, &place)| match given {
            Given::Named(name) => named(name).is_some_and(|param| takes_content(param)),
            Given::Positional => at(place).is_some_and(|(_, param)| takes_content(param)),
            Given::Spread => false,
        });
    let shows_string = !to_body && func.returns().is_none_or(makes_content);
    given
        .iter()
        .zip(places)
        .map(|(&given, place)| match given {
            Given::Named(name) => Target {
                content: named(name).is_some_and(|param| takes_content(param)),
                string: false,
            },
            Given::Spread => Target::ANY,
            Given::Positional => {
                let mut target = None;
                for (n, param) in (place..=place + slack).filter_map(at) {
                    let target = target.get_or_insert(Target {
                        content: false,
                        string: false,
                    });
                    target.content |= takes_content(param);
                    target.string |= shows_string && n == 0 && shows_its_string(param);
                }
                target.unwrap_or(Target::ANY)
            }
        })
        .collect()
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

/// Typst's smart quotes for one paragraph (its `SmartQuoter`): a quote
/// closes the quotation opened last if it's the same kind and doesn't
/// follow a space or an opening bracket, and opens one otherwise. After a
/// digit it's a prime instead, and a single quote after a letter or an
/// object (math, an image, a box) is an apostrophe. A box's content is a
/// paragraph of its own.
#[derive(Default)]
struct Quoter {
    /// Open quotations, innermost last: `true` for double quotes.
    open: Vec<bool>,
    /// The paragraph's last character so far.
    before: Option<char>,
}

impl Quoter {
    /// Replace the quote marks ([`QUOTE_MARK`]) in `inlines`, which
    /// continue the paragraph read so far, and unwrap its boxes
    /// ([`FRAME_MARK`]).
    fn run(&mut self, inlines: &mut Vec<Inline>) {
        let mut out = Vec::with_capacity(inlines.len());
        for mut inline in std::mem::take(inlines) {
            match &mut inline {
                Inline::Link { url, .. } if url.starts_with(QUOTE_MARK) => {
                    let quote = self.quote(url.ends_with('"'));
                    self.see(quote);
                    push_text(&mut out, &quote.to_string());
                    continue;
                }
                Inline::Link { url, content, .. } if url.starts_with(FRAME_MARK) => {
                    Quoter::default().run(content);
                    self.see('\u{FFFC}');
                    for inline in std::mem::take(content) {
                        push_inline(&mut out, inline);
                    }
                    continue;
                }
                Inline::Text(text) => text.chars().for_each(|c| self.see(c)),
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => self.run(c),
                Inline::Code(code) => code.chars().for_each(|c| self.see(c)),
                Inline::Math(_) | Inline::Image { .. } => self.see('\u{FFFC}'),
                // A note's mark is its number.
                Inline::FootnoteRef(_) => self.see('1'),
                Inline::SoftBreak => self.see(' '),
                Inline::LineBreak => self.see('\n'),
                Inline::Raw { .. } => {}
            }
            push_inline(&mut out, inline);
        }
        *inlines = out;
    }

    fn see(&mut self, c: char) {
        if !typst::text::is_default_ignorable(c) {
            self.before = Some(c);
        }
    }

    fn quote(&mut self, double: bool) -> char {
        let before = self.before.unwrap_or(' ');
        let opened = self.open.last().copied();
        if before.is_numeric() && opened != Some(double) {
            return if double { '″' } else { '′' };
        }
        if !double && opened != Some(false) && (before.is_alphabetic() || before == '\u{FFFC}') {
            return '’';
        }
        if opened == Some(double) && !before.is_whitespace() && !matches!(before, '(' | '[' | '{') {
            self.open.pop();
            return if double { '”' } else { '’' };
        }
        self.open.push(double);
        if double { '“' } else { '‘' }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::ast::ListItem;
    use crate::converter::document::native::read::markdown;

    fn text(s: &str) -> Inline {
        Inline::Text(s.into())
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

    #[test]
    fn unknown_calls_keep_their_content() {
        let doc = read("#custom(width: 3pt)[Visible] #box[boxed] #v(1em)\n");
        assert_eq!(doc.blocks, [Block::Paragraph(vec![text("Visible boxed")])]);
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

    #[test]
    fn headings_in_footnotes_keep_heading_labels_in_step() {
        let doc = read(
            "Text#footnote[= Note heading <nh>]\n\n= Real <real>\n\n\
             See @real and @nh.#footnote[Back to @real.]\n",
        );
        let md = write::markdown::write(&doc);
        assert!(
            md.contains("See [Real](#real) and [Note heading](#note-heading)."),
            "{md}"
        );
        assert!(md.contains("Back to [Real](#real)."), "{md}");
    }

    /// Headings whose slugs share a base, in the text, in a note, in a note
    /// inside a note and in a note that's lost with the title it was in.
    /// Their texts differ only in punctuation, which slugs leave out.
    const SHARED_SLUGS: &str = "#set document(title: [T#footnote[= Dup... <lost>]])\n\n\
        A#footnote[Outer #footnote[= Dup? <inner>]] B#footnote[= Dup! <third>]\n\n\
        = Dup <main>\n\n= Dup. <later>\n\n\
        Refs @main, @inner, @third, @later, @lost and more.\n";

    /// Checks that every link to a heading in `pages` (file name and
    /// HTML) shows the text of the heading it goes to, and counts them.
    fn heading_links(pages: &[(String, String)]) -> usize {
        let select = |s| scraper::Selector::parse(s).unwrap();
        let mut headings = HashMap::new();
        for (file, html) in pages {
            let html = scraper::Html::parse_document(html);
            for h in html.select(&select("h1, h2, h3, h4, h5, h6")) {
                if let Some(id) = h.value().attr("id") {
                    headings.insert(format!("{file}#{id}"), h.text().collect::<String>());
                }
            }
        }
        let mut links = 0;
        for (file, html) in pages {
            let html = scraper::Html::parse_document(html);
            for a in html.select(&select("a[href*='#']")) {
                let href = a.value().attr("href").unwrap();
                let (page, id) = href.split_once('#').unwrap();
                if id.starts_with("fn") {
                    continue;
                }
                let target = format!("{}#{id}", if page.is_empty() { file } else { page });
                let text = a.text().collect::<String>();
                assert_eq!(
                    headings.get(&target),
                    Some(&text),
                    "{href} in {file}: {html:?}"
                );
                links += 1;
            }
        }
        links
    }

    /// [`heading_links`] for LaTeX: `\section[text]…\label{id}` and
    /// `\hyperref[id]{text}`.
    fn latex_heading_links(tex: &str) -> usize {
        let until = |s: &str, end: char| s[..s.find(end).unwrap()].to_string();
        let mut labels = HashMap::new();
        for (at, _) in tex.match_indices("section[") {
            let rest = &tex[at + "section[".len()..];
            let label = &rest[rest.find("\\label{").unwrap() + "\\label{".len()..];
            labels.insert(until(label, '}'), until(rest, ']'));
        }
        let mut links = 0;
        for (at, _) in tex.match_indices("\\hyperref[") {
            let rest = &tex[at + "\\hyperref[".len()..];
            let id = until(rest, ']');
            let text = until(&rest[id.len() + 2..], '}');
            assert_eq!(labels.get(&id), Some(&text), "{id}: {tex}");
            links += 1;
        }
        links
    }

    #[test]
    fn heading_labels_reach_their_heading_in_every_output() {
        let doc = read(SHARED_SLUGS);
        let md = write::markdown::write(&doc);
        assert!(md.contains("[Dup.](#dup-1), @lost and more."), "{md}");
        let html = write::html::write(&doc);
        assert_eq!(heading_links(&[(String::new(), html)]), 4);
        // Markdown ids are the renderer's: read it back to see them.
        let md_html = write::html::write(&markdown::read(&md));
        assert_eq!(heading_links(&[(String::new(), md_html)]), 4);
        assert_eq!(latex_heading_links(&write::latex::write(&doc)), 4);

        let options = write::epub::Options {
            base_dir: std::env::temp_dir(),
        };
        let epub = write::epub::write(&doc, &options).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(epub)).unwrap();
        let chapters: Vec<(String, String)> = (0..zip.len())
            .filter_map(|i| {
                let mut file = zip.by_index(i).unwrap();
                let name = file.name().strip_prefix("OEBPS/text/")?.to_string();
                let mut html = String::new();
                std::io::Read::read_to_string(&mut file, &mut html).unwrap();
                Some((name, html))
            })
            .collect();
        assert_eq!(heading_links(&chapters), 4);
    }

    /// Checks that every link to a heading reaches it in HTML, in Markdown
    /// read back and in LaTeX, and counts them.
    fn checked_heading_links(doc: &Document) -> [usize; 3] {
        let html = write::html::write(doc);
        let md_html = write::html::write(&markdown::read(&write::markdown::write(doc)));
        [
            heading_links(&[(String::new(), html)]),
            heading_links(&[(String::new(), md_html)]),
            latex_heading_links(&write::latex::write(doc)),
        ]
    }

    #[test]
    fn references_to_notes_before_them_keep_heading_labels_in_step() {
        // The writers number notes by their first reference, here before
        // the note itself, and give headings in notes ids in that order.
        for source in [
            "See @fn-a.\n\nX#footnote[= Dup! <b>] Y#footnote[= Dup? <a2>] <fn-a>\n\n\
             Refs @b and @a2.\n",
            "See #footnote(<fn-a>).\n\nX#footnote[= Dup! <b>] Y#footnote[= Dup? <a2>] <fn-a>\n\n\
             Refs @b and @a2.\n",
            // A note in the title, written only because `@fn` names it.
            "#set document(title: [T#footnote[= Dup? <a2>] <fn>])\n\nSee @fn.\n\n\
             X#footnote[= Dup! <b>]\n\nRefs @b and @a2.\n",
        ] {
            assert_eq!(checked_heading_links(&read(source)), [2; 3], "{source}");
        }
    }

    #[test]
    fn headings_holding_references_get_ids_from_their_text() {
        let doc = read("= Intro <intro>\n\n= See @intro <see>\n\nGo to @see.\n");
        let md = write::markdown::write(&doc);
        assert!(md.contains("Go to [See Intro](#see-intro)."), "{md}");
        assert_eq!(checked_heading_links(&doc), [2; 3]);
        // A reference back to a heading being read keeps its label.
        let doc = read("= A @b <a>\n\n= B @a <b>\n\n= Self @self <self>\n\n@a, @b, @self.\n");
        let md = write::markdown::write(&doc);
        assert!(
            md.contains("[A B @a](#a-b-a), [B @a](#b-a), [Self @self](#self-self)."),
            "{md}"
        );
        assert_eq!(checked_heading_links(&doc), [4; 3]);
    }

    /// Regression for #58: a reference to a heading shows the heading's
    /// math as math, as text it would show its TeX, through a heading that
    /// refers to it too.
    #[test]
    fn references_show_a_headings_math_as_math() {
        let doc = read("= Sets $f := g$ and $alpha$ <s>\n\n= See @s <t>\n\nGo to @s and @t.\n");
        let md = write::markdown::write(&doc);
        assert!(
            md.contains(
                "Go to [Sets $f ≔ g$ and $\\alpha$](#sets-f-g-and-alpha) and \
                 [See Sets $f ≔ g$ and $\\alpha$](#see-sets-f-g-and-alpha)."
            ),
            "{md}"
        );
    }

    #[test]
    fn references_resolve_in_every_container() {
        let doc = read(
            "= Target <t>\n\n#quote(block: true)[Quote @t.]\n\n- Item @t.\n\n\
             #table(columns: 1, [Cell @t.])\n\n*Strong _@t emph_*#footnote[Note @t.]\n",
        );
        let md = write::markdown::write(&doc);
        assert_eq!(md.matches("[Target](#target)").count(), 5, "{md}");
        assert!(!md.contains(LABEL_LINK), "{md}");
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
    fn references_may_come_before_the_footnote_they_name() {
        let doc = read("See @fn-a and #footnote(<fn-a>).\n\nText#footnote[Note] <fn-a>\n");
        let md = write::markdown::write(&doc);
        assert!(md.contains("See [^1] and [^1]."), "{md}");
        assert!(md.contains("Text[^1]"), "{md}");
    }

    fn paragraph_texts(doc: &Document) -> Vec<String> {
        doc.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(content) => Some(inline_text(content)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn smart_quotes_follow_typst() {
        // As typst renders it: a quote opens unless it closes the last
        // quotation opened (so the one after `c`, with none open, opens);
        // after a digit (a note's mark too) it's a prime, and after a
        // letter or an object a single quote is an apostrophe. Each
        // paragraph starts afresh.
        let doc = read(
            "#set document(title: [\"Quoted\" title])\n\
             (\"quoted\") ['single'] --\"dash\" \"wait---\" 5' 5\" don't \
             A#footnote[n]'s x $y$'s *\"bold\"* \"a *b\"* c\" \"hi (there)\"\n\n\
             \"Unclosed\n\n\"Fresh\" paragraph.\n\n\"a (\"b\") c\"\n",
        );
        assert_eq!(doc.meta.title.as_deref(), Some("“Quoted” title"));
        assert_eq!(
            paragraph_texts(&doc),
            [
                "(“quoted”) [‘single’] –“dash” “wait—” 5′ 5″ don’t A′s x y’s “bold” “a b” c“ “hi (there)”",
                "“Unclosed",
                "“Fresh” paragraph.",
                // A quotation already open doesn't close after a bracket.
                "“a (“b”) c”",
            ]
        );
    }

    #[test]
    fn boxes_quote_on_their_own_and_transforms_are_blocks() {
        // As typst renders them: a box is laid out on its own, so quotes in
        // it start afresh, and the text around it sees an object there.
        // `rotate`, `move`, `scale`, `skew` and `repeat` are blocks.
        let doc = read(
            "x#box['y'] z\n\n\"a#box[\"x\"]b\"\n\n5#box[\"x\"]\n\n\
             a#box[\"b\"]c#box['d']e\n\n\"a #box[b\"] c\"\n\n\"x #box[(]\" y\n\n\
             #box[5]'s\n\n\"a#rotate(0deg)[\"x\"]b\"\n\n\
             \"a#move(dx: 0pt)[\"x\"]b\" #scale(100%)['y'] #skew(ax: 0deg)[z] #repeat[r]\n",
        );
        assert_eq!(
            paragraph_texts(&doc),
            [
                "x‘y’ z",
                "“a“x”b”",
                "5“x”",
                "a“b”c‘d’e",
                "“a b“ c”",
                "“x (” y",
                "5’s",
                "“a",
                "“x”",
                "b“",
                "“a",
                "“x”",
                "b“",
                "‘y’",
                "z",
                "r",
            ]
        );
    }

    #[test]
    fn control_characters_stay_text() {
        let doc = read("a\u{3}b\u{4}c #\"\\u{3}\\u{4}\"\n");
        assert_eq!(paragraph_texts(&doc), ["a\u{3}b\u{4}c \u{3}\u{4}"]);
    }

    #[test]
    fn only_table_functions_shape_a_table() {
        let doc = read(
            "#table(columns: 2, table.header([H1], [H2]), sectionheader[A], [B], \
             table.hline(), myvline[C], strong[D])\n",
        );
        assert_eq!(
            doc.blocks,
            [Block::Table(Table {
                align: vec![Align::Default; 2],
                header: vec![vec![text("H1")], vec![text("H2")]],
                rows: vec![
                    vec![vec![text("A")], vec![text("B")]],
                    vec![vec![text("C")], vec![Inline::Strong(vec![text("D")])]],
                ],
            })]
        );
    }

    /// A table's header and rows, as text.
    type TableText = (Vec<String>, Vec<Vec<String>>);

    /// The header and rows of each table in `source`, as text.
    fn table_texts(source: &str) -> Vec<TableText> {
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
    fn one_table(rows: &[&[&str]]) -> Vec<TableText> {
        vec![(Vec::new(), rows.iter().map(|r| strings(r)).collect())]
    }

    #[test]
    fn table_parts_are_known_by_every_name_typst_gives_them() {
        let expected = (
            vec!["H1".to_string(), "H2".into()],
            vec![
                vec!["a".to_string(), "b".into()],
                vec!["c".into(), "d".into()],
            ],
        );
        for source in [
            "#import table: header, hline\n\
             #table(columns: 2, header([H1], [H2]), [a], [b], hline(), [c], [d])\n",
            "#import table: header as th, vline as vl\n\
             #table(columns: 2, th([H1], [H2]), [a], vl(x: 1), [b], [c], [d])\n",
            "#import grid: *\n#grid(columns: 2, header([H1], [H2]), [a], [b], hline(), [c], [d])\n",
            "#table(columns: 2, std.table.header([H1], [H2]), [a], [b], std.table.hline(), [c], [d])\n",
            "#import std: table as t\n#t(columns: 2, t.header([H1], [H2]), [a], [b], [c], [d])\n",
            // Wrappers that return a part are that part.
            "#let theader(..cells) = table.header(..cells)\n#let thick() = table.hline(stroke: 2pt)\n\
             #let thin = table.hline\n\
             #table(columns: 2, theader([H1], [H2]), [a], [b], thick(), thin(), [c], [d])\n",
            // Lines inside a header are no cells.
            "#table(columns: 2, table.header(table.hline(), [H1], [H2]), [a], [b], [c], [d])\n",
            // Presets made with `.with` are the part they preset.
            "#let hline = table.hline.with(stroke: .5pt)\n#let vl = table.vline.with(x: 1)\n\
             #let th = table.header.with(repeat: true)\n\
             #table(columns: 2, th([H1], [H2]), [a], vl(), [b], hline(), [c], [d])\n",
            "#import table: hline\n#let hline = hline.with(stroke: 2pt)\n\
             #table(columns: 2, table.header([H1], [H2]), [a], [b], hline(), [c], [d])\n",
            "#table(columns: 2, table.header.with(repeat: true)([H1], [H2]), [a], [b], \
             table.hline.with(stroke: 1pt)(), [c], [d])\n",
            // Wrappers whose body only returns a part.
            "#let th(..c) = { table.header(..c) }\n#let rule() = [#table.hline()]\n\
             #let tv = (..a) => table.vline(..a)\n\
             #table(columns: 2, th([H1], [H2]), [a], tv(x: 1), [b], rule(), [c], [d])\n",
            // The table function under another name.
            "#let t = table\n#t(columns: 2, t.header([H1], [H2]), [a], [b], t.hline(), [c], [d])\n",
            // Lines made once and placed by name, as in booktabs-style tables.
            "#let toprule = table.hline(stroke: .08em)\n\
             #table(columns: 2, toprule, table.header([H1], [H2]), toprule, [a], [b], [c], [d], \
             toprule)\n",
        ] {
            assert_eq!(
                table_texts(source),
                std::slice::from_ref(&expected),
                "{source}"
            );
        }
        // A `let` of the same name hides an imported part.
        assert_eq!(
            table_texts(
                "#import table: header\n#let header(body) = strong(body)\n\
                 #table(columns: 2, header[a], [b])\n"
            ),
            [(Vec::new(), vec![vec!["a".to_string(), "b".into()]])]
        );
        // Names a content block binds are its own: past it, `header` and
        // `hline` mean what they meant before.
        let rows = vec![
            vec!["a".to_string(), "b".into()],
            vec!["c".into(), "d".into()],
        ];
        assert_eq!(
            table_texts(
                "#let header(body) = strong(body)\n#import table: hline\n\
                 #figure[\n  #import table: header\n  #let hline(..args) = none\n\
                   #table(columns: 2, header[H1][H2], [1], [2])\n]\n\
                 #[#import table: header\n]\n\
                 #table(columns: 2, header[a], [b], hline(), [c], [d])\n"
            ),
            [
                (
                    vec!["H1".to_string(), "H2".into()],
                    vec![vec!["1".to_string(), "2".into()]]
                ),
                (Vec::new(), rows),
            ]
        );
        // The footer's cells are the last row.
        assert_eq!(
            table_texts("#table(columns: 2, [a], [b], table.footer([F1], [F2]))\n"),
            [(
                Vec::new(),
                vec![
                    vec!["a".to_string(), "b".into()],
                    vec!["F1".into(), "F2".into()]
                ]
            )]
        );
    }

    #[test]
    fn wrappers_of_table_parts_keep_the_arguments_they_add() {
        let table = |header: &[&str], rows: &[&[&str]]| {
            (
                strings(header),
                rows.iter().map(|r| strings(r)).collect::<Vec<_>>(),
            )
        };
        let cases: &[(&str, TableText)] = &[
            (
                "#let head() = table.header([Name], [Age])\n\
                 #table(columns: 2, head(), [a], [b])\n",
                table(&["Name", "Age"], &[&["a", "b"]]),
            ),
            // Made beforehand, as a part placed by name.
            (
                "#let head() = table.header([Name], [Age])\n#let h = head()\n\
                 #table(columns: 2, h, [a], [b])\n",
                table(&["Name", "Age"], &[&["a", "b"]]),
            ),
            // The caller's arguments go where the wrapper spreads them, as
            // the arguments of a wrapper of it do.
            (
                "#let numhead(..c) = table.header([\\#], ..c)\n\
                 #table(columns: 3, numhead([X], [Y]), [a], [b], [c])\n",
                table(&["#", "X", "Y"], &[&["a", "b", "c"]]),
            ),
            (
                "#let numhead(..c) = table.header([\\#], ..c)\n\
                 #let numhead0(..c) = numhead([0], ..c)\n\
                 #table(columns: 3, numhead0([X]), [a], [b], [c])\n",
                table(&["#", "0", "X"], &[&["a", "b", "c"]]),
            ),
            (
                "#let total() = table.cell(colspan: 2)[Total]\n\
                 #table(columns: 2, [a], [b], total())\n",
                table(&[], &[&["a", "b"], &["Total", ""]]),
            ),
            // Parameters by place and by name, their defaults, and named
            // arguments given after others, which typst takes over them.
            (
                "#let wide(body, span: 2) = table.cell(colspan: span, body)\n\
                 #let one(..c) = table.cell(..c, colspan: 1)\n\
                 #table(columns: 3, wide[x], [y], wide(span: 3)[z], one(colspan: 2)[q], [r], [s])\n",
                table(&[], &[&["x", "", "y"], &["z", "", ""], &["q", "r", "s"]]),
            ),
            (
                "#let hd(a, b) = table.header(b, a)\n#table(columns: 2, hd[A][B], [a], [b])\n",
                table(&["B", "A"], &[&["a", "b"]]),
            ),
            (
                "#let mytable(..c) = table(columns: 2, ..c)\n\
                 #mytable(table.header([M1], [M2]), [a], [b])\n",
                table(&["M1", "M2"], &[&["a", "b"]]),
            ),
            (
                "#let mytable(..c) = table(columns: 2, ..c)\n\
                 #mytable(columns: 3, [a], [b], [c])\n",
                table(&[], &[&["a", "b", "c"]]),
            ),
            // Under the name of typst's own.
            (
                "#let table(..args) = std.table(columns: 2, ..args)\n\
                 #table([a], [b], [c], [d])\n",
                table(&[], &[&["a", "b"], &["c", "d"]]),
            ),
            // Presets give what they preset first.
            (
                "#let wide = table.cell.with(colspan: 2)\n#let hd = table.header.with([\\#])\n\
                 #table(columns: 3, hd([X], [Y]), wide[x], [a])\n",
                table(&["#", "X", "Y"], &[&["x", "", "a"]]),
            ),
            (
                "#table(columns: 3, table.cell.with(colspan: 2).with(rowspan: 2)()[big], \
                 [a], [b], [c], [d])\n",
                table(&[], &[&["big", "", "a"], &["", "", "b"], &["c", "d", ""]]),
            ),
            // What the methods of an argument sink pick of what it takes:
            // its arguments by position or by name, one at a place or a
            // key, a slice of them, or each made into something else.
            (
                "#let mytable(..args) = table(..args.named(), ..args.pos())\n\
                 #mytable(columns: 2, [a], [b], [c], [d])\n",
                table(&[], &[&["a", "b"], &["c", "d"]]),
            ),
            (
                "#let my-table(..args) = table(stroke: none, ..args.named(), \
                 ..args.pos().map(x => text(size: 9pt, x)))\n\
                 #my-table(columns: 2, [a], [b], [c], [d])\n",
                table(&[], &[&["a", "b"], &["c", "d"]]),
            ),
            (
                "#let th(..args) = table.header(..args.named(), ..args.pos().map(strong))\n\
                 #table(columns: 2, th(repeat: true, [Name], [Age]), [Ann], [30])\n",
                table(&["Name", "Age"], &[&["Ann", "30"]]),
            ),
            (
                "#let hd(..c) = table.header(c.pos().at(0), c.pos().at(1))\n\
                 #table(columns: 2, hd([A], [B]), [a], [b])\n",
                table(&["A", "B"], &[&["a", "b"]]),
            ),
            (
                "#let hd(..c) = table.header(c.pos().last(), c.at(0))\n\
                 #table(columns: 2, hd([A], [B]), [a], [b])\n",
                table(&["B", "A"], &[&["a", "b"]]),
            ),
            (
                "#let wide(..c) = table.cell(colspan: 2, ..c.named(), ..c.pos())\n\
                 #table(columns: 3, wide(fill: red)[AB], [c], [d], [e], [f])\n",
                table(&[], &[&["AB", "", "c"], &["d", "e", "f"]]),
            ),
            (
                "#let hd(..c) = table.header(..c.pos().slice(1), c.pos().first())\n\
                 #table(columns: 3, hd([A], [B], [C]), [a], [b], [c])\n",
                table(&["B", "C", "A"], &[&["a", "b", "c"]]),
            ),
            (
                "#let hd(..c) = table.header(..c.pos().slice(0, 2))\n\
                 #table(columns: 2, hd([A], [B], [C]), [a], [b])\n",
                table(&["A", "B"], &[&["a", "b"]]),
            ),
            (
                "#let hd(..c) = table.header(..c.named().values(), ..c.pos())\n\
                 #table(columns: 2, hd(x: [N], [A]), [a], [b])\n",
                table(&["N", "A"], &[&["a", "b"]]),
            ),
            (
                "#let w(..c) = table.cell(c.at(\"body\", default: [none]), \
                 colspan: c.at(\"span\", default: 1))\n\
                 #table(columns: 2, w(body: [B], span: 2), [x], [y])\n",
                table(&[], &[&["B", ""], &["x", "y"]]),
            ),
            // Through the `let`s before the call, and a `let` hiding a
            // parameter.
            (
                "#let w(..c) = { let cells = c.pos().rev(); table.header(..cells) }\n\
                 #table(columns: 2, w([A], [B]), [a], [b])\n",
                table(&["B", "A"], &[&["a", "b"]]),
            ),
            (
                "#let w(a) = { let a = [Z]; table.header(a, [Q]) }\n\
                 #table(columns: 2, w([1]), [x], [y])\n",
                table(&["Z", "Q"], &[&["x", "y"]]),
            ),
            // A table's parts a wrapper gives with its parameters are those
            // parts, given what the parameters are given, however deep.
            (
                "#let mk(title, ..c) = table(columns: 2, table.header(title, [B]), ..c)\n\
                 #mk([T], [a], [b])\n",
                table(&["T", "B"], &[&["a", "b"]]),
            ),
            (
                "#let mytable(..c) = table(columns: 2, \
                 table.header(..c.pos().slice(0, 2)), ..c.pos().slice(2))\n\
                 #mytable([H1], [H2], [a], [b])\n",
                table(&["H1", "H2"], &[&["a", "b"]]),
            ),
            (
                "#let mk(s, ..c) = table(columns: 2, table.hline(stroke: s), ..c)\n\
                 #mk(1pt, [a], [b], [c], [d])\n",
                table(&[], &[&["a", "b"], &["c", "d"]]),
            ),
            (
                "#let mk(w, ..c) = table(columns: 2, table.cell(colspan: 2, w), ..c)\n\
                 #mk([Wide], [a], [b])\n",
                table(&[], &[&["Wide", ""], &["a", "b"]]),
            ),
            (
                "#let mk(t, ..c) = table(columns: 3, \
                 table.header(table.cell(colspan: 2, t), [C]), ..c)\n\
                 #mk([Wide], [a], [b], [c])\n",
                table(&["Wide", "", "C"], &[&["a", "b", "c"]]),
            ),
            (
                "#let th(x) = table.header(x, [Y])\n\
                 #let mk(t, ..c) = table(columns: 2, th(t), ..c)\n\
                 #mk([X], [a], [b])\n",
                table(&["X", "Y"], &[&["a", "b"]]),
            ),
            (
                "#let b(..c) = table(columns: 3, table.header(..c.pos().slice(0, 2)), \
                 ..c.pos().slice(2))\n\
                 #let a(t, ..c) = b(table.cell(colspan: 2, t), [H], ..c)\n\
                 #a([Wide], [x], [y], [z])\n",
                table(&["Wide", "", "H"], &[&["x", "y", "z"]]),
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(
                table_texts(source),
                std::slice::from_ref(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn headers_after_the_first_row_stay_where_typst_puts_them() {
        // As typst lays them out: a header or footer starts a row of its
        // own, and only a header that opens the table heads it.
        assert_eq!(
            table_texts(
                "#table(columns: 2, table.header([H1], [H2], [H3], [H4]), [a], [b], \
                 table.header(level: 2, [S1], [S2]), [c], [d])\n\n\
                 #table(columns: 2, [a], table.header([M1], [M2]), [c], [d], table.footer([F]))\n"
            ),
            [
                (
                    strings(&["H1", "H2"]),
                    vec![
                        strings(&["H3", "H4"]),
                        strings(&["a", "b"]),
                        strings(&["S1", "S2"]),
                        strings(&["c", "d"]),
                    ]
                ),
                (
                    Vec::new(),
                    vec![
                        strings(&["a", ""]),
                        strings(&["M1", "M2"]),
                        strings(&["c", "d"]),
                        strings(&["F", ""]),
                    ]
                ),
            ]
        );
    }

    #[test]
    fn column_specs_are_evaluated_as_typst_does() {
        let expected = (
            Vec::new(),
            vec![strings(&["a", "b", "c"]), strings(&["d", "e", "f"])],
        );
        for columns in [
            "(auto,) * 3",
            "3 * (1fr,)",
            "(auto,) * 2 + (1fr,)",
            "((auto, 1fr, 2cm))",
            "(..(auto,) * 2, 1fr)",
        ] {
            let source = format!("#table(columns: {columns}, [a], [b], [c], [d], [e], [f])\n");
            assert_eq!(
                table_texts(&source),
                std::slice::from_ref(&expected),
                "{source}"
            );
        }
        // One track, or none given, is one column.
        let one = (Vec::new(), vec![strings(&["a"]), strings(&["b"])]);
        for source in [
            "#table(columns: 1fr, [a], [b])\n",
            "#table(columns: auto, [a], [b])\n",
            "#table(columns: (), [a], [b])\n",
            "#table([a], [b])\n",
        ] {
            assert_eq!(table_texts(source), std::slice::from_ref(&one), "{source}");
        }
        assert_eq!(
            table_texts("#table(table.header([A], [B]), [a])\n"),
            [(strings(&["A"]), vec![strings(&["B"]), strings(&["a"])])]
        );
        // Alignments repeat the way columns do.
        let doc = read("#table(columns: (auto,) * 4, align: (left, right) * 2, [a])\n");
        let [Block::Table(table)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks);
        };
        assert_eq!(
            table.align,
            [Align::Left, Align::Right, Align::Left, Align::Right]
        );
    }

    #[test]
    fn spanning_cells_leave_the_slots_they_cover_empty() {
        // As typst places them: each cell in the next slot no other cell
        // covers.
        let cases: [(&str, Vec<Vec<String>>); 5] = [
            (
                "#table(columns: 3, table.cell(colspan: 2)[wide], [c], [d], [e], [f])\n",
                vec![strings(&["wide", "", "c"]), strings(&["d", "e", "f"])],
            ),
            (
                "#table(columns: 3, table.cell(rowspan: 2)[tall], [b], [c], [d], [e], [f], [g])\n",
                vec![
                    strings(&["tall", "b", "c"]),
                    strings(&["", "d", "e"]),
                    strings(&["f", "g", ""]),
                ],
            ),
            (
                "#table(columns: 2, table.cell(colspan: 2, rowspan: 2)[big], [a], [b])\n",
                vec![
                    strings(&["big", ""]),
                    strings(&["", ""]),
                    strings(&["a", "b"]),
                ],
            ),
            // A cell spanning past the last row adds rows.
            (
                "#table(columns: 2, [a], table.cell(rowspan: 3)[b])\n",
                vec![strings(&["a", "b"]), strings(&["", ""]), strings(&["", ""])],
            ),
            // A span typst refuses, past the last column, covers what fits.
            (
                "#table(columns: 3, [a], [b], table.cell(colspan: 2)[wide], [c])\n",
                vec![strings(&["a", "b", "wide"]), strings(&["c", "", ""])],
            ),
        ];
        for (source, rows) in cases {
            assert_eq!(table_texts(source), [(Vec::new(), rows)], "{source}");
        }
        // Headers and footers are laid out the same way, on rows of their
        // own.
        assert_eq!(
            table_texts(
                "#table(columns: 2, table.header(table.cell(colspan: 2)[Title]), [a], [b], \
                 table.footer(table.cell(colspan: 2)[Foot]))\n\n\
                 #table(columns: 2, table.header([H1], table.cell(rowspan: 2)[H2], [H3]), \
                 [a], [b], table.footer([F]))\n"
            ),
            [
                (
                    strings(&["Title", ""]),
                    vec![strings(&["a", "b"]), strings(&["Foot", ""])]
                ),
                (
                    strings(&["H1", "H2"]),
                    vec![
                        strings(&["H3", ""]),
                        strings(&["a", "b"]),
                        strings(&["F", ""])
                    ]
                ),
            ]
        );
    }

    #[test]
    fn tables_without_cells_are_dropped() {
        for source in [
            "#table()\n",
            "#table(columns: 2)\n",
            "#grid(columns: (1fr,) * 3, gutter: 1em)\n",
            "#table(columns: 2, table.header(), table.hline())\n",
        ] {
            assert_eq!(read(source).blocks, [], "{source}");
        }
        // Empty cells are cells.
        assert_eq!(
            table_texts("#table([], [])\n"),
            [(Vec::new(), vec![vec![String::new()], vec![String::new()]])]
        );
    }

    #[test]
    fn huge_column_counts_and_spans_stay_bounded() {
        for columns in ["99999999999", "(auto,) * 99999999999"] {
            let doc = read(&format!(
                "#table(columns: {columns}, table.cell(colspan: 99999999999, \
                 rowspan: 99999999999)[a], [b])\n"
            ));
            let [Block::Table(table)] = doc.blocks.as_slice() else {
                panic!("{:?}", doc.blocks);
            };
            // At most `MAX_COLUMNS` columns, and spans add at most
            // `MAX_GROWN_CELLS` past the rows the two cells start (the
            // columns `a` spans, empty, are left out once that's spent).
            assert!(table.align.len() <= MAX_COLUMNS);
            let cells = table.rows.len() * table.align.len();
            assert!(cells <= MAX_GROWN_CELLS + 2 * MAX_COLUMNS, "{cells} cells");
            assert_eq!(table.rows[0][0], [text("a")]);
            assert_eq!(table.rows.last().unwrap()[0], [text("b")]);
        }
        // The bound is the document's: tables after it spend it keep their
        // cells but grow no more for their spans.
        let source = "#table(columns: 100, table.cell(rowspan: 99999999)[a], [b])\n\n".repeat(10);
        let doc = read(&source);
        assert_eq!(doc.blocks.len(), 10);
        let mut rows = 0;
        for block in &doc.blocks {
            let Block::Table(table) = block else {
                panic!("{block:?}");
            };
            assert_eq!(table.rows[0][..2], [vec![text("a")], vec![text("b")]]);
            rows += table.rows.len();
        }
        assert!(rows <= MAX_GROWN_CELLS / 100 + 20, "{rows} rows");

        // Columns left empty at a table's end are cells grown too: a form's
        // column to fill in is kept, but past the bound they're left out.
        assert_eq!(
            table_texts("#table(columns: 2, [Name], [], [Ann], [])\n"),
            [(
                Vec::new(),
                vec![strings(&["Name", ""]), strings(&["Ann", ""])]
            )]
        );
        let doc = read(&"#table(columns: 1000, [a])\n\n".repeat(600));
        let widths: Vec<usize> = doc
            .blocks
            .iter()
            .map(|block| match block {
                Block::Table(table) => table.align.len(),
                _ => 0,
            })
            .collect();
        assert_eq!((widths[0], widths[599]), (1000, 1));
        // So are the slots a span covers in its own row.
        let doc = read(
            &"#table(columns: 1000, ..(table.cell(colspan: 999)[a], [b]) * 500)\n\n".repeat(40),
        );
        let slots: usize = doc
            .blocks
            .iter()
            .map(|block| match block {
                Block::Table(table) => table.rows.len() * table.align.len(),
                _ => 0,
            })
            .sum();
        assert!(slots <= MAX_GROWN_CELLS + 2 * 40 * 1000, "{slots} slots");
    }

    #[test]
    fn block_content_in_cells_keeps_its_text() {
        assert_eq!(
            table_texts(
                "#table(columns: 3, align(center)[- one\n- two], quote(block: true)[Quoted], \
                 block[= Title], table(columns: 1, [i1], [i2]), \
                 block[First para.\n\nSecond para.], table.cell(align: left, strong[wide]))\n"
            ),
            [(
                Vec::new(),
                vec![
                    vec!["one two".to_string(), "Quoted".into(), "Title".into()],
                    vec![
                        "i1 i2".into(),
                        "First para. Second para.".into(),
                        "wide".into()
                    ],
                ]
            )]
        );
    }

    /// Every output `doc` is written to, as text: Markdown, plain text,
    /// HTML, LaTeX, typst and the EPUB's files.
    fn every_output(doc: &Document) -> Vec<String> {
        let mut outputs = vec![
            write::markdown::write(doc),
            write::plain::write(doc),
            write::html::write(doc),
            write::latex::write(doc),
        ];
        let options = write::typst::Options {
            target: write::typst::Target::TypstFile,
            fonts: Vec::new(),
            paper: "a4",
            base_dir: std::env::temp_dir(),
            tex_fallback: &std::collections::HashSet::new(),
        };
        outputs.push(write::typst::write(doc, &options).source);
        let options = write::epub::Options {
            base_dir: std::env::temp_dir(),
        };
        let epub = write::epub::write(doc, &options).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(epub)).unwrap();
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).unwrap();
            let mut text = String::new();
            if std::io::Read::read_to_string(&mut file, &mut text).is_ok() {
                outputs.push(text);
            }
        }
        outputs
    }

    #[test]
    fn no_placeholder_survives_into_any_output() {
        // References in notes, quotes, tables and headings, to headings,
        // notes and nothing, and smart quotes and boxes in each of them.
        let doc = read(
            "= Intro \"q\" #box['b'] <intro>\n\
             Text#footnote[See @intro and #link(<intro>)[\"there\"] and @nowhere.] <fn>\n\n\
             = About @intro <about>\n\n\
             #quote(block: true)[@about @fn #box[\"x\"]]\n\n\
             #table(columns: 1, [@intro #link(<nowhere>)[gone] 'c' #box[d]])\n\n\
             - *\"e\"* #text(red)[#box['f'] g\n\n  h #box[\"i\"]]\n",
        );
        let md = write::markdown::write(&doc);
        assert!(
            md.contains(
                "[^1]: See [Intro “q” ‘b’](#intro-q-b) and [“there”](#intro-q-b) and @nowhere."
            ),
            "{md}"
        );
        for output in every_output(&doc) {
            for mark in [LABEL_LINK, HEADING_MARK, QUOTE_MARK, FRAME_MARK] {
                assert!(!output.contains(mark), "{mark:?} in {output:?}");
            }
        }
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
    fn table_parts_reach_through_files_destructuring_and_parentheses() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("doc");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(
            dir.join("lib.typ"),
            "#let theader(..c) = table.header(..c)\n\
             #let thickhline = table.hline.with(stroke: 2pt)\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("sub/more.typ"),
            "#import \"../lib.typ\": theader as th\n#import \"/lib.typ\": thickhline\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("outside.typ"),
            "#let theader(..c) = table.header(..c)\n",
        )
        .unwrap();
        let tables = |source: &str| tables_in(source, &dir);
        let expected = (
            vec!["H1".to_string(), "H2".into()],
            vec![
                vec!["a".to_string(), "b".into()],
                vec!["c".into(), "d".into()],
            ],
        );
        for source in [
            "#import \"lib.typ\": theader, thickhline\n\
             #table(columns: 2, theader([H1], [H2]), thickhline(), [a], [b], [c], [d])\n",
            "#import \"lib.typ\": *\n\
             #table(columns: 2, theader[H1][H2], [a], [b], thickhline(), [c], [d])\n",
            "#import \"lib.typ\"\n\
             #table(columns: 2, lib.theader[H1][H2], [a], [b], lib.thickhline(), [c], [d])\n",
            "#import \"lib.typ\" as l\n\
             #table(columns: 2, l.theader[H1][H2], [a], [b], l.thickhline(), [c], [d])\n",
            // A file's imports are names of its own, by paths from it or
            // from the root.
            "#import \"sub/more.typ\": th, thickhline\n\
             #table(columns: 2, th[H1][H2], [a], [b], thickhline(), [c], [d])\n",
            "#let (header, hline) = (table.header, table.hline)\n\
             #table(columns: 2, header[H1][H2], [a], [b], hline(), [c], [d])\n",
            "#table(columns: 2, (table).header[H1][H2], [a], [b], (table.hline)(), [c], [d])\n",
        ] {
            assert_eq!(tables(source), std::slice::from_ref(&expected), "{source}");
        }
        // A file outside the input's folder isn't read, and the content
        // given to calls the reader can't tell stays, a cell each.
        assert_eq!(
            tables(
                "#import \"../outside.typ\": theader\n\
                 #table(columns: 2, theader[H1][H2], [a], myrow([b], [c]), [d], myline())\n"
            ),
            [(
                Vec::new(),
                vec![
                    vec!["H1".to_string(), "H2".into()],
                    vec!["a".into(), "b".into()],
                    vec!["c".into(), "d".into()],
                ]
            )]
        );
    }

    fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph(content)
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
    fn settings_given_to_calls_show_nothing() {
        // Colors, gradients and paths are a call's settings, not text it
        // shows; typst shows the content alone.
        let doc = read(
            "Word #text(rgb(\"#ff0000\"))[red] done. #text(color.rgb(\"#123\"))[cr] \
             #text(20pt, rgb(\"#00f\"))[big] \
             #text(gradient.linear(rgb(\"#f00\"), rgb(\"#00f\")))[Rainbow] \
             #badge(\"#f00\")[hot] #fig(\"photo.png\", [Cap]) #html.elem(\"span\")[styled] \
             #todo(\"fix this\") #custom([Title], \"and\", [Body])\n",
        );
        assert_eq!(
            paragraph_texts(&doc),
            ["Word red done. cr big Rainbow hot Cap styled fix this Title and Body"]
        );
        let md = write::markdown::write(&read(
            "= #text(rgb(\"#c00\"))[Intro] <i>\n\nSee @i.\n\n\
             #table(columns: 1, text(fill: red, rgb(\"#f00\"))[a])\n",
        ));
        assert!(md.contains("See [Intro](#intro)."), "{md}");
        assert!(md.contains("| a |"), "{md}");
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

    /// The header and rows of each table in `source`, read in `dir`, as
    /// text.
    fn tables_in(source: &str, dir: &Path) -> Vec<TableText> {
        tables_of(&read_in(source, dir))
    }

    fn strings(cells: &[&str]) -> Vec<String> {
        cells.iter().map(|c| c.to_string()).collect()
    }

    #[test]
    fn names_the_reader_cant_tell_keep_their_content() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("doc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            root.path().join("outside.typ"),
            "#let theader(..c) = table.header(..c)\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("lib.typ"),
            "#let h(..c) = { let cells = c.pos(); table.header(..cells) }\n\
             #let theader(..c) = table.header(..c)\n#let thl = table.hline\n\
             #let sh(title, sub) = table.cell(colspan: 2)[#title: #sub]\n\
             #let pct(x) = [#x%]\n#let mark() = [x]\n",
        )
        .unwrap();
        std::fs::write(dir.join("lib2.typ"), "#let header(body) = strong(body)\n").unwrap();
        let header = |cells: &[&str], rows: &[&[&str]]| {
            (
                strings(cells),
                rows.iter().map(|r| strings(r)).collect::<Vec<_>>(),
            )
        };
        let cases: &[(&str, TableText)] = &[
            // A name for what the reader can't read hides typst's own.
            (
                "#import \"../outside.typ\": theader as h\n\
                 #table(columns: 2, h[H1][H2], [a], [b])\n",
                header(&[], &[&["H1", "H2"], &["a", "b"]]),
            ),
            (
                "#let line(..c) = c.pos().join([ -- ])\n#table(columns: 2, line[L1][L2], [a])\n",
                header(&[], &[&["L1 L2", "a"]]),
            ),
            // A function returning what it computes first.
            (
                "#import \"lib.typ\": h\n#table(columns: 2, h[H1][H2], [a], [b])\n",
                header(&["H1", "H2"], &[&["a", "b"]]),
            ),
            // Destructuring, nested, parenthesized, by key or of a name.
            (
                "#let parts = (table.header, table.hline)\n#let (v, pl) = parts\n\
                 #table(columns: 2, v[E1][E2], pl(), [a], [b])\n",
                header(&["E1", "E2"], &[&["a", "b"]]),
            ),
            (
                "#let ((h, l), v) = ((table.header, table.hline), table.vline)\n\
                 #table(columns: 2, h[A1][A2], l(), v(x: 1), [a], [b])\n",
                header(&["A1", "A2"], &[&["a", "b"]]),
            ),
            (
                "#let (hh) = table.header\n#table(columns: 2, hh[A][B], [a], [b])\n",
                header(&["A", "B"], &[&["a", "b"]]),
            ),
            (
                "#let (eh, el) = ((table.header, table.hline))\n\
                 #table(columns: 2, eh[A][B], el(), [a], [b])\n",
                header(&["A", "B"], &[&["a", "b"]]),
            ),
            (
                "#let (header: dh) = (header: table.header, line: table.hline)\n\
                 #table(columns: 2, dh[A][B], [a], [b])\n",
                header(&["A", "B"], &[&["a", "b"]]),
            ),
            (
                "#import table: hline, header\n\
                 #let (hline, header) = (hline.with(stroke: 2pt), header.with(repeat: true))\n\
                 #table(columns: 2, header[A][B], hline(), [a], [b])\n",
                header(&["A", "B"], &[&["a", "b"]]),
            ),
            // A module's names, through each way of importing them.
            (
                "#import \"lib.typ\" as l: thl\n\
                 #table(columns: 2, l.theader[A][B], thl(), [a], [b])\n",
                header(&["A", "B"], &[&["a", "b"]]),
            ),
            (
                "#import \"lib.typ\"\n#import lib: theader\n\
                 #table(columns: 2, theader[A][B], [a], [b])\n",
                header(&["A", "B"], &[&["a", "b"]]),
            ),
            (
                "#import \"lib.typ\" as l\n#import l: theader as T\n\
                 #table(columns: 2, T[A][B], [a], [b])\n",
                header(&["A", "B"], &[&["a", "b"]]),
            ),
            // A file's names hide what they meant before.
            (
                "#import table: header\n#import \"lib2.typ\": *\n\
                 #table(columns: 2, header[A], [B])\n",
                header(&[], &[&["A", "B"]]),
            ),
            // Lines in parentheses are lines.
            (
                "#let toprule = table.hline()\n\
                 #table(columns: 2, (toprule), [a], (table.hline()), [b])\n",
                header(&[], &[&["a", "b"]]),
            ),
            // The document's own functions make a cell each, the content
            // they're given in it.
            (
                "#let money(x) = [USD #x]\n#let yes() = text(green)[YES]\n\
                 #let pair(a, b) = [#a / #b]\n\
                 #table(columns: 3, [Apple], money(12), yes(), [k1], pair[x][y], [z])\n",
                header(&[], &[&["Apple", "", ""], &["k1", "x y", "z"]]),
            ),
            // A cell's wrapper keeps its span, and a call to what the
            // reader can't see given only values is a cell, given nothing
            // a line.
            (
                "#import \"lib.typ\": sh, pct, mark\n\
                 #table(columns: 2, sh[Alpha][Beta], [a], pct(50), mark(), num(1.5), myline())\n",
                header(&[], &[&["Alpha Beta", ""], &["a", ""], &["", ""]]),
            ),
        ];
        for (source, expected) in cases {
            assert_eq!(
                tables_in(source, &dir),
                std::slice::from_ref(expected),
                "{source}"
            );
        }
    }

    #[test]
    fn imports_are_read_once_each() {
        // A file importing itself, and a long chain of files importing the
        // next one again and again: each file is read once, so this ends
        // at once.
        let dir = tempfile::tempdir().unwrap();
        let imports = |file: &str| {
            (0..8)
                .map(|n| format!("#import \"{file}\": x{n}\n"))
                .collect::<String>()
        };
        std::fs::write(dir.path().join("self.typ"), imports("self.typ")).unwrap();
        for n in 0..12 {
            let next = format!("c{}.typ", n + 1);
            std::fs::write(dir.path().join(format!("c{n}.typ")), imports(&next)).unwrap();
        }
        std::fs::write(dir.path().join("c12.typ"), "#let th = table.header\n").unwrap();
        let start = std::time::Instant::now();
        let source = format!(
            "{}{}#import \"c11.typ\": *\n#import \"c12.typ\": th\n\
             #table(columns: 1, th[H], [a])\n",
            imports("self.typ"),
            imports("c0.typ")
        );
        assert_eq!(
            tables_in(&source, dir.path()),
            [(strings(&["H"]), vec![strings(&["a"])])]
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
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
    fn settings_made_by_the_documents_own_functions_show_nothing() {
        // As typst shows them: functions returning an alignment, an angle,
        // a length or a color are settings, not content.
        let doc = read(
            "#let al(x) = center\n#let ang(x) = 45deg\n#let gap(x) = 1em\n\
             #let pos(x) = top + right\n#let tint(x) = { let c = rgb(x); c }\n\
             #let id(text) = text\n\
             #align(al(\"c\"))[aligned] #rotate(ang(\"x\"))[rotated] \
             #place(pos(\"tr\"))[placed] #stack([s1], gap(\"m\"), [s2]) \
             #text(tint(\"#f00\"))[red] #gap(\"g\") #id(\"kept\")\n",
        );
        assert_eq!(
            paragraph_texts(&doc),
            ["aligned", "rotated", "placed", "s1", "s2", "red kept"]
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
    fn cells_of_calls_the_reader_cant_tell_keep_the_rows() {
        // Strings before content are settings, and a call given only
        // values is one cell, as a method call on a value is, which shows
        // nothing of what it computes with.
        assert_eq!(
            table_texts(
                "#let names = (\"Ann\", \"Bob\")\n\
                 #table(columns: 2, [Speed], qty(\"10\", \"km/h\"), badge(\"#f00\")[hot], [b], \
                 [Rev], \"abc\".rev(), upper(\"abc\").rev(), [y], \
                 datetime(year: 2024, month: 1, day: 2).display(), [z], \
                 [Who], names.join(\", \"))\n"
            ),
            one_table(&[
                &["Speed", "10 km/h"],
                &["hot", "b"],
                &["Rev", ""],
                &["", "y"],
                &["", "z"],
                &["Who", ""]
            ])
        );
        // A name the reader can't see, placed bare, is taken for a line, as
        // a call to one given nothing is.
        assert_eq!(
            table_texts(
                "#import \"@preview/booktabs:0.1.0\": *\n\
                 #table(columns: 2, toprule, [a], [b], midrule(), [c], [d], bottomrule)\n"
            ),
            one_table(&[&["a", "b"], &["c", "d"]])
        );
    }

    #[test]
    fn content_given_by_name_to_calls_the_reader_cant_tell_adds_no_cells() {
        // A box's title and footer show in its cell, with its body.
        assert_eq!(
            table_texts(
                "#import \"@preview/gentle-clues:1.0.0\": *\n\
                 #table(columns: 2, [Tip], tip(title: [Hint])[Use X], [a], [b])\n"
            ),
            one_table(&[&["Tip", "Hint Use X"], &["a", "b"]])
        );
        assert_eq!(
            table_texts(
                "#import \"@preview/showybox:2.0.4\": showybox\n\
                 #table(columns: 2, [x], showybox(title: [Title], footer: [Foot])[Body], \
                 [a], [b])\n"
            ),
            one_table(&[&["x", "Title Foot Body"], &["a", "b"]])
        );
        // Given by name alone, it's still one cell, and given content by
        // name, it's one cell however much it's given by position.
        assert_eq!(
            table_texts(
                "#import \"@preview/x:1.0.0\": *\n\
                 #table(columns: 2, clue(title: [T], footer: [F]), [a], \
                 pair(title: [T])[L][R], two([A], note: [n]))\n"
            ),
            one_table(&[&["T F", "a"], &["T L R", "A n"]])
        );
        // Content given by name is content of any form, and a string given
        // by a name for what it shows is too: it's no line, and no cell
        // after it moves.
        for (call, cell) in [
            ("showybox(title: emph[Hint])", "Hint"),
            ("showybox(title: strong[Hint], [Body])", "Hint Body"),
            ("tip(title: emph[Hint])[Use X]", "Hint Use X"),
            ("showybox(title: \"Hint\")", "Hint"),
            ("showybox(title: [T], [B1], [B2])", "T B1 B2"),
            ("tip(title: [T], \"a\", \"b\")", "T a b"),
            ("qty(\"10\", \"km/h\", note: [approx])", "10 km/h approx"),
        ] {
            let source = format!(
                "#import \"@preview/x:1.0.0\": *\n#table(columns: 2, [Tip], {call}, [a], [b])\n"
            );
            assert_eq!(
                table_texts(&source),
                one_table(&[&["Tip", cell], &["a", "b"]]),
                "{source}"
            );
        }
        // Given only settings by name, it's a line, as in booktabs-style
        // tables; given strings and content, it shows the content.
        assert_eq!(
            table_texts(
                "#import \"@preview/x:1.0.0\": *\n\
                 #table(columns: 2, toprule(stroke: 1pt), [a], [b], \
                 badge(color: \"red\")[hot], [c])\n"
            ),
            one_table(&[&["a", "b"], &["hot", "c"]])
        );
    }

    #[test]
    fn settings_given_by_name_to_calls_the_reader_cant_tell_show_nothing() {
        // By any name but one for what it shows, a string is a setting, as
        // is a method of a value of typst's own or what a function the
        // reader can't tell returns: given only those, a call is a line.
        for rule in [
            "toprule(stroke: black.lighten(20%))",
            "hline(stroke: theme.rule())",
            "toprule(stroke: thick())",
            "midrule(color: \"gray\")",
            "toprule(label: \"x\")",
        ] {
            let source = format!(
                "#import \"@preview/booktabs:0.1.0\": *\n#table(columns: 2, {rule}, [a], [b])\n"
            );
            assert_eq!(table_texts(&source), one_table(&[&["a", "b"]]), "{source}");
        }
        // Given content by position too, it's a cell for each.
        for fill in [
            "red.transparentize(50%)",
            "rgb(\"#eee\").darken(10%)",
            "my-color()",
        ] {
            let source = format!(
                "#import \"@preview/x:1.0.0\": *\n\
                 #table(columns: 2, hdr([A], [B], fill: {fill}), [c], [d])\n"
            );
            assert_eq!(
                table_texts(&source),
                one_table(&[&["A", "B"], &["c", "d"]]),
                "{source}"
            );
        }
        // Outside tables too.
        let doc = read(
            "#import \"@preview/unify:0.7.1\": *\n\
             #qty(\"1.3\", \"km/s\", per: \"fraction\") #num(\"123456\", thousandsep: \",\") \
             #badge(\"x\", color: \"red\") #badge(fill: theme.color(\"primary\"))[hot] \
             #box2(stroke: stroke-of(\"dashed\"))[y]\n",
        );
        assert_eq!(paragraph_texts(&doc), ["1.3 km/s 123456 x hot y"]);
    }

    #[test]
    fn functions_of_modules_and_dictionaries_are_no_methods() {
        // A module's or a dictionary's function shows the strings it's
        // given, as it would by its own name; a method of a value the
        // reader can't tell doesn't.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.typ"),
            "#import \"@preview/x:0.1.0\": make\n#let note = make()\n\
             #let pick = if true { x => x } else { x => x }\n",
        )
        .unwrap();
        let doc = read_in(
            "#import \"@preview/x:0.1.0\": make\n#import \"lib.typ\"\n#import \"lib.typ\" as l\n\
             #let d = (note: make(), pick: if true { x => x })\n#let t = make()\n\
             #lib.note(\"Hello\") #lib.pick(\"Picked\") #(l.note)(\"Paren\") \
             #d.note(\"Entry\") #d.pick(\"Picks\") #t.callout(\"pos\")[kept]\n\n\
             #table(columns: 1, lib.note(\"cell\"))\n",
            dir.path(),
        );
        assert_eq!(
            paragraph_texts(&doc),
            ["Hello Picked Paren Entry Picks kept"]
        );
        assert_eq!(
            doc.blocks[1],
            Block::Table(Table {
                align: vec![Align::Default],
                header: Vec::new(),
                rows: vec![vec![vec![text("cell")]]],
            })
        );
    }

    #[test]
    fn presets_give_the_arguments_they_preset_first() {
        // As typst calls `f.with(a)(b)` as `f(a, b)`: of a function the
        // document defines, a package's function, or a preset again.
        let definitions = "#import \"@preview/gentle-clues:1.3.1\": clue\n\
                           #let greet(name, msg) = [#name: #msg]\n#let g = greet.with([Ann])\n\
                           #let tip = clue.with(title: [Tip])\n#let quiet = tip.with(icon: none)\n";
        assert_calls_show(
            definitions,
            &[
                ("#g[hello]", "Ann hello"),
                ("#tip[body]", "Tip body"),
                ("#quiet[again]", "Tip again"),
            ],
        );
        assert_eq!(
            table_texts(&format!(
                "{definitions}#table(columns: 2, tip[cell], [y])\n"
            )),
            one_table(&[&["Tip cell", "y"]])
        );
        // A string given through an argument sink is a setting, as typst's
        // functions take it, but by a name for what it shows.
        assert_calls_show(
            "#import \"@preview/gentle-clues:1.3.1\": clue\n#let pass(..args) = clue(..args)\n",
            &[("#pass(title: \"Hint\", accent: \"red\")[body]", "Hint body")],
        );
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
    fn functions_named_after_typsts_own_that_call_them_are_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("template.typ"),
            "#let link(dest, body) = std.link(dest, underline(body))\n",
        )
        .unwrap();
        let md = |source: &str| write::markdown::write(&read_in(source, dir.path()));
        for source in [
            "#let link(dest, body) = std.link(dest, underline(body))\n\
             See #link(\"https://example.com\")[the site] now.\n",
            "#import \"template.typ\": *\nSee #link(\"https://example.com\")[the site] now.\n",
        ] {
            assert!(
                md(source).contains("See [the site](https://example.com) now."),
                "{source}: {}",
                md(source)
            );
        }
        assert!(
            md(
                "#let image(path, ..args) = std.image(path, width: 50%, ..args)\n\
                #image(\"pic.png\")\n"
            )
            .contains("![](pic.png)")
        );
        assert_eq!(
            table_texts(
                "#let table(..args) = { set text(size: 9pt); std.table(..args) }\n\
                 #table(columns: 2, [H1], [H2], [a], [b])\n"
            ),
            [(
                Vec::new(),
                vec![strings(&["H1", "H2"]), strings(&["a", "b"])]
            )]
        );
    }

    #[test]
    fn dictionaries_destructure_by_key_and_parts_made_beforehand_keep_their_cells() {
        let expected = (strings(&["H1", "H2"]), vec![strings(&["a", "b"])]);
        for source in [
            "#let (header, hline) = (header: table.header, hline: table.hline)\n\
             #table(columns: 2, header([H1], [H2]), hline(), [a], [b])\n",
            "#let hdr = table.header([H1], [H2])\n#table(columns: 2, hdr, [a], [b])\n",
        ] {
            assert_eq!(
                table_texts(source),
                std::slice::from_ref(&expected),
                "{source}"
            );
        }
    }

    #[test]
    fn arrays_doubling_themselves_stay_bounded() {
        let mut source = String::from("#let a0 = (table.hline, table.header)\n");
        for n in 1..=24 {
            source.push_str(&format!("#let a{n} = (..a{}, ..a{})\n", n - 1, n - 1));
        }
        source.push_str(&"#[x] ".repeat(500));
        let start = std::time::Instant::now();
        assert_eq!(paragraph_texts(&read(&source)).len(), 1);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn wrappers_calling_wrappers_again_and_again_stay_bounded() {
        let mut source =
            String::from("#let w0() = table.cell[x]\n#let h0(..c) = table.header([x], ..c)\n");
        for n in 1..=40 {
            let m = n - 1;
            source.push_str(&format!(
                "#let w{n}() = table.cell[#w{m}() #w{m}()]\n\
                 #let h{n}(..c) = table.header(h{m}(..c), h{m}(..c))\n"
            ));
        }
        source.push_str("#table(w40(), h40[y])\n");
        let start = std::time::Instant::now();
        assert_eq!(table_texts(&source).len(), 1);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        // Each giving the one before what it's given twice: the arguments
        // double at each, as typst doubles them, but only so far.
        for (base, twice) in [
            ("table.cell(..c)", "..c, ..c"),
            ("table(columns: 2, ..c)", "..c, ..c"),
            ("table.header(..c)", "..c.pos(), ..c.pos()"),
            ("table.cell(..c)", "c, c"),
            ("table.cell(..c)", "[#c], [#c]"),
        ] {
            let mut source = format!("#let w0(..c) = {base}\n");
            for n in 1..=40 {
                source.push_str(&format!("#let w{n}(..c) = w{}({twice})\n", n - 1));
            }
            source.push_str("#table(columns: 2, w40[a][b], [c])\n\n#w40[a][b]\n");
            let start = std::time::Instant::now();
            assert!(!table_texts(&source).is_empty(), "{base} {twice}");
            assert!(
                start.elapsed() < std::time::Duration::from_secs(5),
                "{base} {twice}"
            );
        }
        // Each wrapping the one before under the same name: the last is
        // still a header, however long the chain.
        let mut source = String::from("#let h(..c) = table.header(..c)\n#let p = table.header\n");
        source.push_str(&"#let h(..c) = h(..c)\n#let p = p.with()\n".repeat(20_000));
        source.push_str("#table(columns: 2, h[A][B], p[C][D], [a], [b])\n");
        assert_eq!(
            table_texts(&source),
            [(
                strings(&["A", "B"]),
                vec![strings(&["C", "D"]), strings(&["a", "b"])]
            )]
        );
    }

    #[test]
    fn long_documents_unwrap_every_call_to_their_wrappers() {
        // A wrapper giving many arguments of its own, called in every row
        // of a long table: what the reader may spend unwrapping grows
        // with the document, so the last row keeps its `#` too.
        let rows = 20_000;
        let mut source = String::from(
            "#let num(..c) = table.cell(colspan: 1, fill: luma(240), \
             inset: (x: 6pt, y: 4pt), align: right + horizon, \
             stroke: (bottom: 0.5pt + gray), [\\#], ..c)\n#table(columns: 2,\n",
        );
        for row in 0..rows {
            source.push_str(&format!("  num([{row}]), [r{row}],\n"));
        }
        source.push_str(")\n");
        let tables = table_texts(&source);
        let last = tables[0].1.last().unwrap();
        assert_eq!(
            last,
            &strings(&[&format!("# {}", rows - 1), &format!("r{}", rows - 1)])
        );
    }

    #[cfg(unix)]
    #[test]
    fn imports_of_what_isnt_a_file_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.typ");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        let dir = dir.path().to_path_buf();
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let doc = read_in("#import \"pipe.typ\": x\nText.\n", &dir);
            send.send(paragraph_texts(&doc)).unwrap();
        });
        let texts = receive
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("reading a pipe hangs");
        assert_eq!(texts, ["Text."]);
    }

    #[test]
    fn functions_the_document_defines_show_the_strings_their_bodies_show() {
        // A string given for a parameter the body shows is text, and one
        // for a parameter it only uses as a setting (given by name or to
        // `rgb`, compared) is a setting, as typst shows them.
        assert_calls_show(
            "#let thm(name, body) = [*Theorem* (#name). #body]\n\
             #let entry(date, title, ..rest) = [#date, #title: #rest.pos().join()]\n\
             #let badge(color, body) = box(fill: rgb(color), inset: 2pt, body)\n\
             #let tag(c, body) = text(fill: c, body)\n\
             #let kind(k, body) = if k == \"warn\" [Warning: #body] else [#body]\n\
             #let card(title: \"Untitled\", tint: \"red\", body) = \
             block(fill: rgb(tint))[#title: #body]\n",
            &[
                ("#thm(\"Pythagoras\")[proof]", "Pythagoras proof"),
                (
                    "#entry(\"2020\", \"Engineer\", \"Acme\")[work]",
                    "2020 Engineer Acme work",
                ),
                ("#badge(\"#f00\")[hot]", "hot"),
                ("#tag(\"#0f0\")[green]", "green"),
                ("#kind(\"warn\")[careful]", "careful"),
                ("#card(title: \"T\", tint: \"blue\")[c]", "T c"),
            ],
        );
        // Given to typst's own functions, a string is shown where their
        // parameter shows it: a link's destination without a body, and raw
        // text.
        assert_calls_show(
            "#let site(url, body) = link(url, body)\n#let bare(url) = link(url)\n\
             #let code(c) = raw(c)\n#let turn(angle, body) = rotate(angle, body)\n",
            &[
                ("#site(\"https://a.org\")[here]", "here"),
                ("#bare(\"https://b.org\")", "https://b.org"),
                ("#code(\"x = 1\")", "x = 1"),
                ("#turn(\"45deg\")[turned]", "turned"),
            ],
        );
        // A path isn't text, a caption given by name is, and a function
        // calling another of the document's shows what that one shows.
        assert_calls_show(
            "#let fig(path, cap) = figure(image(path, width: 80%), caption: cap)\n\
             #let inner(c, body) = text(fill: rgb(c), body)\n\
             #let outer(c, note, body) = inner(c, [#note #body])\n",
            &[
                ("#fig(\"photo.png\", \"Caption text\")", "Caption text"),
                ("#outer(\"#f00\", \"Note:\")[x]", "Note: x"),
            ],
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

    #[test]
    fn references_copy_a_bounded_amount_of_heading_text() {
        // A long heading referred to again and again: past what references
        // may copy, a reference shows its label.
        let long = "word ".repeat(4000);
        let source = format!(
            "#set heading(numbering: \"1.\")\n= {long}<h>\n\n{}\n",
            "@h ".repeat(5000)
        );
        let doc = read(&source);
        let [_, Block::Paragraph(body)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks.len());
        };
        let shown = inline_text(body);
        assert!(shown.starts_with("word word "), "{}", &shown[..40]);
        assert!(shown.ends_with("@h"), "{}", &shown[shown.len() - 40..]);
        // The links count too, whose targets are as long as the heading.
        let targets: usize = body
            .iter()
            .map(|inline| match inline {
                Inline::Link { url, .. } => url.len(),
                _ => 0,
            })
            .sum();
        let budget = MAX_REFERENCE_TEXT.max(source.len() * REFERENCE_TEXT_PER_BYTE);
        let copied = shown.len() + targets;
        assert!(copied < budget + 5000 * "@h ".len(), "{copied}");
        // So do the inlines, which a writer may write at length (math).
        let source = format!(
            "#set heading(numbering: \"1.\")\n= {}<h>\n\n{}\n",
            "$x$ ".repeat(5000),
            "@h ".repeat(5000)
        );
        let doc = read(&source);
        let [_, Block::Paragraph(body)] = doc.blocks.as_slice() else {
            panic!("{:?}", doc.blocks.len());
        };
        let math: usize = body
            .iter()
            .map(|inline| match inline {
                Inline::Link { content, .. } => content.len(),
                _ => 0,
            })
            .sum();
        let budget = MAX_REFERENCE_TEXT.max(source.len() * REFERENCE_TEXT_PER_BYTE);
        assert!(math <= budget / REFERENCE_INLINE_COST, "{math}");
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

    #[test]
    fn functions_the_document_defines_show_only_the_content_their_bodies_show() {
        // Content given for a parameter the body never shows, by position
        // or by name, is hidden, as typst hides it, as is what an argument
        // sink takes but for the places the body picks.
        assert_calls_show(
            "#let second(a, b) = b\n#let note(author, body) = block[#body]\n\
             #let tr(en, zh) = zh\n\
             #let exercise(body, solution: none) = [*Exercise.* #body]\n\
             #let card(sep: [---], body) = [#body]\n\
             #let rest(a, ..more) = more.pos().join()\n\
             #let last(..args) = args.pos().last()\n#let first(..args) = [#args.pos().at(0)]\n\
             #let count(..args) = [#args.pos().len()]\n",
            &[
                ("#second([hidden], [shown])", "shown"),
                ("#note([Ann])[The note]", "The note"),
                ("#tr([Hello], [你好])", "你好"),
                (
                    "#exercise(solution: [42])[What is 6 times 7?]",
                    "What is 6 times 7?",
                ),
                ("#card(sep: [SEP])[x]", "x"),
                ("#rest([first])[others]", "others"),
                ("#last(\"en\", [no])[yes]", "yes"),
                ("#first([one], [two])", "one"),
                ("#count([p], [q])", ""),
            ],
        );
        // Content the body shows is shown, by name too, as is what it
        // passes on to what the reader can't see.
        assert_calls_show(
            "#import \"@preview/pkg:0.1.0\": frame\n\
             #let card(title: none, body) = [#title: #body]\n\
             #let wrap(a, b) = frame(a, b)\n",
            &[("#card(title: [T])[c]", "T c"), ("#wrap([A], [B])", "A B")],
        );
    }

    /// Checks what each call of `cases` shows: `(call, shown)`, each call
    /// read on its own after `definitions`, so a failure names its call.
    /// A call shown as nothing makes no paragraph.
    fn assert_calls_show(definitions: &str, cases: &[(&str, &str)]) {
        for &(call, shown) in cases {
            let doc = read(&format!("{definitions}{call}\n"));
            let expected: Vec<&str> = [shown].into_iter().filter(|s| !s.is_empty()).collect();
            assert_eq!(paragraph_texts(&doc), expected, "{call}");
        }
    }

    #[test]
    fn settings_the_documents_own_functions_pass_on_show_nothing() {
        // A string the body gives `text` or `raw` for a setting (a
        // language, a font, a weight) isn't text: only `text`'s body and
        // `raw`'s text are.
        assert_calls_show(
            "#let lt(lang, body) = text(lang: lang, body)\n\
             #let ft(f, body) = text(font: f, body)\n#let wt(w, body) = text(weight: w, body)\n\
             #let ft2(f, body) = text(font: f)[#body]\n\
             #let snippet(src, lang: \"txt\") = raw(src, lang: lang, block: true)\n",
            &[
                ("#lt(\"zh\")[中文]", "中文"),
                ("#ft(\"Arial\")[sans]", "sans"),
                ("#wt(\"bold\")[heavy]", "heavy"),
                ("#ft2(\"Arial\")[again]", "again"),
                ("#snippet(\"x = 1\", lang: \"python\")", "x = 1"),
            ],
        );
    }

    #[test]
    fn values_typst_shows_as_text_keep_their_strings() {
        // A string or a symbol is text where typst shows it, though no
        // parameter of `str` or `symbol` takes content.
        assert_eq!(
            paragraph_texts(&read("Start #str(\"plain\") #symbol(\"x\") End\n")),
            ["Start plain x End"]
        );
    }

    #[test]
    fn methods_of_values_the_document_makes_keep_their_content() {
        // What the method makes of its strings the reader can't tell, but
        // the content it's given it shows, its blocks included.
        let doc = read(
            "#import \"@preview/themes:0.1.0\": make\n#let theme = make()\n\
             #(theme.note)[Hi there] #theme.note[x] #theme.badge(\"warn\")[hot] \
             #theme.callout[Body #table(columns: 1, [c])] end\n",
        );
        assert_eq!(
            doc.blocks,
            [
                para(vec![text("Hi there x hot Body")]),
                Block::Table(Table {
                    align: vec![Align::Default],
                    header: Vec::new(),
                    rows: vec![vec![vec![text("c")]]],
                }),
                para(vec![text("end")]),
            ]
        );
    }

    #[test]
    fn what_bodies_show_follows_presets_math_packages_and_types() {
        // A preset gives the first parameters, so the arguments given after
        // it go to the parameters past them.
        assert_calls_show(
            "#let admon(kind, body) = block(fill: rgb(kind))[#body]\n\
             #let warn = admon.with(\"#f00\")\n#let second(a, b) = b\n\
             #let tr(en, zh) = zh\n#let zh = tr.with([Hello])\n\
             #let pair(a, b, c) = [#c]\n#let p = pair.with([x]).with([y])\n\
             #let f(b) = warn(b)\n",
            &[
                ("#warn[Body]", "Body"),
                ("#second.with([preset])([given])", "given"),
                ("#zh([你好])", "你好"),
                ("#p([Z])", "Z"),
                ("#f[Inner]", "Inner"),
            ],
        );
        // Math shows all it's given.
        assert_calls_show(
            "#let norm(body) = $lr(|| body ||)$\n#let root(body) = $sqrt(body)$\n\
             #let ratio(num, den) = $num / den$\n\
             #let sumto(top, bot) = $attach(sum, t: #top, b: #bot)$\n",
            &[
                ("#norm[vec]", "vec"),
                ("#root[r]", "r"),
                ("#ratio([top], [bottom])", "top bottom"),
                ("#sumto([T], [B])", "T B"),
            ],
        );
        // What a body gives by name to what the reader can't see (a
        // package's function, a method) may be content it shows, but a
        // string given there is a setting, unless the body shows it or
        // gives it by a name for what that shows.
        assert_calls_show(
            "#import \"@preview/showybox:2.0.4\": showybox\n\
             #let note(title, body) = showybox(title: title, body)\n\
             #let tip(title: none, body) = showybox(title: [*#title*], body)\n\
             #let tinted(color, body) = showybox(frame: (border-color: color), body)\n\
             #let theme = (card: (title: none, body) => [#title: #body])\n\
             #let card(t, b) = (theme.card)(title: t, b)\n",
            &[
                ("#note([Ttl])[Body]", "Ttl Body"),
                ("#note(\"Ttl\")[Body]", "Ttl Body"),
                ("#tip(title: \"Hint\")[Use]", "Hint Use"),
                ("#tinted(\"red\")[Plain]", "Plain"),
                ("#card([T2])[B2]", "T2 B2"),
            ],
        );
        // Typst finds the settings it may be given before a body by their
        // type, so a body alone goes to the body, and an array to a
        // parameter taking arrays gives it its items.
        assert_calls_show(
            "#let centered(body) = align(body)\n#let pl(body) = place(body)\n\
             #let rt(body) = rotate(body)\n#let two(body) = columns(body)\n\
             #let turn(angle, body) = rotate(angle, body)\n\
             #let pair(a, b) = math.mat((a, b))\n",
            &[
                ("#centered[Mid]", "Mid"),
                ("#pl[Placed]", "Placed"),
                ("#rt[Turned]", "Turned"),
                ("#two[Cols]", "Cols"),
                ("#turn(\"45deg\")[ok]", "ok"),
                ("#pair([AA], [BB])", "AA BB"),
            ],
        );
        // Past arguments spread from what the reader can't tell, it can't
        // tell which parameter an argument goes to.
        assert_calls_show(
            "#let second(a, b) = b\n#let xs = ([A],)\n",
            &[("#second(..xs, [B])", "B")],
        );
    }

    #[test]
    fn set_rules_in_bodies_show_only_the_content_they_take() {
        // A setting given to a set rule is no text, though content given
        // to one (a page's header) is.
        assert_calls_show(
            "#let b1(lang, body) = { set text(lang: lang); body }\n\
             #let b2(f, body) = [#set text(font: f)\n#body]\n\
             #let b3(lang, src) = { set raw(lang: lang); raw(src) }\n\
             #let b4(l, body) = { set text(lang: l) if l != none; body }\n\
             #let b5(lang, body) = [#set text(lang: lang); #body]\n\
             #let b6(font, body) = { show raw: set text(font: font); body }\n\
             #let b7(h, body) = { set page(header: h); body }\n",
            &[
                ("#b1(\"zh\")[中文]", "中文"),
                ("#b2(\"Libertinus Serif\")[font]", "font"),
                ("#b3(\"python\", \"x = 3\")", "x = 3"),
                ("#b4(\"fr\")[Oui]", "Oui"),
                ("#b5(\"ja\")[日本]", "日本"),
                ("#b6(\"Menlo\")[m]", "m"),
                ("#b7([HDR])[Body]", "HDR Body"),
            ],
        );
    }

    #[test]
    fn values_made_of_strings_show_them() {
        // A number or a string typst makes of a string shows that string,
        // as a value made of one does where it's shown as text
        // (`str(label(…))`). A path isn't text, nor is a pattern (what
        // `numbering` makes of it the reader can't tell) or a setting named
        // by a string (`math.class`).
        assert_calls_show(
            "#let g(x) = int(x)\n",
            &[
                ("#int(\"12\")", "12"),
                ("#float(\"1.5\")", "1.5"),
                ("#decimal(\"3.14\")", "3.14"),
                ("#str.trim(\"  tr  \")", "tr"),
                ("#str(int(\"5\"))", "5"),
                ("#upper(str.trim(\" u \"))", "u"),
                ("#str(label(\"lab\"))", "lab"),
                ("#str(bytes(\"by\"))", "by"),
                ("#g(\"7\")", "7"),
                ("#numbering(\"1.\", 3)", ""),
                ("#read(\"data.txt\")", ""),
                ("#regex(\"ab+\")", ""),
            ],
        );
        let texts = paragraph_texts(&read("#math.class(str(\"binary\"), $+$)\n"));
        assert!(!texts.concat().contains("binary"), "{texts:?}");
        assert_eq!(
            table_texts("#let g(x) = int(x)\n#table(columns: 3, int(\"4\"), g(\"6\"), [x])\n"),
            [(Vec::new(), vec![strings(&["4", "6", "x"])])]
        );
    }

    #[test]
    fn methods_of_typsts_own_values_show_nothing() {
        // What a method of one of typst's own values computes the reader
        // can't tell, and an update or a push shows nothing.
        let doc = read(
            "#let chapter = state(\"chapter\", [])\n#chapter.update([Introduction])\n\
             #state(\"x\").update([Other])\n#let arr = ([a],)\n#arr.push([pushed])\n\
             #let d = (k: [v])\n#d.insert(\"k2\", [w])\n#counter(heading).step()\n\
             #let names = (\"Ann\", \"Bob\")\n#let j(sep) = names.join(sep)\n#j(\", \")\n\
             Text\n",
        );
        assert_eq!(paragraph_texts(&doc), ["Text"]);
        assert_eq!(
            table_texts(
                "#let s = state(\"s\")\n\
                 #table(columns: 2, s.update([upd]), [x], state(\"t\").update([u]), [y])\n"
            ),
            [(Vec::new(), vec![strings(&["", "x"]), strings(&["", "y"])])]
        );
    }

    /// The paragraphs `source` holds, read on a thread with the stack
    /// conversions run on, unless that takes longer than `limit`.
    fn paragraphs_within(source: String, limit: std::time::Duration) -> Vec<String> {
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || send.send(paragraph_texts(&read(&source))))
            .unwrap();
        receive.recv_timeout(limit).expect("reading takes too long")
    }

    #[test]
    fn function_bodies_are_read_in_linear_time_on_a_bounded_stack() {
        let limit = std::time::Duration::from_secs(10);
        // Each value bound in a body is looked at once, however often the
        // names after it use it.
        let mut lets = String::from("let x0 = 1pt; ");
        for n in 1..=200 {
            lets.push_str(&format!("let x{n} = x{} + x{}; ", n - 1, n - 1));
        }
        let source = format!(
            "#let gap() = {{ {lets}x200 }}\n#let keep(body) = {{ {lets}body }}\n\
             Text #gap() end #keep[kept].\n"
        );
        assert_eq!(paragraphs_within(source, limit), ["Text end kept."]);
        // However many names a body binds, each is found at once.
        let mut lets = String::new();
        for n in 0..20_000 {
            lets.push_str(&format!("let w{n} = size * {n}; let c{n} = red; "));
        }
        let source = format!("#let wide(size, body) = {{ {lets}body }}\nText #wide(1pt)[kept].\n");
        assert_eq!(paragraphs_within(source, limit), ["Text kept."]);
        // A long chain of operators is read without a level of the stack
        // for each, as long as typst parses on that stack.
        let chain = vec!["body"; 3_000].join(" + ");
        let source = format!("#let many(body) = {{ {chain} }}\nText #many[kept].\n");
        assert_eq!(paragraphs_within(source, limit), ["Text kept."]);
    }
}
