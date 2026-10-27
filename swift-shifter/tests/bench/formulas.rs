//! How many formulas a LaTeX document has, and how many its Typst has and
//! how many of those parse: the scorer's formulas kept and parsing.
//!
//! LaTeX is read as the app's LaTeX reader reads it: its verbatim text cut
//! out first, by the reader's own code, then parsed with mitex-parser.
//!
//! Known limits, as Matt decided on 2026-10-27 (15:36): the count follows
//! LaTeX's structure, not all of TeX's syntax. A `\def` whose parameters
//! are delimited (`\def\x#1]{…}`), a definition whose body isn't a group
//! (`\newcommand\x a`), and a `document` environment inside a definition
//! may be read wrong, as may others of their kind. The scores compare one
//! run with another, and a document is read the same wrong way each time,
//! so these don't move a score between runs.

use std::path::Path;

use mitex_parser::syntax::{SyntaxElement, SyntaxKind, SyntaxNode};

#[allow(dead_code)]
#[path = "../../src/converter/document/native/read/latex/verbatim.rs"]
mod verbatim;

/// The environments that are a formula each, starred or not.
const MATH: &[&str] = &[
    "equation",
    "align",
    "gather",
    "multline",
    "eqnarray",
    "flalign",
    "alignat",
    "displaymath",
    "math",
];

/// The environments whose text isn't LaTeX, besides `verbatim`, which the
/// reader's code cuts out: cut out too before parsing (see [`unparsed`]),
/// as an unclosed brace in one would take the parser past its end.
const UNPARSED: &[&str] = &[
    "Verbatim",
    "Verbatim*",
    "BVerbatim",
    "LVerbatim",
    "lstlisting",
    "minted",
    "comment",
];

/// The commands that define a macro, an environment or an operator, and
/// how many bodies each takes after its name: what it would expand to, not
/// the document's (see [`past_definition`]).
const DEFINITIONS: &[(&str, usize)] = &[
    ("\\newcommand", 1),
    ("\\renewcommand", 1),
    ("\\providecommand", 1),
    ("\\DeclareRobustCommand", 1),
    ("\\DeclareMathOperator", 1),
    ("\\newenvironment", 2),
    ("\\renewenvironment", 2),
    ("\\def", 1),
    ("\\gdef", 1),
    ("\\edef", 1),
    ("\\xdef", 1),
];

/// How many formulas the LaTeX document `main` has in its `document`
/// environment (all of it, for a fragment with none): each `$…$`,
/// `$$…$$`, `\(…\)`, `\[…\]` and math environment (see [`MATH`]) outside
/// another, and those of the files it `\input`s or `\include`s. Comments,
/// verbatim text and definitions don't count, nor what macros would expand
/// to. A file that can't be read is an error.
pub fn in_latex(main: &Path) -> Result<usize, String> {
    let root = parse(&read(main)?);
    let body = root
        .descendants()
        .find(|node| node.kind() == SyntaxKind::ItemEnv && environment_name(node) == "document");
    let mut count = Count {
        dir: main.parent().unwrap_or(Path::new(".")),
        depth: 0,
        formulas: 0,
    };
    count.items(body.as_ref().unwrap_or(&root))?;
    Ok(count.formulas)
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse(source: &str) -> SyntaxNode {
    let cut = verbatim::cut(source);
    mitex_parser::parse_without_macro(
        &unparsed(&cut.source),
        mitex_parser::spec::CommandSpec::new(Default::default()),
    )
}

/// `source` without its environments that aren't LaTeX (see [`UNPARSED`]):
/// each from its `\begin` to the first `\end` of it, as LaTeX reads them.
/// One in a comment is left, and one left unclosed.
fn unparsed(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(at) = rest.find(['\\', '%']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with('%') {
            let line = rest.find(['\n', '\r']).unwrap_or(rest.len());
            out.push_str(&rest[..line]);
            rest = &rest[line..];
            continue;
        }
        let begun = rest
            .strip_prefix("\\begin")
            .map(str::trim_start)
            .and_then(|after| after.strip_prefix('{'))
            .and_then(|after| {
                let close = after.find('}')?;
                UNPARSED
                    .contains(&after[..close].trim())
                    .then(|| (after[..close].trim(), &after[close + 1..]))
            });
        if let Some((name, after)) = begun
            && let Some(end) = after.find(&format!("\\end{{{name}}}"))
        {
            rest = &after[end + format!("\\end{{{name}}}").len()..];
            continue;
        }
        // A command's backslash and the character after, so an escaped
        // one (`\%`, `\\`) is passed whole.
        let len = rest[1..].chars().next().map_or(1, |c| 1 + c.len_utf8());
        out.push_str(&rest[..len]);
        rest = &rest[len..];
    }
    out.push_str(rest);
    out
}

/// Counting a document's formulas (see [`in_latex`]).
struct Count<'a> {
    /// Where included files are read from.
    dir: &'a Path,
    /// How many includes in.
    depth: usize,
    formulas: usize,
}

impl Count<'_> {
    /// Counts the formulas in `node`'s children.
    fn items(&mut self, node: &SyntaxNode) -> Result<(), String> {
        let items = flattened(node);
        let mut at = 0;
        while let Some(item) = items.get(at) {
            at += 1;
            let SyntaxElement::Node(item) = item else {
                continue;
            };
            match item.kind() {
                // What's in it, `\text{$…$}` too, is of it.
                SyntaxKind::ItemFormula => self.formulas += 1,
                SyntaxKind::ItemEnv => {
                    let name = environment_name(item);
                    let bare = name.trim_end_matches('*');
                    if MATH.contains(&bare) {
                        self.formulas += 1;
                    } else {
                        self.items(item)?;
                    }
                }
                SyntaxKind::ItemCmd => {
                    let name = command_name(item);
                    if let Some(&(_, bodies)) = DEFINITIONS.iter().find(|(d, _)| *d == name) {
                        at = past_definition(&items, at, bodies);
                    } else if matches!(name.as_str(), "\\input" | "\\include") {
                        if let Some(file) = included(&items[at..])
                            && self.depth < 16
                        {
                            self.include(&file)?;
                        }
                    } else {
                        self.items(item)?;
                    }
                }
                _ => self.items(item)?,
            }
        }
        Ok(())
    }

    /// Counts the formulas of `file`, an included one, all of it.
    fn include(&mut self, file: &str) -> Result<(), String> {
        let path = self.dir.join(file);
        let path = if path.extension().is_some() {
            path
        } else {
            path.with_extension("tex")
        };
        let root = parse(&read(&path)?);
        self.depth += 1;
        let counted = self.items(&root);
        self.depth -= 1;
        counted
    }
}

/// `node`'s children, those that only group their text (text, a command's
/// arguments) taken apart into what they hold, as the reader takes them.
fn flattened(node: &SyntaxNode) -> Vec<SyntaxElement> {
    let mut out = Vec::new();
    for child in node.children_with_tokens() {
        match child {
            SyntaxElement::Node(child)
                if matches!(
                    child.kind(),
                    SyntaxKind::ItemText | SyntaxKind::ItemAttachComponent
                ) =>
            {
                out.extend(flattened(&child));
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether `item` is space, a line break or a comment: what TeX passes
/// over looking for an argument.
fn passed_over(item: &SyntaxElement) -> bool {
    matches!(
        item.kind(),
        SyntaxKind::TokenWhiteSpace | SyntaxKind::TokenLineBreak | SyntaxKind::TokenComment
    )
}

/// Where `items` go on past a definition whose command is right before
/// `at`: past its star, its name (`{\x}`, `\x`), what's between that and
/// its bodies (`[2][{x}]`, `#1#2`), and its `bodies` groups.
fn past_definition(items: &[SyntaxElement], mut at: usize, bodies: usize) -> usize {
    let skip_over = |at: &mut usize| {
        while items.get(*at).is_some_and(passed_over) {
            *at += 1;
        }
    };
    skip_over(&mut at);
    if items
        .get(at)
        .is_some_and(|item| item.kind() == SyntaxKind::TokenAsterisk)
    {
        at += 1;
        skip_over(&mut at);
    }
    // Its name.
    at += 1;
    let mut left = bodies;
    let mut brackets = 0;
    while left > 0
        && let Some(item) = items.get(at)
    {
        at += 1;
        match item.kind() {
            SyntaxKind::TokenLBracket => brackets += 1,
            SyntaxKind::TokenRBracket => brackets -= 1,
            SyntaxKind::ItemCurly if brackets == 0 => left -= 1,
            _ => {}
        }
    }
    at
}

/// The file an `\input` or `\include` names, from `items` right after it:
/// a group, or, for `\input part`, the word after its space.
fn included(items: &[SyntaxElement]) -> Option<String> {
    let mut items = items.iter().skip_while(|item| passed_over(item)).peekable();
    if let Some(SyntaxElement::Node(group)) = items.peek()
        && group.kind() == SyntaxKind::ItemCurly
    {
        let text = group.text().to_string();
        return Some(text.trim_matches(['{', '}']).trim().to_string());
    }
    let name: String = items
        .map_while(|item| match item {
            SyntaxElement::Token(token) if !passed_over(item) => Some(token.text().to_string()),
            _ => None,
        })
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The name of `env`, an environment: what its `\begin` names.
fn environment_name(env: &SyntaxNode) -> String {
    env.first_child()
        .and_then(|begin| begin.first_token())
        .map(|name| name.text().to_string())
        .unwrap_or_default()
}

fn command_name(command: &SyntaxNode) -> String {
    command
        .first_token()
        .map(|name| name.text().to_string())
        .unwrap_or_default()
}

/// How many equations the Typst `source` has outside another, and how many
/// of them hold a syntax error.
pub fn in_typst(source: &str) -> (usize, usize) {
    fn walk(node: &typst::syntax::SyntaxNode, counts: &mut (usize, usize)) {
        // What's in it, `#text[$…$]` too, is of it, as in LaTeX (see
        // [`in_latex`]).
        if node.kind() == typst::syntax::SyntaxKind::Equation {
            counts.0 += 1;
            counts.1 += usize::from(!node.errors_and_warnings().0.is_empty());
            return;
        }
        for child in node.children() {
            walk(child, counts);
        }
    }
    let mut counts = (0, 0);
    walk(&typst::syntax::parse(source), &mut counts);
    counts
}

#[test]
fn latex_formulas_are_counted_once_each() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.tex");
    std::fs::write(
        &main,
        "\\newcommand{\\R}{$\\mathbb{R}$}\n\
         \\begin{document}\n\
         $a$ and $$b$$, \\(c\\) and \\[d\\], costs \\$5 and \\$6. % $not$\n\
         \\begin{align*} x &= \\text{$y$} \\\\ z \\end{align*}\n\
         \\begin{equation}\\begin{aligned} e \\end{aligned}\\end{equation}\n\
         \\begin{verbatim} $not$ \\end{verbatim}\n\
         \\begin{lstlisting} $not$ \\end{lstlisting}\n\
         \\begin{itemize}\\item $f$\\end{itemize}\n\
         \\input{part}\n\
         \\include{sub/more.tex}\n\
         \\end{document}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("part.tex"), "$g$ and $h$").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/more.tex"), "\\[i\\]").unwrap();
    // a b c d, the align and the equation, f; g h; i.
    assert_eq!(in_latex(&main), Ok(10));
}

#[test]
fn latex_formulas_are_where_latex_reads_them() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.tex");
    std::fs::write(dir.path().join("part.tex"), "$x$ $y$").unwrap();
    let cases = [
        // Inline verbatim, its `%` a character.
        (
            "\\begin{document}\\verb|$no$| \\verb*+%+ $a$\\end{document}",
            1,
        ),
        // Spaces and comments before an argument.
        (
            "\\begin {document}\\input {part} \\begin\n{equation}e\\end{equation}\\end{document}",
            3,
        ),
        ("\\begin{document}\\input % here\n{part}\\end{document}", 2),
        // The body is the real one, and ends at its end.
        (
            "% \\begin{document}\n\\def\\x{$p$}\\begin{document}$a$\\end{document}$draft$",
            1,
        ),
        // A fragment with no body is all body.
        ("$a$ and \\[b\\]", 2),
        // Nothing in a comment ends a formula or an environment.
        ("$a % $ not\n+b$", 1),
        ("\\(a % \\)\nb\\) \\[c % \\]\nd\\] $$e % $$\nf$$", 3),
        (
            "\\begin{equation}a % \\end{equation}\n b\\end{equation}$c$",
            2,
        ),
        // Nor do spaces keep an environment from ending.
        ("\\begin{equation}a\\end {equation}$b$", 2),
        // An escaped `%` is no comment; one after an escaped `\` is.
        ("$a\\%$ $b$ \\\\% $c$", 2),
        // A definition's bodies are what it would expand to, not the
        // document's, wherever it is.
        (
            "\\newcommand{\\startmath}{\\begin{equation}}\\begin{document}$a$\\end{document}",
            1,
        ),
        (
            "\\begin{document}\\newcommand{\\unused}{$x$}\\newcommand*\\two[2][1]{$#1#2$}\
             \\def\\three#1{\\[#1\\]}\\DeclareMathOperator*{\\op}{op}\
             \\newenvironment{e}{$}{$}\\renewcommand{\\b}{{\\{$\\}}}$y$\\end{document}",
            1,
        ),
        // An optional argument's group is no body.
        ("\\newcommand{\\foo}[1][{x}]{$y$}$z$", 1),
        // A formula in a formula is of it.
        ("$x + \\text{$y$}$", 1),
        // Command names that aren't ASCII.
        ("\\def\\☃{$x$}$y$", 1),
        // `\input` without braces; not a longer name.
        (
            "\\begin{document}\\input part \\inputencoding{x}$a$\\end{document}",
            3,
        ),
    ];
    for (source, formulas) in cases {
        std::fs::write(&main, source).unwrap();
        assert_eq!(in_latex(&main), Ok(formulas), "{source}");
    }
    // A file that isn't there is an error, not no formulas.
    std::fs::write(&main, "\\begin{document}\\input{gone}\\end{document}").unwrap();
    assert!(in_latex(&main).unwrap_err().contains("gone.tex"));
    assert!(in_latex(&dir.path().join("none.tex")).is_err());
}

#[test]
fn typst_equations_are_counted_with_those_that_dont_parse() {
    assert_eq!(in_typst("a $x$ and $ y = 1 $ #text[$z$]"), (3, 0));
    // A script with nothing to it doesn't parse; an unclosed bracket does,
    // as Typst draws it.
    assert_eq!(in_typst("$x^$ and $(y$"), (2, 1));
    assert_eq!(in_typst("$a \"b$"), (1, 1));
    assert_eq!(in_typst("no math"), (0, 0));
    // One in another is of it, counted with it once, and so is its error.
    assert_eq!(in_typst("$x + #text[$y$]$"), (1, 0));
    assert_eq!(in_typst("$x + #text[$y^$]$"), (1, 1));
}

/// A formula with one in it is one, in LaTeX and in Typst alike, so the
/// share kept compares like with like.
#[test]
fn latex_and_typst_count_in_one_unit() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.tex");
    std::fs::write(&main, "$x + \\text{$y$}$ and $z$").unwrap();
    assert_eq!(in_latex(&main), Ok(2));
    assert_eq!(in_typst("$x + #text[$y$]$ and $z$").0, 2);
}

#[test]
fn environments_that_arent_latex_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.tex");
    let cases = [
        // An unclosed brace in one doesn't take the rest with it.
        (
            "\\begin{lstlisting}\nif (x) {\n\\end{lstlisting}\n$a$ and $b$",
            2,
        ),
        ("\\begin{minted}{c}\nint f() { $no$\n\\end{minted}$a$", 1),
        ("\\begin {Verbatim}\n}{ $no$\n\\end{Verbatim}$a$", 1),
        // One in a comment isn't one.
        ("% \\begin{lstlisting}\n$a$ \\end{lstlisting} $b$", 2),
    ];
    for (source, formulas) in cases {
        std::fs::write(&main, source).unwrap();
        assert_eq!(in_latex(&main), Ok(formulas), "{source}");
    }
}
