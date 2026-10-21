//! What a call shows of the arguments it's given: of typst's own
//! functions, what goes to the parameters that show it (see
//! [`std_targets`]); of the functions the document defines, what their
//! bodies show (see [`Signature`]); and of those the reader can't see,
//! their content and strings, but for the strings they take for settings:
//! those before their first content, and those given by a name other than
//! one for what they show (see [`SHOWN_NAMES`]).

use std::collections::HashMap;
use std::rc::Rc;

use typst::foundations::{
    Array, CastInfo, Content, Decimal, Func, NativeParamInfo, Str, Symbol, Type, Value,
};
use typst::syntax::ast::{self, AstNode, Expr};
use typst::syntax::{RootedPath, SyntaxNode};

use super::names::Name;
use super::wrapper::Presets;
use super::{Args, MAX_NESTING, Reader, unparenthesized};
use crate::converter::document::native::typst as native_typst;

/// How deep the body of a function the document defines is read for what
/// it shows (see [`Reader::shown_names`] and [`Reader::may_show`]): deeper
/// than any body is written but for chains typst parses past its nesting
/// limit (`x.f().g()…`), past which each name is taken to show, so the
/// reader fits the 2 MiB stack conversions run on. Chains of operators
/// (`a + b + …`) are read along without going deeper.
const MAX_BODY_DEPTH: usize = 4 * MAX_NESTING;

/// The names a function the reader can't see is taken to show what it's
/// given by, strings included: those of the text a box or a template
/// sets (`showybox(title: "Hint")`). By any other name, it takes a
/// string, or a value the reader can't tell, for a setting (see
/// [`Reader::shown_by_name`]).
const SHOWN_NAMES: &[&str] = &[
    "title", "subtitle", "caption", "body", "content", "text", "header", "footer",
];

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

/// What a function the document defines shows of each of its parameters,
/// as far as the reader can tell from its body (see
/// [`Reader::shown_names`]), so of the strings and content given to it.
pub(super) struct Signature {
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
pub(super) enum Given<'a> {
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

impl Reader {
    /// The positional arguments typst shows of a call to `name`, one of its
    /// own functions, by the parameters they go to (see [`std_targets`]):
    /// a string, where it may be shown, and what may be content (see
    /// [`Reader::is_content`]), where content or a string may be. Where
    /// only a string is shown, a value made of one shows that string (see
    /// [`Reader::made_of`]). A function returning neither content nor
    /// text shows nothing (`rgb("#f00")`).
    pub(super) fn std_shown_args<'a>(&self, name: &str, args: &Args<'a>) -> Vec<Expr<'a>> {
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
    pub(super) fn custom_shown_args<'a>(
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
    pub(super) fn is_content(&self, expr: Expr) -> bool {
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

    /// What the function `closure`, of the document's own, stands for: one
    /// returning a setting shows nothing (see [`Reader::may_show`]), and
    /// any other shows the strings and content given for the parameters its
    /// body shows (see [`Reader::shown_names`]).
    pub(super) fn defined(&self, closure: ast::Closure) -> Name {
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
pub(super) fn std_value(name: &str) -> Option<&'static Value> {
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
pub(super) fn std_symbol_text(path: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::super::read;
    use super::super::tests::{
        assert_calls_show, one_table, paragraph_texts, strings, table_texts, text,
    };
    use crate::converter::document::native::ast::Block;
    use crate::converter::document::native::write;

    #[test]
    fn unknown_calls_keep_their_content() {
        let doc = read("#custom(width: 3pt)[Visible] #box[boxed] #v(1em)\n");
        assert_eq!(doc.blocks, [Block::Paragraph(vec![text("Visible boxed")])]);
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
