//! Functions the document makes of others: wrappers of tables and their
//! parts (see [`Wrapper`]) and presets made with `.with(…)` (see
//! [`Preset`]), with the arguments each gives what it calls.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use typst::syntax::SyntaxNode;
use typst::syntax::ast::{self, AstNode, Expr};

use super::names::Name;
use super::table::{array_items, table_part};
use super::{Args, MAX_NESTING, Reader, unparenthesized};

/// What the reader may spend unwrapping calls to the document's wrappers
/// of tables and their parts (see [`Reader::unwrapped`]), in bytes: of the
/// wrappers read, and of the arguments they give (see [`argument_cost`]).
/// It's this, or [`UNWRAPPED_PER_BYTE`] for each byte of a longer
/// document, which may call its wrappers in every row: no short source
/// (`#let w1() = table.cell[#w0() #w0()]`, or `#let w1(..c) = w0(..c,
/// ..c)`, again and again) makes the reader read or make a huge one.
pub(super) const MAX_UNWRAPPED: usize = 1 << 22;
/// What the reader may spend unwrapping calls for each byte of a long
/// document (see [`MAX_UNWRAPPED`]).
pub(super) const UNWRAPPED_PER_BYTE: usize = 32;

/// A function the document makes that calls a table, a grid, one of
/// their parts or another such function, and does nothing else: one it
/// defines (`#let numhead(..c) = table.header([\#], ..c)`; see
/// [`Reader::returned`]) or a preset (`table.cell.with(colspan: 2)`).
/// Called, it calls that with arguments of its own (see
/// [`Wrapper::arguments`]).
pub(super) struct Wrapper {
    /// The function as it's written: a closure, or a call to `.with`.
    node: SyntaxNode,
    /// What it calls, as that stood where the function was made: a
    /// function of typst's own, or another wrapper.
    calls: Name,
    /// The function of typst's own it ends up calling (`table.header`).
    pub(super) function: String,
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
    pub(super) fn of(node: &SyntaxNode, calls: Name, styled: bool) -> Option<Name> {
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
    pub(super) fn preset(preset: ast::FuncCall, presets: Name) -> Name {
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

/// A preset, `f.with(…)`, of a function the document defines or one the
/// reader can't see (see [`Name::Preset`]).
pub(super) struct Preset {
    /// The call to `.with`, whose arguments the function is given first.
    node: SyntaxNode,
    /// What it presets: that function, or a preset of it.
    of: Name,
    /// How many presets deep it is, itself included: no more than
    /// [`MAX_NESTING`].
    pub(super) depth: usize,
}

/// The presets a call goes through to reach the function it calls, the
/// outermost first (see [`Presets::of`]).
pub(super) struct Presets(pub(super) Vec<Rc<Preset>>);

impl Presets {
    /// The presets `name` is, and the function they preset past them all:
    /// `name` itself, and no presets, if it's no preset.
    pub(super) fn of(name: Name) -> (Presets, Name) {
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
    pub(super) fn give<'p>(&'p self, args: &Args<'p>) -> Args<'p> {
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
    pub(super) fn positional(&self) -> Option<usize> {
        let empty = Args::default();
        let given = self.give(&empty);
        given.spread.is_none().then(|| given.positional().count())
    }
}

impl Reader {
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
    pub(super) fn unwrapped<'a>(
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
}

/// The call a function's `body` returns, when that's all it does: `f(…)`,
/// `[#f(…)]` or `{ f(…) }`, after `let`s (`{ let c = …; f(..c) }`) and
/// `set` and `show` rules, and whether such rules style it.
pub(super) fn returned_call(body: Expr) -> Option<(ast::FuncCall, bool)> {
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

#[cfg(test)]
mod tests {
    use super::super::tests::{TableText, assert_calls_show, one_table, strings, table_texts};

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
}
