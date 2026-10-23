//! Typst math → TeX, for the AST (which keeps math as TeX). Covers the
//! notation documents commonly use: symbols, fractions, attachments, roots,
//! matrices and the usual functions. Anything else becomes
//! `\operatorname{…}` or plain text rather than failing.
//!
//! Line breaks and alignment points follow typst's layout, which sets math
//! in runs. The equation is a run, and so is each operand, script and
//! function argument inside it, while a delimited group and what `lr(…)`
//! or `display(…)` passes through belong to the run around them. A run's
//! line breaks split it into rows, but for one that ends it, and its
//! alignment points align those rows; in a run with a single row they do
//! nothing. Rows without alignment points are centred in an equation of
//! its own, and start at the left in one in text.
//!
//! Characters stay as typed (`α`, `→`, `ℝ`), which KaTeX, MathJax,
//! pulldown-latex and mitex all take and draw as typst does. Only TeX's
//! specials are escaped. LaTeX's own fonts lack the others, so the LaTeX
//! writer gives them their names (see `write::latex_math`).

use typst::ecow::EcoString;
use typst::foundations::Value;
use typst::math::MathSize;
use typst::syntax::ast::{self, AstNode, Expr, MathTextKind};
use typst::utils::default_math_class;
use unicode_math_class::MathClass;

use crate::converter::document::native::ast::{is_variation_selector, push_tex};
use crate::converter::document::native::typst::library;

/// TeX for a typst equation, which typst sets in display size on a line of
/// its own (`$ x $`), and in text size in a line of text (`$x$`), where
/// rows that start at the left have their last on the baseline, as
/// `aligned`'s `[b]` sets them.
pub fn equation(eq: ast::Equation) -> String {
    if eq.block() {
        return run(Expr::Math(eq.body()), MathSize::Display, Align::Center);
    }
    let tex = run(Expr::Math(eq.body()), MathSize::Text, Align::Start);
    match tex.strip_prefix("\\begin{aligned}&") {
        Some(rows) => format!("\\begin{{aligned}}[b]&{rows}"),
        None => tex,
    }
}

/// How typst aligns the rows of a run that has no alignment points: at
/// the centre in an equation of its own, which typst centres, and at the
/// start in one in text, as a paragraph starts its lines.
#[derive(Clone, Copy)]
enum Align {
    Center,
    Start,
}

/// TeX for math that typst sets as a run of its own, in size `size`: an
/// equation, or an operand, script or argument inside one. A run with
/// several rows, which typst stacks even inline, gets an environment for
/// them. Rows with alignment points get `aligned`, and rows without get
/// `gathered`, which centres them, or `aligned` with each in its
/// left-aligned column (see [`Grid::LEFT`]), as `align` has them. In
/// script size they get `\substack`, which centres them and can't align
/// them. Nor can it start them at the left, as typst does in text:
/// `subarray` could, but mitex sets that in text size. A single row gets
/// no environment, so its alignment points are left out.
fn run(expr: Expr, size: MathSize, align: Align) -> String {
    let setting = |grid| Setting {
        size,
        align,
        grid,
        last: true,
    };
    if !in_whole_run(expr, &|expr| matches!(expr, Expr::Linebreak(_))) {
        return trimmed(expr, setting(Grid::ROW));
    }
    if is_script(size) {
        return format!("\\substack{{{}}}", trimmed(expr, setting(Grid::STACK)));
    }
    let aligned = in_whole_run(expr, &|expr| matches!(expr, Expr::MathAlignPoint(_)));
    let (environment, grid) = match (aligned, align) {
        (true, _) => ("aligned", Grid::ALIGNED),
        (false, Align::Center) => ("gathered", Grid::STACK),
        (false, Align::Start) => ("aligned", Grid::LEFT),
    };
    let tex = trimmed(expr, setting(grid));
    let rows = if grid.left {
        format!("&{{{tex}}}")
    } else {
        format!("{}{tex}", bracket_guard(&tex))
    };
    format!("\\begin{{{environment}}}{rows}\\end{{{environment}}}")
}

/// How typst sets a part of an equation, as far as its TeX depends on it.
#[derive(Clone, Copy)]
struct Setting {
    /// The size it's set in, as TeX sets it: typst's, but in the cells of
    /// `mat`, `vec` and `cases`, which TeX sets in text size and typst a
    /// size smaller than the math around them, and in what `display(…)`
    /// and the other sizes pass through (see [`CallKind::Size`]).
    size: MathSize,
    /// How the equation aligns rows without alignment points.
    align: Align,
    /// Which of its run's line breaks and alignment points the TeX around
    /// it takes.
    grid: Grid,
    /// Whether it's the last thing typst sets in its run or in an `lr(…)`
    /// group, where typst drops a line break: `a \ ` is a single row. A
    /// delimited group's body, or a fence's, ends before its closing
    /// delimiter, so a line break at its end is one of the run's.
    last: bool,
}

/// The size of a script, a limit or a brace's annotation in math of size
/// `size`, as typst and TeX set it.
fn script_size(size: MathSize) -> MathSize {
    match size {
        MathSize::Display | MathSize::Text => MathSize::Script,
        MathSize::Script | MathSize::ScriptScript => MathSize::ScriptScript,
    }
}

/// The size of the parts of a fraction in math of size `size`, as typst
/// and TeX set them.
fn fraction_size(size: MathSize) -> MathSize {
    match size {
        MathSize::Display => MathSize::Text,
        MathSize::Text => MathSize::Script,
        MathSize::Script | MathSize::ScriptScript => MathSize::ScriptScript,
    }
}

/// Whether `size` is a script size, where typst and TeX give relations,
/// binary operators and punctuation no space of their own.
fn is_script(size: MathSize) -> bool {
    size <= MathSize::Script
}

/// What goes between `\begin{aligned}` (or `gathered`) or `\\` and the
/// `tex` after them: an empty group when `tex` starts with `[`, which they
/// would read, even past spaces, as the start of an optional argument
/// (`aligned`'s `[t]`, or the `[2pt]` of extra space after a row).
fn bracket_guard(tex: &str) -> &'static str {
    if tex.trim_start().starts_with('[') {
        "{}"
    } else {
        ""
    }
}

/// Whether typst drops `expr`: a line break that's the last thing in its
/// run or `lr(…)` group (`last`), with no row after it.
fn dropped(expr: Expr, last: bool) -> bool {
    last && matches!(expr, Expr::Linebreak(_))
}

/// Whether `found` holds for `expr` or for anything typst sets in the same
/// run (see [`same_run`]), but for a line break that it drops (see
/// [`dropped`]). `last` says whether `expr` is the last thing in its run
/// or `lr(…)` group.
fn in_run(expr: Expr, last: bool, found: &impl Fn(Expr) -> bool) -> bool {
    (!dropped(expr, last) && found(expr))
        || same_run(expr, last)
            .into_iter()
            .any(|(expr, last)| in_run(expr, last, found))
}

/// [`in_run`] for `expr` that is a run of its own, so last in it.
fn in_whole_run(expr: Expr, found: &impl Fn(Expr) -> bool) -> bool {
    in_run(expr, true, found)
}

/// [`count_in_run`] for `expr` that is a run of its own, so last in it.
fn count_in_whole_run(expr: Expr, found: &impl Fn(Expr) -> bool) -> usize {
    count_in_run(expr, true, found)
}

/// How many of `expr` and the things typst sets in the same run (see
/// [`in_run`]) `found` holds for.
fn count_in_run(expr: Expr, last: bool, found: &impl Fn(Expr) -> bool) -> usize {
    let inside: usize = same_run(expr, last)
        .into_iter()
        .map(|(expr, last)| count_in_run(expr, last, found))
        .sum();
    usize::from(!dropped(expr, last) && found(expr)) + inside
}

/// What typst sets in the same run as `expr`, each with whether it's the
/// last thing in the run or an `lr(…)` group (see [`Setting::last`]), when
/// `expr` is (`last`): the parts of a sequence, the body of a delimited
/// group and the arguments that calls set in the run around them (see
/// [`CallKind`]). Operands, scripts and other arguments are runs of their
/// own.
fn same_run(expr: Expr, last: bool) -> Vec<(Expr, bool)> {
    match expr {
        Expr::Math(math) => parts(math.exprs(), last),
        Expr::MathDelimited(delimited) => parts(delimited.body().exprs(), false),
        Expr::MathCall(call) => match call_kind(&callee_name(call)) {
            CallKind::Size => styled_body(call)
                .map(|body| (body, last))
                .into_iter()
                .collect(),
            CallKind::Lr => positional(call)
                .last()
                .map(|body| (body, true))
                .into_iter()
                .collect(),
            CallKind::Fence(_) | CallKind::Other => {
                positional(call).map(|arg| (arg, false)).collect()
            }
            CallKind::Command(_) => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The parts of a sequence, each with whether it's the last thing in its
/// run or `lr(…)` group, when the sequence is (`last`): the last part that
/// typst sets anything for, past spaces and `#none`.
fn parts<'a>(exprs: impl Iterator<Item = Expr<'a>>, last: bool) -> Vec<(Expr<'a>, bool)> {
    let exprs: Vec<Expr> = exprs.collect();
    let end = exprs
        .iter()
        .rposition(|expr| !matches!(expr, Expr::Space(_) | Expr::None(_)));
    exprs
        .into_iter()
        .enumerate()
        .map(|(i, expr)| (expr, last && Some(i) == end))
        .collect()
}

/// Which of a run's line breaks and alignment points the TeX around it
/// takes, as `\\` (`rows`) and `&` (`columns`). It leaves the others out,
/// as typst ignores them where it doesn't break or align.
#[derive(Clone, Copy)]
struct Grid {
    rows: bool,
    columns: bool,
    /// Whether each row goes in the second column of `aligned` (see
    /// [`Grid::LEFT`]).
    left: bool,
}

impl Grid {
    /// A single row. Typst ignores alignment points with no other row to
    /// align to, and TeX rejects an `&` outside an environment.
    const ROW: Grid = Grid {
        rows: false,
        columns: false,
        left: false,
    };
    /// The rows of `\substack` or `gathered`, which can't align them.
    const STACK: Grid = Grid {
        rows: true,
        columns: false,
        left: false,
    };
    /// The rows and columns of `aligned`.
    const ALIGNED: Grid = Grid {
        rows: true,
        columns: true,
        left: false,
    };
    /// Rows without columns in `aligned`, each after an `&`, in its second
    /// column, which starts them at the left. Each is in braces: amsmath
    /// sets `{}` before what that column holds, which would make an
    /// operator that starts the row binary.
    const LEFT: Grid = Grid {
        rows: true,
        columns: false,
        left: true,
    };
    /// A cell of `mat`, `vec` or `cases`. Typst ignores its line breaks,
    /// and aligns its alignment points down the column, which the
    /// environment's extra columns stand in for.
    const CELL: Grid = Grid {
        rows: false,
        columns: true,
        left: false,
    };

    /// The TeX between two rows.
    fn row_break(self) -> &'static str {
        if self.left { "} \\\\ &{" } else { "\\\\ " }
    }

    /// Whether the run's line breaks or alignment points that this grid
    /// takes are among `parts` (see [`same_run`]), which then can't go
    /// between `\left` and `\right`.
    fn splits(self, parts: &[(Expr, bool)]) -> bool {
        let splits_at = |expr: Expr| match expr {
            Expr::Linebreak(_) => self.rows,
            Expr::MathAlignPoint(_) => self.columns,
            _ => false,
        };
        parts
            .iter()
            .any(|&(expr, last)| in_run(expr, last, &splits_at))
    }
}

/// A space that typst keeps where no string beside it can hold it (see
/// [`joined`]): as wide as a space of typst's math font. TeX's `\ ` would
/// do in LaTeX, but pulldown-latex shows it as the text `&nbsp;`, and
/// draws two spaces for the one of `\text{ }`.
const SPACE: &str = "\\hspace{0.333em}";

/// The TeX of each of `parts`, a sequence (see [`same_run`]), one after
/// another.
fn joined(parts: Vec<(Expr, bool)>, setting: Setting) -> String {
    let exprs: Vec<Expr> = parts.iter().map(|&(expr, _)| expr).collect();
    // The spaces that typst keeps (see [`kept_spaces`]) go where it puts
    // them: right after the thing before, so on the left of an alignment
    // point between them. TeX ignores spaces in math, so each goes in the
    // text of a string beside it: the one before it, or else the one after
    // it if no alignment point comes between them. With neither, it's a
    // space of its own, after the thing before.
    let mut padding = vec![("", ""); exprs.len()];
    let mut spaced = vec![false; exprs.len()];
    for (left, right) in kept_spaces(&exprs, setting) {
        let aligned = setting.grid.columns
            && exprs[left..right]
                .iter()
                .any(|expr| matches!(expr, Expr::MathAlignPoint(_)));
        if styled_string(exprs[left]).is_some() {
            padding[left].1 = " ";
        } else if styled_string(exprs[right]).is_some() && !aligned {
            padding[right].0 = " ";
        } else {
            spaced[left] = true;
        }
    }
    let mut out = String::new();
    for (i, (expr, last)) in parts.into_iter().enumerate() {
        let setting = Setting { last, ..setting };
        let (before, after) = padding[i];
        let tex = if before.is_empty() && after.is_empty() {
            self::expr(expr, setting)
        } else {
            padded(expr, before, after).unwrap_or_else(|| self::expr(expr, setting))
        };
        if out.trim_end().ends_with("\\\\") {
            // A row break, from this sequence or the end of a part of it.
            out.push_str(bracket_guard(&tex));
        }
        push_tex(&mut out, &tex);
        if spaced[i] {
            out.push_str(SPACE);
        }
    }
    out
}

/// `expr`'s TeX, set as `setting` has it. The parser has already dropped
/// the parentheses that group an operand, like those of `(a+b)/c`, which
/// TeX's braces replace.
fn expr(expr: Expr, setting: Setting) -> String {
    let Setting {
        size,
        align,
        grid,
        last,
    } = setting;
    match expr {
        Expr::Math(_) => joined(same_run(expr, last), setting),
        Expr::MathText(text) => match text.get() {
            MathTextKind::Grapheme(g) => grapheme(g),
            MathTextKind::Number(n) => n.to_string(),
        },
        Expr::Text(text) => grapheme(text.get()),
        Expr::MathIdent(ident) => symbol(ident.as_str()),
        Expr::MathFieldAccess(access) => symbol(&access.to_untyped().full_text()),
        Expr::MathShorthand(shorthand) => character(shorthand.get()),
        Expr::MathAlignPoint(_) if grid.columns => "&".to_string(),
        Expr::Linebreak(_) if grid.rows && !dropped(expr, last) => grid.row_break().to_string(),
        Expr::MathAlignPoint(_) | Expr::Linebreak(_) => String::new(),
        Expr::Space(_) => " ".to_string(),
        Expr::Escape(escape) => character(escape.get()),
        Expr::Str(text) => string(&text.get()),
        // Typst shows nothing for `#none`.
        Expr::None(_) => String::new(),
        Expr::MathPrimes(primes) => "'".repeat(primes.count()),
        Expr::MathFrac(frac) => {
            let size = fraction_size(size);
            let (num, denom) = (run(frac.num(), size, align), run(frac.denom(), size, align));
            format!("\\frac{{{num}}}{{{denom}}}")
        }
        Expr::MathAttach(attach) => {
            let mut out = group_base(attach.base(), size, align);
            if let Some(primes) = attach.primes() {
                out.push_str(&"'".repeat(primes.count()));
            }
            let script = script_size(size);
            if let Some(bottom) = attach.bottom() {
                out.push_str(&format!("_{{{}}}", run(bottom, script, align)));
            }
            if let Some(top) = attach.top() {
                out.push_str(&format!("^{{{}}}", run(top, script, align)));
            }
            out
        }
        Expr::MathRoot(root) => {
            let radicand = run(root.radicand(), size, align);
            match root.index() {
                Some(index) => format!("\\sqrt[{index}]{{{radicand}}}"),
                None => format!("\\sqrt{{{radicand}}}"),
            }
        }
        Expr::MathDelimited(delimited) => {
            let body = same_run(expr, last);
            let split = grid.splits(&body);
            fenced(delimiter_chars(delimited), &joined(body, setting), split)
        }
        Expr::MathCall(call) => self::call(call, setting),
        // `#expr` embedded code, and anything else: its source text, but
        // for code that names a symbol (`#sym.alpha`) and typst's spacing
        // (`#h(1em)`).
        other => {
            let source = other.to_untyped().full_text();
            let code = unparenthesized(other);
            let path = code.to_untyped().full_text();
            match code {
                Expr::FieldAccess(_) if symbol_name(&path) != path => symbol(&path),
                Expr::FuncCall(call) => {
                    horizontal_space(call).unwrap_or_else(|| math_text(&source))
                }
                _ => math_text(&source),
            }
        }
    }
}

/// `expr`'s TeX as `setting` has it, without the spaces around it.
fn trimmed(expr: Expr, setting: Setting) -> String {
    self::expr(expr, setting).trim().to_string()
}

/// An attachment's base, a run of its own in size `size`: anything longer
/// than one character or command gets braces. A lone line break or
/// alignment point, which typst sets nothing for there, leaves the base
/// empty: the scripts sit on nothing.
fn group_base(expr: Expr, size: MathSize, align: Align) -> String {
    let tex = run(expr, size, align);
    let simple = tex.chars().count() == 1
        || (tex.starts_with('\\') && tex[1..].chars().all(|c| c.is_ascii_alphabetic()));
    if simple { tex } else { format!("{{{tex}}}") }
}

/// A single character as `character` writes it, and anything longer as it
/// is. A character that stays as it is keeps a variation selector after
/// it, which picks the glyph of a CJK ideograph or an emoji; TeX's specials
/// have none to pick.
fn grapheme(text: &str) -> String {
    match single_char(text) {
        Some(c) if character(c) != c.to_string() => character(c),
        _ => text.to_string(),
    }
}

/// A character in math, however it was typed (as itself, a shorthand like
/// `->` or an escape): one of TeX's specials, escaped so that it shows as
/// itself, or else the character as it is.
fn character(c: char) -> String {
    let tex = match c {
        '{' => "\\{",
        '}' => "\\}",
        '#' => "\\#",
        '$' => "\\$",
        '%' => "\\%",
        '&' => "\\&",
        '_' => "\\_",
        '\\' => "\\backslash",
        // Ordinary symbols in typst, where TeX would read a superscript
        // and a space.
        '^' => "\\hat{}",
        '~' => "\\mathord{\\sim}",
        // The minus sign, which TeX has typed as a hyphen.
        '−' => "-",
        c => return c.to_string(),
    };
    tex.to_string()
}

/// The pairs of `exprs`, set as `setting` has them, that typst keeps the
/// space between as it does in text: where the source has one, beside text
/// (see [`is_spaced`]), unless their math classes space them themselves
/// (see [`keeps_space`]). Typst drops a space beside its own spacing
/// (`quad`, `#h(1em)`) and at a line break, and keeps one past an
/// alignment point or `#none`, which it sets nothing for.
fn kept_spaces(exprs: &[Expr], setting: Setting) -> Vec<(usize, usize)> {
    use MathClass::*;
    let mut kept = Vec::new();
    // The thing set before, and its class, and whether a space follows it.
    let mut last: Option<(usize, MathClass)> = None;
    let mut space = false;
    for (i, &expr) in exprs.iter().enumerate() {
        match expr {
            Expr::Space(_) => {
                space = last.is_some();
                continue;
            }
            Expr::Linebreak(_) if setting.grid.rows => {
                (last, space) = (None, false);
                continue;
            }
            Expr::Linebreak(_) | Expr::MathAlignPoint(_) | Expr::None(_) => continue,
            expr if is_spacing(expr) => {
                (last, space) = (None, false);
                // `dif` sets its `d` after the spacing.
                if !is_differential(expr) {
                    continue;
                }
            }
            _ => {}
        }
        let Some(mut class) = math_class(expr) else {
            continue;
        };
        // A `+` or `-` after an operand is a binary operator.
        if class == Vary
            && last.is_some_and(|(_, l)| matches!(l, Normal | Alphabetic | Closing | Fence))
        {
            class = Binary;
        }
        if let Some((left, l)) = last
            && space
            && keeps_space(l, class, is_script(setting.size))
            && (is_spaced(exprs[left], l) || is_spaced(expr, class))
        {
            kept.push((left, i));
        }
        (last, space) = (Some((i, class)), false);
    }
    kept
}

/// Whether typst leaves the space between things of math classes `left`
/// and `right` to their being spaced (see [`is_spaced`]), rather than
/// spacing them by their classes. It never does beside a large operator,
/// before punctuation, on the inside of a delimiter or between relations.
/// Beside other relations and binary operators, and after punctuation, it
/// only does in script size (`script`), where it gives them no space of
/// their own.
fn keeps_space(left: MathClass, right: MathClass, script: bool) -> bool {
    use MathClass::*;
    let never = matches!(left, Opening | Large)
        || matches!(right, Punctuation | Closing | Large)
        || (left, right) == (Relation, Relation);
    let spaced_by_class =
        matches!(left, Punctuation | Relation | Binary) || matches!(right, Relation | Binary);
    !never && (script || !spaced_by_class)
}

/// Whether typst sets `expr`, of math class `class`, with the spaces beside
/// it that the source has: a fence like `|`, or text (see [`is_text`]),
/// even styled (see [`styled_string`]), of an ordinary class.
fn is_spaced(expr: Expr, class: MathClass) -> bool {
    class == MathClass::Fence
        || (matches!(class, MathClass::Normal | MathClass::Alphabetic)
            && styled_string(expr).is_some_and(|text| is_text(&text.get())))
}

/// Whether typst sets string `text` in math as text: a line, with at most
/// a line break after it, that isn't a number. It sets a number as math,
/// and stacks the lines of a string with more, or with none.
fn is_text(text: &str) -> bool {
    let mut lines = typst::syntax::split_newlines(text);
    lines.pop_if(|line| line.is_empty());
    let [line] = lines[..] else {
        return false;
    };
    // Digits, with at most one decimal point.
    let dots = line.matches('.').count();
    let number =
        line.chars().all(|c| c.is_ascii_digit() || c == '.') && dots < line.len() && dots <= 1;
    !number
}

/// Whether typst sets `expr` as spacing of its own (`quad`, `#h(1em)`),
/// which takes the place of what it would put between the things beside
/// it, or starts with some, as `dif` does (see [`is_differential`]).
fn is_spacing(expr: Expr) -> bool {
    match expr {
        Expr::MathIdent(ident) => {
            matches!(ident.as_str(), "thin" | "med" | "thick" | "quad" | "wide")
                || is_differential(expr)
        }
        Expr::FuncCall(call) => call.callee().to_untyped().full_text() == "h",
        _ => false,
    }
}

/// Whether `expr` is typst's `dif` or `Dif`: thin spacing, and then an
/// upright `d` or `D`.
fn is_differential(expr: Expr) -> bool {
    matches!(expr, Expr::MathIdent(ident) if matches!(ident.as_str(), "dif" | "Dif"))
}

/// The math class typst gives `expr`, which spaces it from its neighbours:
/// a character's own, a large operator's for a function name like `sin`,
/// the one `class(…)` gives, a styled body's own, and an ordinary one for
/// anything else typst sets. `None` for what typst sets nothing for, like
/// a space.
fn math_class(expr: Expr) -> Option<MathClass> {
    let of = |c: char| default_math_class(c).unwrap_or(MathClass::Normal);
    let named = |name: &str| match name {
        name if FUNCTIONS.contains(&name) || name == "op" => MathClass::Large,
        "dif" | "Dif" => MathClass::Unary,
        name => symbol_char(name).map_or(MathClass::Normal, of),
    };
    Some(match expr {
        Expr::Space(_) | Expr::Linebreak(_) | Expr::MathAlignPoint(_) | Expr::None(_) => {
            return None;
        }
        Expr::MathText(text) => match text.get() {
            MathTextKind::Grapheme(g) => single_char(g).map_or(MathClass::Normal, of),
            MathTextKind::Number(_) => MathClass::Normal,
        },
        Expr::MathShorthand(shorthand) => of(shorthand.get()),
        Expr::Escape(escape) => of(escape.get()),
        Expr::MathIdent(_) | Expr::MathFieldAccess(_) | Expr::FieldAccess(_) => {
            named(&expr.to_untyped().full_text())
        }
        Expr::MathCall(call) => {
            let name = callee_name(call);
            if let CallKind::Command(Command::Class) = call_kind(&name)
                && let Some(Expr::Str(class)) = positional(call).next()
            {
                return Some(class_named(&class.get()));
            }
            match styled_body(call) {
                Some(body) => return math_class(body),
                None => named(&name),
            }
        }
        Expr::MathAttach(attach) => return math_class(attach.base()),
        Expr::Str(_) => MathClass::Alphabetic,
        _ => MathClass::Normal,
    })
}

/// The math class that `class(…)` names: `"relation"` and the others.
fn class_named(name: &str) -> MathClass {
    use MathClass::*;
    match name {
        "alphabetic" => Alphabetic,
        "binary" => Binary,
        "closing" => Closing,
        "diacritic" => Diacritic,
        "fence" => Fence,
        "glyph-part" => GlyphPart,
        "large" => Large,
        "opening" => Opening,
        "punctuation" => Punctuation,
        "relation" => Relation,
        "space" => Space,
        "unary" => Unary,
        "vary" => Vary,
        "special" => Special,
        _ => Normal,
    }
}

/// The string that `expr` is, or that a call styling it sets
/// (`bold("if")`, see [`styled_body`]).
fn styled_string(expr: Expr) -> Option<ast::Str> {
    match expr {
        Expr::Str(text) => Some(text),
        Expr::MathCall(call) => styled_string(styled_body(call)?),
        _ => None,
    }
}

/// The TeX of `expr` as the string it is or styles (see
/// [`styled_string`]), with `before` and `after` around its text. `None`
/// for anything else.
fn padded(expr: Expr, before: &str, after: &str) -> Option<String> {
    match expr {
        Expr::Str(text) => Some(string(&format!("{before}{}{after}", text.get()))),
        Expr::MathCall(call) => {
            let body = padded(styled_body(call)?, before, after)?;
            Some(match style_command(call) {
                Some(command) => format!("\\{command}{{{body}}}"),
                None => body,
            })
        }
        _ => None,
    }
}

/// A string in math, which typst sets as text.
fn string(text: &str) -> String {
    format!("\\text{{{}}}", text_arg(text))
}

/// Text inside `\text{…}`, with TeX's specials as text has them.
fn text_arg(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' | '}' | '#' | '$' | '%' | '&' | '_' => {
                out.push('\\');
                out.push(c);
            }
            '^' => out.push_str("\\^{}"),
            '~' => out.push_str("\\~{}"),
            c => out.push(c),
        }
    }
    out
}

/// Text set in math as it is, or as an operator's name: its characters as
/// [`character`] writes them in math.
fn math_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        push_tex(&mut out, &character(c));
    }
    out
}

/// A string that `op(…)` sets as an operator's name, for `\operatorname`:
/// its characters as math has them (see [`math_text`]), with a no-break
/// space for each space, which TeX would ignore. pulldown-latex shows the
/// name as it's written, and the LaTeX writer names the no-break space
/// `~`, which is a space in math too.
fn operator_name(text: &str) -> String {
    math_text(&text.replace(' ', "\u{A0}"))
}

/// TeX's name for a delimiter that `\left` and `\right` can stretch, or
/// `None` for one with no name that LaTeX (with amssymb), pulldown-latex
/// and mitex all know, like `⟦` or `⎰`.
fn stretchy(c: char) -> Option<&'static str> {
    let tex = match c {
        '(' => "(",
        ')' => ")",
        '[' => "[",
        ']' => "]",
        '{' => "\\{",
        '}' => "\\}",
        '|' => "|",
        '‖' => "\\|",
        '⟨' => "\\langle",
        '⟩' => "\\rangle",
        '⌈' => "\\lceil",
        '⌉' => "\\rceil",
        '⌊' => "\\lfloor",
        '⌋' => "\\rfloor",
        '⟮' => "\\lgroup",
        '⟯' => "\\rgroup",
        '⌜' => "\\ulcorner",
        '⌝' => "\\urcorner",
        '⌞' => "\\llcorner",
        '⌟' => "\\lrcorner",
        _ => return None,
    };
    Some(tex)
}

/// The characters of a delimited group's opening and closing tokens.
fn delimiter_chars(delimited: ast::MathDelimited) -> (Option<char>, Option<char>) {
    (
        delimiter_token(delimited.open()),
        delimiter_token(delimited.close()),
    )
}

/// The character of a delimited group's opening or closing token: text, or
/// the `[|` and `|]` shorthands.
fn delimiter_token(expr: Expr) -> Option<char> {
    match expr {
        Expr::MathText(text) => match text.get() {
            MathTextKind::Grapheme(g) => single_char(g),
            MathTextKind::Number(_) => None,
        },
        Expr::MathShorthand(shorthand) => Some(shorthand.get()),
        _ => None,
    }
}

/// The only character of `text`, if it has just one, past the variation
/// selectors that typst gives some of its symbols (`arrow.r.hook` is `↪`
/// as text, not an emoji), which TeX has no use for.
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars().filter(|&c| !is_variation_selector(c));
    chars.next().filter(|_| chars.next().is_none())
}

/// Typst symbol names → TeX. Greek letters, operators, relations, arrows,
/// sets and function names get TeX's names, and any other symbol in typst's
/// table its character. Anything else that's a single letter stays as it
/// is, and a longer name becomes an operator name.
fn symbol(name: &str) -> String {
    const GREEK: &[&str] = &[
        "alpha", "beta", "gamma", "delta", "zeta", "eta", "theta", "iota", "kappa", "lambda", "mu",
        "nu", "xi", "pi", "rho", "sigma", "tau", "upsilon", "chi", "psi", "omega", "Gamma",
        "Delta", "Theta", "Lambda", "Xi", "Pi", "Sigma", "Upsilon", "Phi", "Psi", "Omega",
    ];
    let name = symbol_name(name);
    if GREEK.contains(&name) || FUNCTIONS.contains(&name) {
        return format!("\\{name}");
    }
    let tex = match name {
        // Typst's `epsilon` and `phi` are ε and φ, whose TeX names are the
        // `var` ones.
        "epsilon" => "\\varepsilon",
        "epsilon.alt" => "\\epsilon",
        "phi" => "\\varphi",
        "phi.alt" => "\\phi",
        "theta.alt" => "\\vartheta",
        "pi.alt" => "\\varpi",
        "rho.alt" => "\\varrho",
        "sigma.alt" => "\\varsigma",
        "infinity" | "oo" => "\\infty",
        "sum" => "\\sum",
        "product" => "\\prod",
        "integral" => "\\int",
        "integral.double" => "\\iint",
        "integral.triple" => "\\iiint",
        "integral.cont" => "\\oint",
        "partial" | "diff" => "\\partial",
        // Typst's upright differential operators.
        "dif" => "\\mathrm{d}",
        "Dif" => "\\mathrm{D}",
        "nabla" => "\\nabla",
        // Typst's dots stay on the baseline, where amsmath's `\dots` would
        // centre them before an operator.
        "dots" | "dots.h" => "\\ldots",
        "dots.c" => "\\cdots",
        "dots.v" => "\\vdots",
        "dots.down" => "\\ddots",
        "times" => "\\times",
        "div" => "\\div",
        "dot" | "dot.op" => "\\cdot",
        "plus.minus" => "\\pm",
        "minus.plus" => "\\mp",
        "plus.circle" | "plus.o" => "\\oplus",
        "times.circle" | "times.o" => "\\otimes",
        "eq.not" => "\\neq",
        "lt.eq" => "\\leq",
        "gt.eq" => "\\geq",
        "lt.double" | "lt.lt" => "\\ll",
        "gt.double" | "gt.gt" => "\\gg",
        "approx" => "\\approx",
        "equiv" => "\\equiv",
        "prop" => "\\propto",
        "tilde.op" => "\\sim",
        "in" => "\\in",
        "in.not" => "\\notin",
        "subset" => "\\subset",
        "subset.eq" => "\\subseteq",
        "supset" => "\\supset",
        "supset.eq" => "\\supseteq",
        "union" => "\\cup",
        "union.big" => "\\bigcup",
        "sect" | "inter" => "\\cap",
        "sect.big" | "inter.big" => "\\bigcap",
        "emptyset" | "nothing" => "\\emptyset",
        "forall" => "\\forall",
        "exists" => "\\exists",
        "exists.not" => "\\nexists",
        "not" => "\\neg",
        "and" => "\\land",
        "or" => "\\lor",
        "top" => "\\top",
        "bot" => "\\bot",
        "angle" => "\\angle",
        "angle.l" | "chevron.l" => "\\langle",
        "angle.r" | "chevron.r" => "\\rangle",
        "arrow.r" => "\\rightarrow",
        "arrow.l" => "\\leftarrow",
        "arrow.l.r" => "\\leftrightarrow",
        "arrow.r.double" => "\\Rightarrow",
        "arrow.l.double" => "\\Leftarrow",
        "arrow.l.r.double" => "\\Leftrightarrow",
        "arrow.r.long" => "\\longrightarrow",
        "arrow.r.bar" | "mapsto" => "\\mapsto",
        "arrow.t" => "\\uparrow",
        "arrow.b" => "\\downarrow",
        "star" | "star.op" => "\\star",
        "circle.stroked.tiny" | "compose" => "\\circ",
        "ell" => "\\ell",
        "planck" | "planck.reduce" => "\\hbar",
        "aleph" => "\\aleph",
        "RR" => "\\mathbb{R}",
        "NN" => "\\mathbb{N}",
        "ZZ" => "\\mathbb{Z}",
        "QQ" => "\\mathbb{Q}",
        "CC" => "\\mathbb{C}",
        "quad" => "\\quad",
        "wide" => "\\qquad",
        "thin" => "\\,",
        "med" => "\\:",
        "thick" => "\\;",
        "space" => "\\ ",
        _ => {
            return match symbol_char(name) {
                Some(c) => character(c),
                None if name.chars().count() == 1 => grapheme(name),
                None => format!("\\operatorname{{{}}}", math_text(name)),
            };
        }
    };
    tex.to_string()
}

/// Typst's `#h(…)`, by a length in a unit LaTeX has, as `\hspace`: typst's
/// point is LaTeX's big point. `None` for anything else.
fn horizontal_space(call: ast::FuncCall) -> Option<String> {
    if call.callee().to_untyped().full_text() != "h" {
        return None;
    }
    let Some(ast::Arg::Pos(Expr::Numeric(length))) = call.args().items().next() else {
        return None;
    };
    let (value, unit) = length.get();
    let unit = match unit {
        ast::Unit::Pt => "bp",
        ast::Unit::Mm => "mm",
        ast::Unit::Cm => "cm",
        ast::Unit::In => "in",
        ast::Unit::Em => "em",
        _ => return None,
    };
    Some(format!("\\hspace{{{value}{unit}}}"))
}

/// Typst's operator names that TeX has commands of the same name for.
const FUNCTIONS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh",
    "log", "ln", "lg", "exp", "lim", "liminf", "limsup", "max", "min", "sup", "inf", "det", "dim",
    "ker", "gcd", "deg", "arg", "Pr", "hom", "mod",
];

/// How typst sets a call's arguments, and so how `call()` writes them and
/// what [`same_run`] takes to be in the run around the call.
#[derive(Clone, Copy)]
enum CallKind {
    /// `display(…)`, `inline(…)`, `script(…)` or `sscript(…)`, which set
    /// their body (see [`styled_body`]) in the run around the call, in a
    /// size of their own. TeX can only change the size for a group, which
    /// would make the body an ordinary atom of its own, whatever its class,
    /// so the TeX leaves the body in the size around the call.
    Size,
    /// `lr(…)`, which sets its body, its last argument, in the run around
    /// the call, as a group of its own (see [`Setting::last`]).
    Lr,
    /// One that TeX has a command or environment for, whose arguments are
    /// runs of their own or an environment's cells.
    Command(Command),
    /// `abs(…)` and the others whose arguments typst sets between
    /// delimiters, in the run around the call.
    Fence(Delimiters),
    /// Any other call, like `sin(x)`: its name, and its arguments in
    /// parentheses, in the run around the call, as typst shows a call to
    /// what isn't a function.
    Other,
}

/// A call that TeX has a command or environment for (see
/// [`CallKind::Command`]), as `command()` writes it.
#[derive(Clone, Copy)]
enum Command {
    /// A command around its first argument: `\sqrt`, or an accent's or a
    /// line's, like `\hat` or `\overline`.
    Wrap(&'static str),
    /// `root(n, x)`, as `\sqrt[n]{x}`.
    Root,
    /// A command for a fraction-like pair of arguments: `\frac` or
    /// `\binom`.
    Fraction(&'static str),
    /// `op(…)`: `\operatorname` for a string, and else its content as one
    /// atom.
    Op,
    /// An over- or underbrace, with its annotation as a script (`^` or
    /// `_`) on it.
    Brace(&'static str, char),
    Vector,
    Matrix,
    Cases,
    /// `class(…)`: its body, with TeX's command for the class, where TeX
    /// has one.
    Class,
    /// `bold(…)` and the others that set their body in an alphabet, as
    /// TeX's command for it (`\mathbf`). Typst sets the body in the run
    /// around the call, but TeX can't split a command's argument at `&` or
    /// `\\`, so its line breaks and alignment points are the body's own.
    Alphabet(&'static str),
    /// `limits(…)` or `scripts(…)`, whose body is a run of its own: as it
    /// is, so the attachments that typst puts above and below it, or
    /// beside it, go where TeX puts them for the body.
    Limits,
}

/// What kind of call one to `name` is (see [`CallKind`]).
fn call_kind(name: &str) -> CallKind {
    use Command::*;
    let command = match name {
        "display" | "inline" | "script" | "sscript" => return CallKind::Size,
        "lr" => return CallKind::Lr,
        "abs" => return CallKind::Fence((Some('|'), Some('|'))),
        "norm" => return CallKind::Fence((Some('‖'), Some('‖'))),
        "floor" => return CallKind::Fence((Some('⌊'), Some('⌋'))),
        "ceil" => return CallKind::Fence((Some('⌈'), Some('⌉'))),
        "sqrt" => Wrap("sqrt"),
        "root" => Root,
        "frac" => Fraction("frac"),
        "binom" => Fraction("binom"),
        "op" => Op,
        "hat" => Wrap("hat"),
        "tilde" => Wrap("tilde"),
        "bar" | "macron" => Wrap("bar"),
        "dot" => Wrap("dot"),
        "dot.double" | "diaer" | "ddot" => Wrap("ddot"),
        "arrow" => Wrap("vec"),
        "overline" => Wrap("overline"),
        "underline" => Wrap("underline"),
        "overbrace" => Brace("overbrace", '^'),
        "underbrace" => Brace("underbrace", '_'),
        "vec" => Vector,
        "mat" => Matrix,
        "cases" => Cases,
        "class" => Class,
        "limits" | "scripts" => Limits,
        "bold" => Alphabet("mathbf"),
        "italic" => Alphabet("mathit"),
        "upright" => Alphabet("mathrm"),
        "cal" => Alphabet("mathcal"),
        "bb" => Alphabet("mathbb"),
        "frak" => Alphabet("mathfrak"),
        "sans" => Alphabet("mathsf"),
        "mono" => Alphabet("mathtt"),
        _ => return CallKind::Other,
    };
    CallKind::Command(command)
}

/// The name a call calls, like `sqrt` or `dot.double`.
fn callee_name(call: ast::MathCall) -> EcoString {
    call.callee().to_untyped().full_text()
}

/// A function call in math (`sqrt(x)`, `mat(1, 2; 3, 4)`), set as
/// `setting` has it, and written as its kind is (see [`CallKind`]).
fn call(call: ast::MathCall, setting: Setting) -> String {
    // Unlike an operand, an argument keeps its parentheses: typst shows
    // them in `sqrt((a+b))` and stretches them in `lr((x+1))`.
    let name = callee_name(call);
    // The arguments in the run around the call, and the TeX of one.
    let args = || same_run(Expr::MathCall(call), setting.last);
    let tex = |(arg, last): (Expr, bool)| trimmed(arg, Setting { last, ..setting });
    // The arguments between delimiters.
    let fence = |delimiters: Delimiters| {
        let args = args();
        let split = setting.grid.splits(&args);
        let body: Vec<String> = args.into_iter().map(tex).collect();
        fenced(delimiters, &body.join(", "), split)
    };
    match call_kind(&name) {
        // The body, passed through.
        CallKind::Size | CallKind::Lr => args().into_iter().map(tex).collect(),
        CallKind::Fence(delimiters) => fence(delimiters),
        CallKind::Other => format!("{}{}", symbol(&name), fence(PARENS)),
        CallKind::Command(command) => self::command(call, command, setting),
    }
}

/// A call that TeX has a command or environment for (see [`Command`]), set
/// as `setting` has it.
fn command(call: ast::MathCall, command: Command, setting: Setting) -> String {
    let Setting { size, align, .. } = setting;
    // Positional arguments, as rows split by `;`.
    let mut rows: Vec<Vec<Expr>> = vec![Vec::new()];
    let mut named: Vec<(&str, Expr)> = Vec::new();
    for arg in call.args().arg_items() {
        match arg.arg {
            ast::Arg::Pos(expr) => rows.last_mut().expect("a row").push(expr),
            ast::Arg::Named(n) => named.push((n.name().as_str(), n.expr())),
            ast::Arg::Spread(_) => {}
        }
        if arg.ends_in_semicolon {
            rows.push(Vec::new());
        }
    }
    if rows.last().is_some_and(Vec::is_empty) && rows.len() > 1 {
        rows.pop();
    }
    let args: Vec<Expr> = rows.concat();
    // An argument, which typst sets as a run of its own in size `size`.
    let own = |index: usize, size: MathSize| {
        args.get(index)
            .map(|&arg| run(arg, size, align))
            .unwrap_or_default()
    };
    // A row of `mat`, `vec` or `cases`, from its cells, which TeX sets in
    // text size.
    let row = |cells: &[Expr]| {
        let cell = Setting {
            size: MathSize::Text,
            align,
            grid: Grid::CELL,
            last: true,
        };
        let tex: Vec<String> = cells.iter().map(|&expr| trimmed(expr, cell)).collect();
        let is_align_point = |expr: Expr| matches!(expr, Expr::MathAlignPoint(_));
        Row {
            tex: tex.join(" & "),
            columns: cells
                .iter()
                .map(|&expr| 1 + count_in_whole_run(expr, &is_align_point))
                .sum(),
        }
    };
    // The rows of `vec` or `cases`, one per argument.
    let one_per_row = || -> Vec<Row> { args.iter().map(|&arg| row(&[arg])).collect() };
    let wrap = |command: &str| format!("\\{command}{{{}}}", own(0, size));
    let named_arg = |key: &str| {
        named
            .iter()
            .find_map(|&(name, expr)| (name == key).then_some(expr))
    };
    let delim = |default| named_arg("delim").and_then(delimiters).unwrap_or(default);
    match command {
        Command::Wrap(command) => wrap(command),
        Command::Root => format!(
            "\\sqrt[{}]{{{}}}",
            own(0, MathSize::ScriptScript),
            own(1, size)
        ),
        Command::Fraction(command) => {
            let size = fraction_size(size);
            format!("\\{command}{{{}}}{{{}}}", own(0, size), own(1, size))
        }
        // A string is the operator's name, set upright. Typst sets other
        // content as it is, as one atom: in braces, since pulldown-latex
        // shows the TeX of anything but a name in `\operatorname` or
        // `\mathop`.
        Command::Op => match args.first() {
            Some(Expr::Str(text)) => format!("\\operatorname{{{}}}", operator_name(&text.get())),
            _ => format!("{{{}}}", own(0, size)),
        },
        // An empty annotation, like `#none`, which typst draws nothing for,
        // is left out.
        Command::Brace(command, script) => match own(1, script_size(size)) {
            annotation if annotation.is_empty() => wrap(command),
            annotation => format!("\\{command}{{{}}}{script}{{{annotation}}}", own(0, size)),
        },
        Command::Vector => matrix(&one_per_row(), delim(PARENS)),
        Command::Matrix => {
            let rows: Vec<Row> = rows.iter().map(|cells| row(cells)).collect();
            matrix(&rows, delim(PARENS))
        }
        Command::Cases => {
            let reverse = named_arg("reverse").map(unparenthesized);
            let reverse = matches!(reverse, Some(Expr::Bool(b)) if b.get());
            cases(&one_per_row(), delim(BRACES), reverse)
        }
        // A style, around a body that is a run of its own, like any of a
        // command's arguments: as it is for `limits(…)` and `scripts(…)`,
        // and for a class that TeX has no command for.
        Command::Class | Command::Alphabet(_) | Command::Limits => {
            let body = styled_body(call)
                .map(|body| run(body, size, align))
                .unwrap_or_default();
            match style_command(call) {
                Some(command) => format!("\\{command}{{{body}}}"),
                None => body,
            }
        }
    }
}

/// The body of a call that only styles it, its last argument: one in a
/// size of its own (see [`CallKind::Size`]), `class(…)`, `limits(…)` or
/// `scripts(…)`, or one that sets it in an alphabet. `None` for any other
/// call.
fn styled_body(call: ast::MathCall) -> Option<Expr> {
    match call_kind(&callee_name(call)) {
        CallKind::Size
        | CallKind::Command(Command::Class | Command::Alphabet(_) | Command::Limits) => {
            positional(call).last()
        }
        CallKind::Lr | CallKind::Command(_) | CallKind::Fence(_) | CallKind::Other => None,
    }
}

/// The TeX command that sets the body of a call styling it (see
/// [`styled_body`]): its alphabet, or the class that `class(…)` gives it,
/// where TeX has one.
fn style_command(call: ast::MathCall) -> Option<&'static str> {
    match call_kind(&callee_name(call)) {
        CallKind::Command(Command::Alphabet(command)) => Some(command),
        CallKind::Command(Command::Class) => {
            let Some(Expr::Str(class)) = positional(call).next() else {
                return None;
            };
            Some(match class_named(&class.get()) {
                MathClass::Normal | MathClass::Alphabetic => "mathord",
                MathClass::Binary => "mathbin",
                MathClass::Relation => "mathrel",
                MathClass::Large => "mathop",
                MathClass::Opening => "mathopen",
                MathClass::Closing => "mathclose",
                MathClass::Punctuation => "mathpunct",
                _ => return None,
            })
        }
        _ => None,
    }
}

/// A call's positional arguments.
fn positional<'a>(call: ast::MathCall<'a>) -> impl Iterator<Item = Expr<'a>> {
    call.args().arg_items().filter_map(|arg| match arg.arg {
        ast::Arg::Pos(expr) => Some(expr),
        ast::Arg::Named(_) | ast::Arg::Spread(_) => None,
    })
}

/// `expr` without the parentheses around code like `#(true)`.
fn unparenthesized(expr: Expr) -> Expr {
    match expr {
        Expr::Parenthesized(inner) => unparenthesized(inner.expr()),
        expr => expr,
    }
}

/// The opening and closing delimiters of a group, or of a `mat`, `vec` or
/// `cases`; `None` is no delimiter.
type Delimiters = (Option<char>, Option<char>);

const PARENS: Delimiters = (Some('('), Some(')'));
const BRACES: Delimiters = (Some('{'), Some('}'));

/// A `delim` argument as typst casts it: an array of two delimiters, or one
/// whose partner is inferred. `None` when it's neither.
fn delimiters(expr: Expr) -> Option<Delimiters> {
    if let Expr::Array(array) = unparenthesized(expr) {
        let items: Vec<Expr> = array
            .items()
            .filter_map(|item| match item {
                ast::ArrayItem::Pos(expr) => Some(expr),
                ast::ArrayItem::Spread(_) => None,
            })
            .collect();
        return match items[..] {
            [open, close] => Some((delimiter_value(open)?, delimiter_value(close)?)),
            _ => None,
        };
    }
    let open = delimiter_value(expr)?;
    Some((open, open.map(matching_delimiter)))
}

/// One delimiter as typst casts it: `none`, a one-character string, or a
/// symbol with a single character, however math writes it (`bar`,
/// `bracket.l`, `#sym.bar`, `\[`, `||`). `None` when it's none of those.
fn delimiter_value(expr: Expr) -> Option<Option<char>> {
    let c = match unparenthesized(expr) {
        Expr::None(_) => return Some(None),
        Expr::Str(text) => single_char(&text.get())?,
        Expr::Escape(escape) => escape.get(),
        Expr::MathShorthand(shorthand) => shorthand.get(),
        expr @ (Expr::MathIdent(_) | Expr::MathFieldAccess(_) | Expr::FieldAccess(_)) => {
            symbol_char(&expr.to_untyped().full_text())?
        }
        _ => return None,
    };
    Some(Some(c))
}

/// The character a typst symbol name stands for, looked up in typst's own
/// symbol table: `bar` is `|`, its default variant, and `bar.double` is
/// `‖`. `None` for a name that isn't a symbol with a single character.
fn symbol_char(name: &str) -> Option<char> {
    let mut parts = symbol_name(name).split('.');
    let mut value = library().math.scope().get(parts.next()?)?.read();
    // A module of symbols, like `gender`, has them by name.
    while let Value::Module(module) = value {
        value = module.scope().get(parts.next()?)?.read();
    }
    let Value::Symbol(symbol) = value else {
        return None;
    };
    let symbol = parts.try_fold(symbol.clone(), |symbol, modifier| {
        symbol.modified((), modifier).ok()
    })?;
    single_char(symbol.get())
}

/// A typst symbol's name without the module that code names it from:
/// `alpha` for `sym.alpha` or `std.sym.alpha`.
fn symbol_name(name: &str) -> &str {
    name.strip_prefix("std.sym.")
        .or_else(|| name.strip_prefix("sym."))
        .unwrap_or(name)
}

/// The closing delimiter typst infers for an opening one and the reverse,
/// as `Delimiter::find_matching` does: neighbouring code points, except for
/// brackets and braces. A fence like `|` is its own partner.
fn matching_delimiter(c: char) -> char {
    match c {
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        _ => match default_math_class(c) {
            Some(MathClass::Opening) => char::from_u32(c as u32 + 1).unwrap_or(c),
            Some(MathClass::Closing) => char::from_u32(c as u32 - 1).unwrap_or(c),
            _ => c,
        },
    }
}

/// A row of `mat`, `vec` or `cases`: the TeX of its cells, between `&`s,
/// and the columns they take, one each and one more for each of their
/// alignment points.
struct Row {
    tex: String,
    columns: usize,
}

/// The rows of an environment, between `\\`s, which don't take a `[`
/// that starts a row as the start of an optional argument (see
/// [`bracket_guard`]).
fn stacked(rows: &[Row]) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push_str(" \\\\ ");
            out.push_str(bracket_guard(&row.tex));
        }
        out.push_str(&row.tex);
    }
    out
}

/// The most columns any of `rows` takes.
fn columns(rows: &[Row]) -> usize {
    rows.iter().map(|row| row.columns).max().unwrap_or(1)
}

/// The most columns amsmath's matrix environments take.
const MATRIX_COLUMNS: usize = 10;

/// A matrix or vector: amsmath's environment for its delimiters, or a
/// plain `matrix` between any others, or an `array` for more columns than
/// those take.
fn matrix(rows: &[Row], (open, close): Delimiters) -> String {
    let body = stacked(rows);
    let columns = columns(rows);
    if columns > MATRIX_COLUMNS {
        let spec = "c".repeat(columns);
        let array = format!("\\begin{{array}}{{{spec}}}{body}\\end{{array}}");
        return left_right(open, close, &array);
    }
    let environment = match (open, close) {
        (Some('('), Some(')')) => "pmatrix",
        (Some('['), Some(']')) => "bmatrix",
        (Some('{'), Some('}')) => "Bmatrix",
        (Some('|'), Some('|')) => "vmatrix",
        (Some('‖'), Some('‖')) => "Vmatrix",
        (None, None) => "matrix",
        _ => {
            let matrix = format!("\\begin{{matrix}}{body}\\end{{matrix}}");
            return left_right(open, close, &matrix);
        }
    };
    format!("\\begin{{{environment}}}{body}\\end{{{environment}}}")
}

/// Cases with the opening delimiter on the left or, reversed, the closing
/// one on the right. Any delimiter but a left `{`, and cases with more
/// columns than the two of `cases`, go around a left-aligned array like
/// the one `cases` is, since amsmath has no `rcases`.
fn cases(rows: &[Row], (open, close): Delimiters, reverse: bool) -> String {
    let (open, close) = if reverse { (None, close) } else { (open, None) };
    let body = stacked(rows);
    let columns = columns(rows).max(2);
    if (open, close) == (Some('{'), None) && columns == 2 {
        return format!("\\begin{{cases}}{body}\\end{{cases}}");
    }
    let spec = "l".repeat(columns);
    let array = format!("\\begin{{array}}{{{spec}}}{body}\\end{{array}}");
    left_right(open, close, &array)
}

/// A delimited group's TeX: `body` between `\left` and `\right`, unless
/// the run's rows or columns are split inside it (`split`), which they
/// can't straddle. The delimiters are then set at their normal size,
/// where typst grows them to the tallest row: TeX can't size a delimiter
/// to rows it isn't around.
fn fenced((open, close): Delimiters, body: &str, split: bool) -> String {
    if split {
        unstretched(open, close, body)
    } else {
        left_right(open, close, body)
    }
}

/// Whether either delimiter is one that `\left` or `\right` can stretch.
fn stretches(open: Option<char>, close: Option<char>) -> bool {
    open.into_iter().chain(close).any(|c| stretchy(c).is_some())
}

/// `body` between stretchy delimiters, either of which may be missing. One
/// that TeX can't stretch is set as it is, beside `\left` or `\right`, and
/// when neither can, both are.
fn left_right(open: Option<char>, close: Option<char>, body: &str) -> String {
    if !stretches(open, close) {
        return unstretched(open, close, body);
    }
    // What follows `\left` or `\right`, and the character set beside it.
    let side = |c: Option<char>| match c.map(|c| (c, stretchy(c))) {
        None => (".", String::new()),
        Some((_, Some(tex))) => (tex, String::new()),
        Some((c, None)) => (".", c.to_string()),
    };
    let (left, before) = side(open);
    let (right, after) = side(close);
    format!("{before}\\left{left} {body} \\right{right}{after}")
}

/// `body` between delimiters at their normal size, either of which may be
/// missing: under their TeX names, or as they are.
fn unstretched(open: Option<char>, close: Option<char>, body: &str) -> String {
    let side = |c: Option<char>| {
        c.map(|c| stretchy(c).map_or_else(|| character(c), str::to_string))
            .unwrap_or_default()
    };
    [side(open), body.to_string(), side(close)]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tex(source: &str) -> String {
        let root = typst::syntax::parse(&format!("${source}$"));
        let markup = root.cast::<ast::Markup>().unwrap();
        let Some(Expr::Equation(eq)) = markup.exprs().next() else {
            panic!("no equation in {source}");
        };
        equation(eq)
    }

    #[test]
    fn converts_common_notation() {
        assert_eq!(tex("x^2 + y_1"), "x^{2} + y_{1}");
        assert_eq!(tex("(a+b)/c"), "\\frac{a+b}{c}");
        assert_eq!(tex("alpha beta"), "\\alpha \\beta");
        assert_eq!(tex("sum_(i=1)^n i"), "\\sum_{i=1}^{n} i");
        assert_eq!(tex("sqrt(x) + root(3, y)"), "\\sqrt{x} + \\sqrt[3]{y}");
        assert_eq!(tex("x -> oo"), "x → \\infty");
        assert_eq!(tex("a <= b != c"), "a ≤ b ≠ c");
        assert_eq!(tex("RR"), "\\mathbb{R}");
        assert_eq!(tex("sin x"), "\\sin x");
        assert_eq!(tex("integral x dif x"), "\\int x \\mathrm{d} x");
        assert_eq!(tex("f'(x)"), "f'\\left( x \\right)");
    }

    /// Regression for #65: characters stay as typed, which every consumer
    /// of the TeX but LaTeX draws as typst does, and the LaTeX writer names.
    #[test]
    fn math_characters_stay_as_typed() {
        assert_eq!(tex("α + β → γ"), "α + β → γ");
        assert_eq!(tex("∀ x ∈ ℝ, µ ≇ Α"), "∀ x ∈ ℝ, µ ≇ Α");
        // However a character is typed: a shorthand, an escape or itself.
        assert_eq!(tex("-> \\u{2192} →"), "→ → →");
        assert_eq!(tex("a := b ... c * d"), "a ≔ b … c ∗ d");
        // Typst's names for symbols TeX has no name for are their
        // characters, with or without `sym.`.
        assert_eq!(
            tex("colon.eq kappa.alt in.small Alpha sym.lt.equiv"),
            "≔ ϰ ∊ Α ≦"
        );
        assert_eq!(tex("x^2 degree ast"), "x^{2} ° ∗");
        assert_eq!(tex("sym.alpha sym.arrow.r"), "\\alpha \\rightarrow");
        // The minus sign is TeX's hyphen.
        assert_eq!(tex("a - b − c"), "a - b - c");
    }

    /// Regression for #65: typst gives some symbols a variation selector,
    /// which picks the glyph of a character (`↪` as text, not an emoji),
    /// and which TeX has no use for: they are their characters. A typed
    /// character keeps its own, which the LaTeX writer drops where LaTeX
    /// has a name for the character.
    #[test]
    fn symbols_with_variation_selectors_are_their_characters() {
        assert_eq!(tex("arrow.r.hook arrow.tr union.serif"), "↪ ↗ ∪");
        assert_eq!(
            tex("a ↔\u{FE0E} b 葛\u{E0100} ❤\u{FE0F}"),
            "a ↔\u{FE0E} b 葛\u{E0100} ❤\u{FE0F}"
        );
        // Symbols in modules of their own, and named the way code does.
        assert_eq!(tex("gender.female gender.male"), "♀ ♂");
        assert_eq!(
            tex("#sym.alpha #(sym.arrow.r.hook) std.sym.arrow.tr #calc.pi"),
            "\\alpha ↪ ↗ calc.pi"
        );
    }

    /// Regression for #49: typst keeps a space between a string and what
    /// is beside it, unless their math classes space them (a relation, an
    /// operator, punctuation or a delimiter on the inside). TeX ignores
    /// spaces in math, so the space goes in a string's text.
    #[test]
    fn spaces_beside_strings_are_kept_as_typst_keeps_them() {
        assert_eq!(tex("x \"if\" y"), "x \\text{ if } y");
        assert_eq!(tex("\"if\"x"), "\\text{if}x");
        assert_eq!(
            tex("\"if\" = x, \"a\" + b"),
            "\\text{if} = x, \\text{a} + b"
        );
        assert_eq!(tex("a \"and\" (b)"), "a \\text{ and } \\left( b \\right)");
        assert_eq!(tex("x, \"if\" sum"), "x, \\text{if} \\sum");
        assert_eq!(tex("\"if\" \"so\""), "\\text{if } \\text{so}");
        // Typst sets a string of digits as a number, which takes no space,
        // and an empty one or one of several lines as no text.
        assert_eq!(tex("\"12\" x"), "\\text{12} x");
        assert_eq!(tex("\"12\" \"if\""), "\\text{12 } \\text{if}");
        assert_eq!(tex("a \"\" b \"x\\ny\" c"), "a \\text{} b \\text{x\ny} c");
        // Past an alignment point, which keeps it on the point's left (see
        // `kept_spaces_go_before_an_alignment_point`).
        assert_eq!(
            tex(" a & \"if\" b \\ \"so\" & c "),
            "\\begin{aligned}a\\hspace{0.333em} & \\text{if } b \\\\  \\text{so } & c\\end{aligned}"
        );
        // Typst's own spacing takes the place of the space beside it.
        assert_eq!(
            tex("a quad \"if\" thin y wide \"or\" #h(1em) z"),
            "a \\quad \\text{if} \\, y \\qquad \\text{or} \\hspace{1em} z"
        );
        assert_eq!(tex("\"if\" dif x"), "\\text{if} \\mathrm{d} x");
        // A styled string is text too, and a class's string has its class.
        assert_eq!(
            tex("bold(\"if\") x upright(\"or\") display(\"so\") y"),
            "\\mathbf{\\text{if }} x \\mathrm{\\text{ or }} \\text{so } y"
        );
        assert_eq!(
            tex("x class(\"binary\", \"and\") y class(\"normal\", \"or\") z"),
            "x \\mathbin{\\text{and}} y \\mathord{\\text{ or }} z"
        );
        // A `+` or `-` is a binary operator after an operand, and spaced
        // as text is where it isn't one.
        assert_eq!(
            tex("- \"if\" + x = - \"a\""),
            "- \\text{ if} + x = - \\text{ a}"
        );
        // `#none` sets nothing, which leaves the space.
        assert_eq!(tex("\"if\" #none x"), "\\text{if }  x");
    }

    /// Typst puts a space that it keeps right after the thing before it,
    /// so on the left of an alignment point between them, which lines up
    /// what comes after the point in each row (`"if"` and `"abc"` here).
    /// With no string on the left to hold it, it's a space of its own.
    #[test]
    fn kept_spaces_go_before_an_alignment_point() {
        assert_eq!(
            tex(" x x x & \"if\" y \\ & \"abc\" "),
            "\\begin{aligned}x x x\\hspace{0.333em} & \\text{if } y \\\\  & \\text{abc}\\end{aligned}"
        );
        assert_eq!(
            tex(" x x x &\"if\" y \\ & \"abc\" "),
            "\\begin{aligned}x x x\\hspace{0.333em} &\\text{if } y \\\\  & \\text{abc}\\end{aligned}"
        );
        assert_eq!(
            tex("cases(x & x >= 0, -x & \"otherwise\")"),
            "\\begin{cases}x & x ≥ 0 \\\\ -x\\hspace{0.333em} & \\text{otherwise}\\end{cases}"
        );
        // In the text of a string on the left.
        assert_eq!(
            tex(" \"w\" & \"if\" y \\ & \"abc\" "),
            "\\begin{aligned}\\text{w } & \\text{if } y \\\\  & \\text{abc}\\end{aligned}"
        );
        // A single row leaves the alignment point out, so the string after
        // it can hold the space.
        assert_eq!(tex("x & \"if\""), "x  \\text{ if}");
    }

    /// Typst keeps the spaces beside a fence like `|` that the source has,
    /// as it does beside text, which a string beside it holds if there is
    /// one.
    #[test]
    fn spaces_beside_fences_are_kept() {
        assert_eq!(tex("x |y| z"), "x\\hspace{0.333em} |y|\\hspace{0.333em} z");
        assert_eq!(tex("x|y|z"), "x|y|z");
        assert_eq!(tex(" | x | "), "|\\hspace{0.333em} x\\hspace{0.333em} |");
        assert_eq!(tex("\"if\" |y|"), "\\text{if } |y|");
        assert_eq!(tex("|y| \"if\""), "|y| \\text{ if}");
    }

    /// In script size, typst gives relations, binary operators and
    /// punctuation no space of their own, as TeX does, and keeps the
    /// spaces beside them that the source has next to text.
    #[test]
    fn script_size_keeps_the_spaces_beside_operators() {
        assert_eq!(tex("x_(a, \"if\")"), "x_{a, \\text{ if}}");
        assert_eq!(tex("x_(a,\"if\")"), "x_{a,\\text{if}}");
        assert_eq!(tex("x_(\"if\" = 1)"), "x_{\\text{if } = 1}");
        assert_eq!(tex("x^(\"a\" + b)"), "x^{\\text{a } + b}");
        assert_eq!(tex("(a, \"if\")"), "\\left( a, \\text{if} \\right)");
        assert_eq!(tex("\"if\" = 1"), "\\text{if} = 1");
        // Never beside a large operator, or before punctuation.
        assert_eq!(tex("x_(sum \"if\")"), "x_{\\sum \\text{if}}");
        assert_eq!(tex("x_(\"if\" ,)"), "x_{\\text{if} ,}");
        // Roots' indices and braces' annotations are in script size, and so
        // are the parts of a fraction in text, but not those of one in an
        // equation of its own.
        assert_eq!(tex("root(\"if\" = 1, x)"), "\\sqrt[\\text{if } = 1]{x}");
        assert_eq!(
            tex("underbrace(x, \"if\" = 1)"),
            "\\underbrace{x}_{\\text{if } = 1}"
        );
        assert_eq!(tex("(a, \"if\")/b"), "\\frac{a, \\text{ if}}{b}");
        assert_eq!(tex(" (a, \"if\")/b "), "\\frac{a, \\text{if}}{b}");
        // Typst sets the cells of `cases` a size smaller, in script size in
        // text, but TeX sets them in text size, which its spaces follow.
        assert_eq!(
            tex("cases(x = \"if\")"),
            "\\begin{cases}x = \\text{if}\\end{cases}"
        );
    }

    #[test]
    fn special_characters_stay_ordinary() {
        assert_eq!(tex("\\# \\_ \\{ \\$"), "\\# \\_ \\{ \\$");
        // TeX would read `^` as a superscript, `~` as a space and `\` as
        // the start of a command, where typst shows them.
        assert_eq!(
            tex("a \\^ b \\~ c \\\\ d"),
            "a \\hat{} b \\mathord{\\sim} c \\backslash d"
        );
        // In text, as text has them, and in an operator's name as math does.
        assert_eq!(tex("\"x\\\\y^\""), "\\text{x\\textbackslash{}y\\^{}}");
        assert_eq!(
            tex("op(\"x\\\\y^\")"),
            "\\operatorname{x\\backslash y\\hat{}}"
        );
    }

    #[test]
    fn greek_variants_follow_their_glyphs() {
        // Typst's `epsilon` is ε, which TeX calls `\varepsilon`; its
        // `epsilon.alt` is TeX's `\epsilon`, ϵ. The same goes for phi.
        assert_eq!(
            tex("epsilon epsilon.alt phi phi.alt"),
            "\\varepsilon \\epsilon \\varphi \\phi"
        );
        assert_eq!(tex("theta theta.alt"), "\\theta \\vartheta");
        // Typst's dots stay on the baseline, unlike amsmath's `\dots`
        // before an operator.
        assert_eq!(tex("x_1 + dots + x_n"), "x_{1} + \\ldots + x_{n}");
    }

    #[test]
    fn commands_do_not_run_into_the_letters_after_them() {
        // An escape ends an identifier; `\alphax` would be one command.
        assert_eq!(tex("alpha\\u{78}"), "\\alpha x");
        // XeTeX and LuaTeX read any Unicode letter as part of a command.
        assert_eq!(tex("oo\\u{4e2d}"), "\\infty 中");
        assert_eq!(tex("x\\u{3b1}"), "xα");
    }

    #[test]
    fn colon_equals_is_one_symbol() {
        // MathML draws `≔` as one symbol, where `\mathrel{:=}` would be a
        // `:` and an `=` spaced apart.
        assert_eq!(tex("a := b"), "a ≔ b");
        assert_eq!(tex("y =: g"), "y ≕ g");
        assert_eq!(tex("a ::= b"), "a ⩴ b");
        // A character is a base without braces, which keeps it a relation.
        assert_eq!(tex("a :=^\"def\" b"), "a ≔^{\\text{def}} b");
    }

    /// Regression for #48 and #62: an operator's content is one atom, its
    /// rows and scripts included, not cut short where it ends in a brace.
    #[test]
    fn operators_keep_their_content_whole() {
        assert_eq!(tex("op(\"sin\") x"), "\\operatorname{sin} x");
        assert_eq!(
            tex("op(a \\ b)"),
            "{\\begin{aligned}&{a } \\\\ &{ b}\\end{aligned}}"
        );
        assert_eq!(
            tex("op([| a \\ b |])"),
            "{\\begin{aligned}&{⟦ a } \\\\ &{ b ⟧}\\end{aligned}}"
        );
        assert_eq!(tex("op(hat(x)) + op(x_1)"), "{\\hat{x}} + {x_{1}}");
    }

    /// Typst shows the spaces in an operator's name, which TeX would
    /// ignore, and LaTeX takes the no-break spaces they become.
    #[test]
    fn operator_names_keep_their_spaces() {
        assert_eq!(tex("op(\"arg max\") x"), "\\operatorname{arg\u{A0}max} x");
    }

    #[test]
    fn call_arguments_keep_their_parentheses() {
        assert_eq!(tex("lr((x+1))"), "\\left( x+1 \\right)");
        assert_eq!(
            tex(" lr((integral_0^1 f)) "),
            "\\left( \\int_{0}^{1} f \\right)"
        );
        assert_eq!(tex("display((a+b))"), "\\left( a+b \\right)");
        assert_eq!(tex("sqrt((a+b))"), "\\sqrt{\\left( a+b \\right)}");
        assert_eq!(tex("frac((a), b)"), "\\frac{\\left( a \\right)}{b}");
        // Attachments and `/` drop the parentheses that group an operand,
        // and only those.
        assert_eq!(tex("x_((a))"), "x_{\\left( a \\right)}");
        assert_eq!(tex("((a))/b"), "\\frac{\\left( a \\right)}{b}");
    }

    #[test]
    fn braces_keep_their_annotation() {
        assert_eq!(
            tex("underbrace(x + y, \"sum\")"),
            "\\underbrace{x + y}_{\\text{sum}}"
        );
        assert_eq!(tex("overbrace(a, n)"), "\\overbrace{a}^{n}");
        assert_eq!(tex("underbrace(a)"), "\\underbrace{a}");
        // Typst draws no annotation for `none`.
        assert_eq!(tex("underbrace(x, #none)"), "\\underbrace{x}");
        // The body and the annotation are runs of their own.
        assert_eq!(tex("underbrace(x, a & b)"), "\\underbrace{x}_{a  b}");
        assert_eq!(
            tex("underbrace(x, a \\ b)"),
            "\\underbrace{x}_{\\substack{a \\\\  b}}"
        );
        assert_eq!(
            tex("underbrace(a &= b, \"x\")"),
            "\\underbrace{a = b}_{\\text{x}}"
        );
    }

    /// Each kind of call sets its arguments in the runs typst does (see
    /// `call_kind`), as an equation's rows show: a command's are runs of
    /// their own, which keep their alignment points to themselves, and a
    /// size's, `lr`'s and a fence's are in the run around the call.
    #[test]
    fn calls_set_their_arguments_in_the_runs_typst_does() {
        let rows = |call: &str| tex(&format!(" {call} \\ c &= d "));
        for (call, own) in [
            ("sqrt(a &= b)", "\\sqrt{a = b}"),
            ("hat(a &= b)", "\\hat{a = b}"),
            ("frac(a &= b, 2)", "\\frac{a = b}{2}"),
            ("op(a &= b)", "{a = b}"),
            ("bold(a &= b)", "\\mathbf{a = b}"),
            ("class(\"normal\", a &= b)", "\\mathord{a = b}"),
            ("limits(a &= b)", "a = b"),
            ("scripts(a &= b)", "a = b"),
        ] {
            let expected = format!("\\begin{{aligned}}{own} \\\\  c &= d\\end{{aligned}}");
            assert_eq!(rows(call), expected, "{call}");
        }
        for (call, shared) in [
            ("display(a &= b)", "a &= b"),
            ("script(a &= b)", "a &= b"),
            ("lr(\\{ a &= b)", "\\{ a &= b"),
            ("abs(a &= b)", "| a &= b |"),
            ("foo(a &= b)", "\\operatorname{foo}( a &= b )"),
        ] {
            let expected = format!("\\begin{{aligned}}{shared} \\\\  c &= d\\end{{aligned}}");
            assert_eq!(rows(call), expected, "{call}");
        }
    }

    #[test]
    fn converts_calls_and_structures() {
        assert_eq!(
            tex("mat(1, 2; 3, 4)"),
            "\\begin{pmatrix}1 & 2 \\\\ 3 & 4\\end{pmatrix}"
        );
        assert_eq!(tex("vec(a, b)"), "\\begin{pmatrix}a \\\\ b\\end{pmatrix}");
        assert_eq!(tex("abs(x)"), "\\left| x \\right|");
        assert_eq!(tex("bold(v) dot hat(x)"), "\\mathbf{v} \\cdot \\hat{x}");
        assert_eq!(tex("\"if\" x"), "\\text{if } x");
        assert_eq!(tex("{a, b}"), "\\left\\{ a, b \\right\\}");
        assert_eq!(tex("foo(x)"), "\\operatorname{foo}\\left( x \\right)");
    }

    #[test]
    fn display_math_with_alignment_gets_an_environment() {
        assert_eq!(
            tex(" a &= b \\ c &= d "),
            "\\begin{aligned}a &= b \\\\  c &= d\\end{aligned}"
        );
    }

    #[test]
    fn a_single_row_drops_its_alignment_points() {
        // Typst ignores alignment points with no other row to align to, and
        // TeX rejects an `&` outside an environment.
        assert_eq!(tex("a &= b"), "a = b");
        assert_eq!(tex(" a &= b "), "a = b");
        assert_eq!(tex("display(a &= b)"), "a = b");
        assert_eq!(tex("lr(\\{ a &= b)"), "\\{ a = b");
        // Those of `cases` are its own.
        assert_eq!(
            tex("cases(1 & x > 0, 0 & \"else\")"),
            "\\begin{cases}1 & x > 0 \\\\ 0\\hspace{0.333em} & \\text{else}\\end{cases}"
        );
    }

    #[test]
    fn a_single_row_leaves_out_nested_alignment_points_too() {
        // Between delimiters, which then stretch.
        assert_eq!(tex("(a &= b)"), "\\left( a = b \\right)");
        assert_eq!(tex("lr((a &= b))"), "\\left( a = b \\right)");
        assert_eq!(tex("abs(a &= b)"), "\\left| a = b \\right|");
        assert_eq!(tex("sin(a &= b)"), "\\sin\\left( a = b \\right)");
        // In operands and arguments, which typst lays out on their own.
        assert_eq!(tex("sqrt(a &= b)"), "\\sqrt{a = b}");
        assert_eq!(tex("x_(a &= b)"), "x_{a = b}");
        assert_eq!(tex("(a &= b)/c"), "\\frac{a = b}{c}");
        assert_eq!(tex("frac(a &= b, c)"), "\\frac{a = b}{c}");
        assert_eq!(tex("bold(a &= b)"), "\\mathbf{a = b}");
        // An operand is a single row of its own inside the equation's rows.
        assert_eq!(
            tex(" x &= sqrt(b & c) \\ d &= e "),
            "\\begin{aligned}x &= \\sqrt{b  c} \\\\  d &= e\\end{aligned}"
        );
    }

    #[test]
    fn inline_math_with_rows_gets_an_environment() {
        // Typst stacks an inline equation's rows and aligns them.
        assert_eq!(
            tex("x &= y \\ z &= w"),
            "\\begin{aligned}x &= y \\\\  z &= w\\end{aligned}"
        );
    }

    /// Typst centres rows without alignment points in an equation of its
    /// own and starts them at the left in one in text, where `aligned`
    /// would put them in its right-aligned column. There it sets their last
    /// on the baseline, as `[b]` does.
    #[test]
    fn rows_without_alignment_points_are_centred_or_start_at_the_left() {
        assert_eq!(
            tex(" a \\ b b b b "),
            "\\begin{gathered}a \\\\  b b b b\\end{gathered}"
        );
        assert_eq!(
            tex("a \\ b b b b"),
            "\\begin{aligned}[b]&{a } \\\\ &{ b b b b}\\end{aligned}"
        );
        // In braces, an operator that starts a row stays as typst sets it.
        assert_eq!(
            tex("a \\ -b \\ = c"),
            "\\begin{aligned}[b]&{a } \\\\ &{ -b } \\\\ &{ = c}\\end{aligned}"
        );
        assert_eq!(
            tex("a \\ \\ b"),
            "\\begin{aligned}[b]&{a } \\\\ &{ } \\\\ &{ b}\\end{aligned}"
        );
        // The rows of operands and arguments too, but those stay centred
        // on the math axis.
        assert_eq!(
            tex(" sqrt(a \\ b) "),
            "\\sqrt{\\begin{gathered}a \\\\  b\\end{gathered}}"
        );
        assert_eq!(
            tex("sqrt(a \\ b)"),
            "\\sqrt{\\begin{aligned}&{a } \\\\ &{ b}\\end{aligned}}"
        );
        // Those in script size, the parts of a fraction in text among
        // them, get `\substack`.
        assert_eq!(tex("x_(a \\ b)"), "x_{\\substack{a \\\\  b}}");
        assert_eq!(tex("(a \\ b)/c"), "\\frac{\\substack{a \\\\  b}}{c}");
    }

    /// Typst drops a line break that ends a run, which starts no row, but
    /// not one before a closing delimiter, which ends a row of its own.
    #[test]
    fn a_line_break_that_ends_a_run_makes_no_row() {
        assert_eq!(tex("a \\ "), "a");
        assert_eq!(tex(" a \\ "), "a");
        assert_eq!(tex("x_(a \\ ) + sqrt(b \\ )"), "x_{a} + \\sqrt{b}");
        // Past spaces and `#none`, which typst sets nothing for, but not
        // past an alignment point.
        assert_eq!(tex(" a \\ #none "), "a");
        assert_eq!(tex(" a \\ & "), "\\begin{aligned}a \\\\  &\\end{aligned}");
        // At the end of what `display` passes through, which ends the run,
        // and of an `lr` group, which ends one of its own.
        assert_eq!(tex(" b + display(a \\ ) "), "b + a");
        assert_eq!(tex(" lr(\\{ a \\ ) + b "), "\\{ a + b");
        assert_eq!(
            tex(" (a \\ ) "),
            "\\begin{gathered}( a \\\\  )\\end{gathered}"
        );
        assert_eq!(
            tex(" abs(a \\ ) "),
            "\\begin{gathered}| a \\\\ |\\end{gathered}"
        );
    }

    #[test]
    fn environments_alone_need_no_aligned_wrapper() {
        // The `&` and `\\` of `mat`, `vec` and `cases` are their own.
        assert_eq!(
            tex(" mat(1, 2; 3, 4) "),
            "\\begin{pmatrix}1 & 2 \\\\ 3 & 4\\end{pmatrix}"
        );
        assert_eq!(tex(" vec(a, b) "), "\\begin{pmatrix}a \\\\ b\\end{pmatrix}");
        assert_eq!(
            tex(" cases(1 & x > 0, 0 & \"else\") "),
            "\\begin{cases}1 & x > 0 \\\\ 0\\hspace{0.333em} & \\text{else}\\end{cases}"
        );
        // An escaped `&` isn't an alignment point.
        assert_eq!(tex(" A \\& B "), "A \\& B");
        // The equation's own line break still gets one, around the matrix.
        assert_eq!(
            tex(" A = vec(1, 2) \\ B = 0 "),
            "\\begin{gathered}A = \\begin{pmatrix}1 \\\\ 2\\end{pmatrix} \\\\  B = 0\\end{gathered}"
        );
    }

    #[test]
    fn rows_that_reach_the_equation_get_an_environment() {
        // An unclosed delimiter leaves the rest of the equation in a plain
        // sequence, whose line break is the equation's own.
        assert_eq!(
            tex(" x in [0, 1[ \\ y = 2 "),
            "\\begin{gathered}x \\in [0, 1[ \\\\  y = 2\\end{gathered}"
        );
        // `display` and `lr` pass their argument through as it is.
        assert_eq!(
            tex(" display(a &= b \\ c &= d) "),
            "\\begin{aligned}a &= b \\\\  c &= d\\end{aligned}"
        );
        assert_eq!(
            tex(" lr(\\{ a &= b \\ c &= d) "),
            "\\begin{aligned}\\{ a &= b \\\\  c &= d\\end{aligned}"
        );
    }

    #[test]
    fn rows_of_an_operand_get_an_environment_of_their_own() {
        // Typst stacks them inside the operand, centred in a script.
        assert_eq!(tex(" x_(a \\ b) "), "x_{\\substack{a \\\\  b}}");
        assert_eq!(
            tex(" x_(a \\ b) + [| c \\ d |] "),
            "\\begin{gathered}x_{\\substack{a \\\\  b}} + ⟦ c \\\\  d ⟧\\end{gathered}"
        );
        assert_eq!(
            tex(" sqrt(a &= b \\ c &= d) "),
            "\\sqrt{\\begin{aligned}a &= b \\\\  c &= d\\end{aligned}}"
        );
        assert_eq!(
            tex(" (a \\ b)/c "),
            "\\frac{\\begin{gathered}a \\\\  b\\end{gathered}}{c}"
        );
    }

    #[test]
    fn rows_between_delimiters_are_the_equations() {
        // Typst lays a delimited group out in the run around it, so the
        // rows and columns inside are the equation's. `\left` and `\right`
        // can't straddle them, so the delimiters are set as they are.
        assert_eq!(
            tex(" [| a \\ b |] "),
            "\\begin{gathered}⟦ a \\\\  b ⟧\\end{gathered}"
        );
        assert_eq!(tex("[| a &= b |]"), "⟦ a = b ⟧");
        assert_eq!(
            tex(" ⌈ a \\ b ⌉ "),
            "\\begin{gathered}\\lceil a \\\\  b \\rceil\\end{gathered}"
        );
        assert_eq!(
            tex(" lr((a &= b \\ c &= d)) "),
            "\\begin{aligned}( a &= b \\\\  c &= d )\\end{aligned}"
        );
        assert_eq!(
            tex(" (a \\ b) + [| c \\ d |] "),
            "\\begin{gathered}( a \\\\  b ) + ⟦ c \\\\  d ⟧\\end{gathered}"
        );
        assert_eq!(
            tex(" abs(a \\ b) "),
            "\\begin{gathered}| a \\\\  b |\\end{gathered}"
        );
        assert_eq!(
            tex(" (a &= b) \\ c &= d "),
            "\\begin{aligned}( a &= b ) \\\\  c &= d\\end{aligned}"
        );
        // A group without the run's rows or columns still stretches.
        assert_eq!(
            tex(" (a/b) \\ c "),
            "\\begin{gathered}\\left( \\frac{a}{b} \\right) \\\\  c\\end{gathered}"
        );
    }

    /// Regression for #62: `aligned`, `gathered` and `\\` would read a `[`
    /// after them, even past spaces, as the start of an optional argument.
    #[test]
    fn rows_that_start_with_a_bracket_are_kept_from_arguments() {
        assert_eq!(
            tex(" [0, 1[ \\ x "),
            "\\begin{gathered}{}[0, 1[ \\\\  x\\end{gathered}"
        );
        assert_eq!(
            tex("[0, 1[ &= x \\ y &= z"),
            "\\begin{aligned}{}[0, 1[ &= x \\\\  y &= z\\end{aligned}"
        );
        assert_eq!(
            tex(" a \\ [b \\ c] "),
            "\\begin{gathered}a \\\\  {}[ b \\\\  c ]\\end{gathered}"
        );
        assert_eq!(
            tex(" display(a \\ ) [0, 1[ "),
            "\\begin{gathered}a \\\\ {}[0, 1[\\end{gathered}"
        );
        // Rows that start at the left start after an `&`.
        assert_eq!(
            tex("[0, 1[ \\ [a]"),
            "\\begin{aligned}[b]&{[0, 1[ } \\\\ &{ \\left[ a \\right]}\\end{aligned}"
        );
        // A bracket that stretches is `\left[`.
        assert_eq!(
            tex(" a \\ [b] "),
            "\\begin{gathered}a \\\\  \\left[ b \\right]\\end{gathered}"
        );
        // The rows of environments too.
        assert_eq!(
            tex("cases(c, [a & b])"),
            "\\begin{cases}c \\\\ {}[ a & b ]\\end{cases}"
        );
        assert_eq!(
            tex("mat(a; [b & c])"),
            "\\begin{pmatrix}a \\\\ {}[ b & c ]\\end{pmatrix}"
        );
    }

    /// Regression for #62: a cell's alignment points are columns of the
    /// environment, which must have room for them: `cases` has two, and
    /// amsmath's matrices ten.
    #[test]
    fn environments_have_a_column_for_every_alignment_point() {
        assert_eq!(
            tex("cases(a & b & c, d)"),
            "\\left\\{ \\begin{array}{lll}a & b & c \\\\ d\\end{array} \\right."
        );
        assert_eq!(
            tex("cases(x &= (a & b))"),
            "\\left\\{ \\begin{array}{lll}x &= ( a & b )\\end{array} \\right."
        );
        let row = "a & b & c & d & e & f & g & h & i & j";
        assert_eq!(
            tex(&format!("mat({row})")),
            format!("\\begin{{pmatrix}}{row}\\end{{pmatrix}}")
        );
        assert_eq!(
            tex("mat(a & b & c & d & e & f, g & h & i & j & k & l)"),
            format!(
                "\\left( \\begin{{array}}{{{}}}{row} & k & l\\end{{array}} \\right)",
                "c".repeat(12)
            )
        );
    }

    #[test]
    fn cells_take_their_alignment_points_as_columns() {
        // Typst aligns them down the column; `&` gives the environment more
        // columns, so a delimited group around one can't stretch.
        assert_eq!(
            tex(" cases((x & y), z) "),
            "\\begin{cases}( x & y ) \\\\ z\\end{cases}"
        );
        assert_eq!(
            tex(" mat((a & b), c) "),
            "\\begin{pmatrix}( a & b ) & c\\end{pmatrix}"
        );
        // Typst ignores line breaks in a cell.
        assert_eq!(
            tex(" vec(a \\ b, c) "),
            "\\begin{pmatrix}a  b \\\\ c\\end{pmatrix}"
        );
    }

    #[test]
    fn a_lone_alignment_point_or_line_break_is_an_empty_base() {
        // Typst lays an attachment's base out on its own, which drops a lone
        // alignment point or line break: the scripts sit on nothing.
        assert_eq!(tex(" a &_x = b "), "a {}_{x} = b");
        assert_eq!(tex(" a &' = b "), "a {}' = b");
        assert_eq!(tex(" a \\ _x = b "), "a {}_{x} = b");
        // The equation's own rows still get an environment, and the row
        // whose alignment point was a base has none.
        assert_eq!(
            tex(" a &_x = b \\ c &= d "),
            "\\begin{aligned}a {}_{x} = b \\\\  c &= d\\end{aligned}"
        );
    }

    #[test]
    fn environments_inside_other_calls_need_no_aligned_wrapper() {
        let pmatrix = "\\begin{pmatrix}1 & 2 \\\\ 3 & 4\\end{pmatrix}";
        assert_eq!(tex(" display(mat(1, 2; 3, 4)) "), pmatrix);
        assert_eq!(
            tex(" (mat(1, 2; 3, 4)) "),
            format!("\\left( {pmatrix} \\right)")
        );
        assert_eq!(tex(" mat(1, 2; 3, 4)^T "), format!("{{{pmatrix}}}^{{T}}"));
        // A string's `&` is text.
        assert_eq!(tex(" \"a & b\" "), "\\text{a \\& b}");
    }

    #[test]
    fn delimiters_are_cast_as_typst_casts_them() {
        let bmatrix = "\\begin{bmatrix}a\\end{bmatrix}";
        let vmatrix = "\\begin{vmatrix}a\\end{vmatrix}";
        let double = "\\begin{Vmatrix}a\\end{Vmatrix}";
        // Symbols, by their default variant or with modifiers.
        assert_eq!(tex("mat(delim: bar, a)"), vmatrix);
        assert_eq!(tex("mat(delim: bracket, a)"), bmatrix);
        assert_eq!(
            tex("vec(delim: brace, a)"),
            "\\begin{Bmatrix}a\\end{Bmatrix}"
        );
        assert_eq!(tex("mat(delim: bar.double, a)"), double);
        assert_eq!(tex("mat(delim: #sym.bar, a)"), vmatrix);
        assert_eq!(
            tex("mat(delim: paren.r, a)"),
            "\\left) \\begin{matrix}a\\end{matrix} \\right("
        );
        assert_eq!(
            tex("mat(delim: chevron, a)"),
            "\\left\\langle \\begin{matrix}a\\end{matrix} \\right\\rangle"
        );
        // Escapes, shorthands and parenthesized code.
        assert_eq!(tex("mat(delim: \\[, a)"), bmatrix);
        assert_eq!(tex("mat(delim: \\|, a)"), vmatrix);
        assert_eq!(tex("mat(delim: ||, a)"), double);
        assert_eq!(tex("mat(delim: #(\"[\"), a)"), bmatrix);
        assert_eq!(
            tex("cases(reverse: #(true), a, b)"),
            "\\left. \\begin{array}{ll}a \\\\ b\\end{array} \\right\\}"
        );
    }

    #[test]
    fn delimiters_tex_cannot_stretch_are_set_as_they_are() {
        // `\left⟦` is an error in TeX without extra packages.
        let stroked = "⟦ \\begin{matrix}a\\end{matrix} ⟧";
        assert_eq!(tex("mat(delim: \"⟦\", a)"), stroked);
        assert_eq!(tex("mat(delim: bracket.stroked.l, a)"), stroked);
        assert_eq!(
            tex("cases(delim: \"⟪\", a, b)"),
            "⟪ \\begin{array}{ll}a \\\\ b\\end{array}"
        );
        assert_eq!(
            tex("vec(delim: #(\"[\", \"⟧\"), a)"),
            "\\left[ \\begin{matrix}a\\end{matrix} \\right.⟧"
        );
        assert_eq!(tex("[| a |]"), "⟦ a ⟧");
        assert_eq!(tex("⟪ a ⟫"), "⟪ a ⟫");
        // Ones it can, under their TeX names.
        assert_eq!(
            tex("mat(delim: \"⟮\", a)"),
            "\\left\\lgroup \\begin{matrix}a\\end{matrix} \\right\\rgroup"
        );
        assert_eq!(tex("⌜ a ⌝"), "\\left\\ulcorner a \\right\\urcorner");
    }

    #[test]
    fn matrices_and_vectors_take_their_delimiters() {
        assert_eq!(
            tex("mat(delim: \"[\", 1, 0; 0, 1)"),
            "\\begin{bmatrix}1 & 0 \\\\ 0 & 1\\end{bmatrix}"
        );
        assert_eq!(
            tex("mat(delim: #none, 1, 0; 0, 1)"),
            "\\begin{matrix}1 & 0 \\\\ 0 & 1\\end{matrix}"
        );
        assert_eq!(
            tex("vec(delim: \"{\", a, b)"),
            "\\begin{Bmatrix}a \\\\ b\\end{Bmatrix}"
        );
        assert_eq!(
            tex("mat(delim: \"|\", a)"),
            "\\begin{vmatrix}a\\end{vmatrix}"
        );
        assert_eq!(
            tex("mat(delim: bar.v.double, a)"),
            "\\begin{Vmatrix}a\\end{Vmatrix}"
        );
        // Delimiters no environment has, and a pair given as an array.
        assert_eq!(
            tex("vec(delim: \"⟨\", a, b)"),
            "\\left\\langle \\begin{matrix}a \\\\ b\\end{matrix} \\right\\rangle"
        );
        assert_eq!(
            tex("vec(delim: #(\"[\", none), a, b)"),
            "\\left[ \\begin{matrix}a \\\\ b\\end{matrix} \\right."
        );
    }

    #[test]
    fn cases_take_their_delimiter_and_direction() {
        assert_eq!(
            tex("cases(reverse: #true, a, b)"),
            "\\left. \\begin{array}{ll}a \\\\ b\\end{array} \\right\\}"
        );
        assert_eq!(
            tex("cases(delim: \"[\", a, b)"),
            "\\left[ \\begin{array}{ll}a \\\\ b\\end{array} \\right."
        );
        assert_eq!(
            tex("cases(delim: \"[\", reverse: #true, a, b)"),
            "\\left. \\begin{array}{ll}a \\\\ b\\end{array} \\right]"
        );
        assert_eq!(
            tex("cases(reverse: #false, a, b)"),
            "\\begin{cases}a \\\\ b\\end{cases}"
        );
    }
}
