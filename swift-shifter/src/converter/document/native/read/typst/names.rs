//! What the names `#import` and `#let` bind stand for (see [`Name`]), as
//! far as the reader can tell without running code: they're followed into
//! the `.typ` files imported from the input's folder, through
//! destructuring, and to the members of modules and dictionaries.

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use typst::syntax::ast::{self, AstNode, Expr};

use super::signature::{Signature, std_value};
use super::table::{Cell, TABLE_PARTS};
use super::wrapper::{Preset, Wrapper, returned_call};
use super::{Args, MAX_NESTING, Reader, unparenthesized};
use crate::converter::document::native::typst as native_typst;

/// How deep imports of imports are followed (see [`Reader::module`]).
const MAX_IMPORT_DEPTH: usize = 8;
/// The largest imported file read (see [`Reader::module`]).
const MAX_IMPORT_BYTES: u64 = 1 << 20;
/// The most values an array bound by `#let` is followed with (see
/// [`Reader::value`]): no short source (`#let a = (..a, ..a)`, again and
/// again) makes the reader build a huge one.
const MAX_ARRAY: usize = 1 << 10;

/// The names a typst file binds (see [`Reader::module`]).
pub(super) type Module = Rc<HashMap<String, Name>>;

/// What a name bound by `#import` or `#let` stands for, as far as the
/// reader can tell without running code. Cloning one is cheap.
#[derive(Clone)]
pub(super) enum Name {
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
    pub(super) fn depth(&self) -> usize {
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

/// The names bound by `#import` and `#let` so far, scoped as typst scopes
/// them: those a content block binds are its own, so past it the names
/// mean what they did before (see [`Reader::scoped`]). Leaving a scope
/// undoes only what it bound, however many names there are.
#[derive(Default)]
pub(super) struct Names {
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

    pub(super) fn open_scope(&mut self) {
        self.shadowed.push(Vec::new());
    }

    pub(super) fn close_scope(&mut self) {
        let scope = self.shadowed.pop().unwrap_or_default();
        for (name, before) in scope.into_iter().rev() {
            match before {
                Some(value) => self.bound.insert(name, value),
                None => self.bound.remove(&name),
            };
        }
    }
}

impl Reader {
    /// What `callee` stands for, past parentheses: what the names the
    /// document bound make of it, else the standard library's function of
    /// that name (without `std.`). A name that's neither may come from a
    /// wildcard import the reader couldn't read: it's [`Name::Unknown`]. A
    /// method of what's no name is the method of a value typst makes (see
    /// [`Reader::method`]). A preset made with `.with(…)` is what it
    /// presets (see [`Wrapper::preset`]), through a chain of them without a
    /// level of the stack each.
    pub(super) fn resolve(&self, callee: Expr) -> Name {
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
    pub(super) fn is_method(&self, callee: Expr) -> bool {
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
    pub(super) fn std_name(&self, callee: Expr) -> Option<String> {
        match self.resolve(callee) {
            Name::Function(name) => Some(name),
            _ => None,
        }
    }

    /// `#import table: header, hline as hl`, `#import grid: *`, `#import
    /// "lib.typ" as lib: theader` and the like: the names it binds stand
    /// for what they import, a file's names where the reader can read the
    /// file (see [`Reader::module`]), and hide what they meant before.
    pub(super) fn import(&mut self, import: ast::ModuleImport) {
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
    pub(super) fn let_binding(&mut self, binding: ast::LetBinding) {
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

#[cfg(test)]
mod tests {
    use super::super::tests::{
        TableText, para, paragraph_texts, strings, table_texts, tables_in, text,
    };
    use super::super::{read, read_in};
    use crate::converter::document::native::ast::{Align, Block, Table};
    use crate::converter::document::native::write;

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
}
