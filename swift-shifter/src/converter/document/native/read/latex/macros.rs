//! Macros in math. A formula stays TeX, for writers to read, and they know
//! none of the document's commands, so what those stand for is written into
//! its source.

use super::{DEFINITIONS, MAX_EXPANSION_DEPTH, Macro, Reader};

/// What [`substituted`] puts at each end of an argument, in a document of
/// `source`: where the macro's source and the argument's meet, which TeX
/// read apart. The spaces after a control word go only where they're of its
/// own source (see [`Reader::expand_tex`]), so not past this; it's taken out
/// when the text is done (see [`without_boundaries`]). A private use
/// character `source` doesn't hold, so neither does any macro's text or
/// argument, which are of it.
pub(super) fn boundary_for(source: &str) -> char {
    let private = |c: &char| matches!(c, '\u{e000}'..='\u{f8ff}' | '\u{f0000}'..='\u{ffffd}');
    let held: std::collections::HashSet<char> = source.chars().filter(private).collect();
    ('\u{e000}'..='\u{f8ff}')
        .chain('\u{f0000}'..='\u{ffffd}')
        .find(|c| !held.contains(c))
        // A source holding all 71,000 of them shares one.
        .unwrap_or('\u{e000}')
}

/// `text` without the `boundary`s put in it (see [`boundary_for`]).
pub(super) fn without_boundaries(text: &str, boundary: char) -> String {
    text.replace(boundary, "")
}

/// The spaces TeX doesn't see after a control word: not other Unicode
/// spaces, which are text.
const TEX_SPACES: [char; 4] = [' ', '\t', '\n', '\r'];

impl Reader {
    /// `tex`, a formula, with the macros it calls expanded, as far as
    /// [`Reader::expand`] would; a call that goes past that stays as it is,
    /// and is counted for the same warning. A formula that defines commands
    /// stays as it is: which of its calls mean what is its own affair.
    pub(super) fn expanded_math(&mut self, tex: &str) -> String {
        if defines(tex) {
            return tex.to_string();
        }
        let mut expanded = Tex::new(self.boundary);
        expanded.text.reserve(tex.len());
        self.expand_tex(tex, None, 0, &mut expanded);
        without_boundaries(&expanded.text, self.boundary)
    }

    /// Appends `tex` to `out` with its macros expanded, `depth` macros deep,
    /// and says how much of `after`, what's unread after `tex`, its last
    /// call took as arguments.
    fn expand_tex(
        &mut self,
        tex: &str,
        after: Option<&Unread>,
        depth: usize,
        out: &mut Tex,
    ) -> usize {
        let mut rest = tex;
        let mut taken_after = 0;
        while let Some(at) = rest.find('\\') {
            out.push(&rest[..at]);
            let (name, past_name) = rest[at..].split_at(command_length(&rest[at..]));
            rest = past_name;
            let body = self.definitions.get(name).cloned().flatten();
            let body = body.filter(|body| !defines(&body.source));
            // TeX doesn't see the spaces after a control word, which would
            // be a call's text's in `\text`: those of its own source, not
            // past a boundary, which isn't a space.
            let control_word = name[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '@');
            let past_spaces = if control_word {
                rest.trim_start_matches(TEX_SPACES)
            } else {
                rest
            };
            let unread = Unread {
                text: past_spaces,
                then: after,
                skip: taken_after,
            };
            let call = body.and_then(|body| Some((self.arguments_at(&body, &unread)?, body)));
            let Some(((arguments, taken), body)) = call else {
                out.push(name);
                continue;
            };
            let text = (depth < MAX_EXPANSION_DEPTH)
                .then(|| substituted(&body.source, &arguments, self.expansion_left, self.boundary))
                .flatten();
            let Some(text) = text else {
                self.unexpanded += 1;
                out.push(name);
                continue;
            };
            self.expansion_left = self.expansion_left.saturating_sub(text.len() + 1);
            rest = past_spaces;
            take(&mut rest, &mut taken_after, taken);
            let unread = Unread {
                text: rest,
                then: after,
                skip: taken_after,
            };
            let taken = self.expand_tex(&text, Some(&unread), depth + 1, out);
            take(&mut rest, &mut taken_after, taken);
        }
        out.push(rest);
        taken_after
    }

    /// What a call of `body` takes from `unread`: from its first text, which
    /// is enough for most, or else from as much of the rest as it needs, as
    /// a macro's text can end in a call whose arguments are after that
    /// macro's. What it takes is counted in what the document may expand to,
    /// and a call going past that is counted for the warning. It copies no
    /// more than twice what it needs, or a small window.
    fn arguments_at(&mut self, body: &Macro, unread: &Unread) -> Option<(Vec<String>, usize)> {
        let last = unread.then.is_none();
        let end = if last { End::Input } else { End::Macro };
        let found = arguments(body, unread.text, end, self.boundary);
        if found.is_some() || last {
            return found;
        }
        let mut window = 64_usize;
        loop {
            let start = unread.start(window, self.boundary);
            let end = if start.whole { End::Input } else { End::Window };
            match arguments(body, &start.text, end, self.boundary) {
                Some((_, taken)) if taken >= self.expansion_left => {
                    self.unexpanded += 1;
                    return None;
                }
                Some((arguments, taken)) => {
                    self.expansion_left -= taken;
                    let added = start.spaces.iter().filter(|at| **at < taken).count();
                    return Some((arguments, taken - added));
                }
                None if start.whole => {
                    self.expansion_left = self.expansion_left.saturating_sub(start.text.len());
                    return None;
                }
                None => window = window.saturating_mul(2),
            }
        }
    }
}

/// What's unread after a macro's call, up the calls it's in: `text`, then
/// `then` past its first `skip` bytes, which calls in `text` took.
struct Unread<'a> {
    text: &'a str,
    then: Option<&'a Unread<'a>>,
    skip: usize,
}

impl Unread<'_> {
    /// Its first `length` bytes or so, whole characters, `boundary`s kept.
    fn start(&self, length: usize, boundary: char) -> Start {
        let mut text = Tex::new(boundary);
        let mut spaces = Vec::new();
        let mut unread = Some(self);
        let mut skip = 0;
        while let Some(part) = unread {
            let rest = &part.text[skip.min(part.text.len())..];
            skip = skip.saturating_sub(part.text.len()) + part.skip;
            let room = length.saturating_sub(text.text.len());
            let end = (room.min(rest.len())..=rest.len())
                .find(|end| rest.is_char_boundary(*end))
                .unwrap_or(rest.len());
            let before = text.text.len();
            text.push(&rest[..end]);
            if text.text.len() > before + end {
                spaces.push(before);
            }
            if end < rest.len() {
                return Start {
                    text: text.text,
                    spaces,
                    whole: false,
                };
            }
            unread = part.then;
        }
        Start {
            text: text.text,
            spaces,
            whole: true,
        }
    }
}

/// The start of what's unread (see [`Unread::start`]).
struct Start {
    text: String,
    /// Where a space keeps tokens of two parts apart, which isn't unread
    /// text.
    spaces: Vec<usize>,
    /// Whether it's all of what's unread.
    whole: bool,
}

/// How a text being read for arguments ends.
#[derive(PartialEq)]
enum End {
    /// There's no more.
    Input,
    /// At a token's end, a macro's text, with more after it.
    Macro,
    /// Anywhere, with more after it.
    Window,
}

/// Takes `taken` bytes of what's unread: from `rest`, then past it, into
/// `taken_after`.
fn take(rest: &mut &str, taken_after: &mut usize, taken: usize) {
    let here = taken.min(rest.len());
    *rest = &rest[here..];
    *taken_after += taken - here;
}

/// TeX being put together, keeping tokens apart where it's joined.
struct Tex {
    text: String,
    /// Whether `text` ends in a control word, which letters would run on
    /// from.
    in_control_word: bool,
    /// What marks an argument's ends in it (see [`boundary_for`]).
    boundary: char,
}

impl Tex {
    fn new(boundary: char) -> Tex {
        Tex {
            text: String::new(),
            in_control_word: false,
            boundary,
        }
    }

    /// Appends `tex`, whole tokens, with a space before it if it would run
    /// on from a control word. Spaces there mean nothing in math, and an
    /// argument of `\text` that holds one is its own. A boundary keeps
    /// nothing apart, and is looked past.
    fn push(&mut self, tex: &str) {
        if tex.is_empty() {
            return;
        }
        let starts = tex.trim_start_matches(self.boundary);
        if self.in_control_word && starts.starts_with(|c: char| c.is_ascii_alphabetic()) {
            self.text.push(' ');
        }
        self.text.push_str(tex);
        let tex = tex.trim_end_matches(self.boundary);
        if tex.is_empty() {
            return;
        }
        let before_letters = tex.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let backslashes = before_letters.len() - before_letters.trim_end_matches('\\').len();
        self.in_control_word = before_letters.len() < tex.len() && backslashes % 2 == 1;
    }
}

/// Whether `tex` calls a command that defines others.
pub(super) fn defines(tex: &str) -> bool {
    let mut rest = tex;
    while let Some(at) = rest.find('\\') {
        let (name, after) = rest[at..].split_at(command_length(&rest[at..]));
        if name == "\\let" || DEFINITIONS.contains(&name) {
            return true;
        }
        rest = after;
    }
    false
}

/// The bytes of the command that `tex` starts with, whose first is a `\`: a
/// control word, with `@`s as the parser reads them, or a single character.
fn command_length(tex: &str) -> usize {
    let rest = &tex[1..];
    let word = rest
        .find(|c: char| !c.is_ascii_alphabetic() && c != '@')
        .unwrap_or(rest.len());
    let symbol = rest.chars().next().map_or(0, char::len_utf8);
    1 + if word > 0 { word } else { symbol }
}

/// What a call of `body` takes from `after`, what's after its name: each
/// argument, and the bytes they took. `None` if one's missing, which leaves
/// the call as written, or if what's past `end` could change them.
fn arguments(body: &Macro, after: &str, end: End, boundary: char) -> Option<(Vec<String>, usize)> {
    let mut arguments = Vec::with_capacity(body.parameters);
    let mut at = 0;
    if let Some(default) = &body.default {
        let next = at + spaces(&after[at..], boundary);
        let given = after[next..].strip_prefix('[').and_then(|inside| {
            let end = closing(inside, ']')?;
            Some((&inside[..end], next + end + 2))
        });
        let open = after[next..].is_empty() || after[next..].starts_with('[');
        if end != End::Input && given.is_none() && open {
            return None;
        }
        match given {
            Some((argument, end)) => {
                arguments.push(argument.to_string());
                at = end;
            }
            None => arguments.push(default.clone()),
        }
    }
    while arguments.len() < body.parameters {
        at += spaces(&after[at..], boundary);
        let rest = &after[at..];
        let (argument, length) = if let Some(inside) = rest.strip_prefix('{') {
            let end = closing(inside, '}')?;
            (&inside[..end], end + 2)
        } else if rest.starts_with('\\') {
            let length = command_length(rest);
            // More letters may be of it.
            if length == rest.len() && end == End::Window {
                return None;
            }
            (&rest[..length], length)
        } else {
            let length = rest.chars().next()?.len_utf8();
            (&rest[..length], length)
        };
        arguments.push(argument.to_string());
        at += length;
    }
    Some((arguments, at))
}

/// The bytes of spaces `tex` starts with, and `boundary`s among them.
fn spaces(tex: &str, boundary: char) -> usize {
    tex.len()
        - tex
            .trim_start_matches(|c: char| c.is_whitespace() || c == boundary)
            .len()
}

/// Where `close` is in `tex`, outside any group: `}` ends the group `tex` is
/// in, and `]` doesn't get past it.
fn closing(tex: &str, close: char) -> Option<usize> {
    let mut depth = 0_usize;
    let mut chars = tex.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            _ if c == close && depth == 0 => return Some(at),
            '{' => depth += 1,
            '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

/// `source`, a macro's, with `arguments` for its `#1`, `#2` and so on, each
/// between `boundary`s (see [`boundary_for`]), and `#` for its `##`. `None`
/// if that's `limit` bytes or more, before it's all put together.
pub(super) fn substituted(
    source: &str,
    arguments: &[String],
    limit: usize,
    boundary: char,
) -> Option<String> {
    let mut text = Tex::new(boundary);
    let push = |text: &mut Tex, tex: &str| {
        let fits = text.text.len() + tex.len() < limit;
        if fits {
            text.push(tex);
        }
        fits
    };
    let mut run = 0;
    let mut chars = source.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
                continue;
            }
            '#' => {}
            _ => continue,
        }
        let next = chars.peek().map(|(_, next)| *next);
        let argument = next
            .and_then(|digit| digit.to_digit(10))
            .and_then(|digit| (digit as usize).checked_sub(1))
            .and_then(|index| arguments.get(index));
        // A `#` of neither kind stays in the run.
        if next != Some('#') && argument.is_none() {
            continue;
        }
        if !push(&mut text, &source[run..at]) {
            return None;
        }
        chars.next();
        let put = match argument {
            Some(argument) => format!("{boundary}{argument}{boundary}"),
            None => "#".to_string(),
        };
        if !push(&mut text, &put) {
            return None;
        }
        run = at + 2;
    }
    push(&mut text, &source[run..]).then_some(text.text)
}

#[cfg(test)]
mod tests {
    use super::super::read;
    use crate::converter::document::native::ast::{Block, Inline};
    use crate::converter::document::native::write::typst::{Options, Target, write};

    /// The first formula of the paragraph or block `source` reads as.
    fn formula(source: &str) -> String {
        let (doc, _) = read(source);
        let found = doc.blocks.iter().find_map(|block| match block {
            Block::Math(tex) => Some(tex.clone()),
            Block::Paragraph(content) => content.iter().find_map(|inline| match inline {
                Inline::Math(tex) => Some(tex.clone()),
                _ => None,
            }),
            _ => None,
        });
        found.unwrap_or_else(|| panic!("no formula in {source}: {:?}", doc.blocks))
    }

    /// Each row: a source, and its formula.
    #[test]
    fn macros_are_expanded_in_formulas() {
        for (source, expected) in [
            ("\\newcommand{\\x}{a}$\\x$", "a"),
            ("\\def\\x{a}$\\x+1$", "a+1"),
            ("\\def\\x{a}\\[\\x\\]", "a"),
            (
                "\\def\\x{a}\\begin{equation}\\x\\end{equation}",
                "\\begin{equation}a\\end{equation}",
            ),
            // Definitions call others, as far as they're defined.
            ("\\def\\x{a}\\def\\y{\\x\\x}$\\y$", "aa"),
            ("\\def\\y{\\x}\\def\\x{a}$\\y$", "a"),
            // The name is the whole of it.
            ("\\def\\x{a}$\\xy$", "\\xy"),
            // TeX doesn't see the spaces after a control word.
            ("\\def\\x{a}$\\x y$", "ay"),
            ("\\def\\x{\\alpha}$\\x{}y$", "\\alpha{}y"),
            // `\\\\` is a line break, then an `x`, and `\\#` is a character.
            ("\\def\\x{a}$\\\\x$", "\\\\x"),
            ("\\def\\x{a}$\\\\\\x$", "\\\\a"),
            ("\\def\\x{\\#}$\\x$", "\\#"),
            ("\\newcommand{\\x}[1]{\\##1}$\\x{a}$", "\\#a"),
            ("\\newcommand{\\x}{##}$\\x$", "#"),
            // Arguments.
            ("\\newcommand{\\f}[1]{(#1)}$\\f{a}$", "(a)"),
            ("\\newcommand{\\f}[2]{#2#1}$\\f{a}{b}$", "ba"),
            ("\\newcommand{\\f}[2]{#2#1}$\\f ab$", "ba"),
            ("\\newcommand{\\f}[1]{(#1)}$\\f\\alpha b$", "(\\alpha) b"),
            ("\\newcommand{\\f}[1]{(#1)}$\\f{{a}\\}}$", "({a}\\})"),
            ("\\newcommand{\\f}[1]{#1#1}$\\f{a}$", "aa"),
            ("\\def\\f#1#2{#2#1}$\\f ab$", "ba"),
            // In `\\text`, where the spaces are the text's.
            (
                "\\newcommand{\\f}[1]{\\text{#1 c}}$\\f{a b}$",
                "\\text{a b c}",
            ),
            ("\\def\\x{a}$\\text{\\x b}$", "\\text{ab}"),
            // But for those after an argument, which are the macro's.
            (
                "\\def\\x{a}\\newcommand{\\f}[1]{#1 b}$\\text{\\f{\\x}}$",
                "\\text{a b}",
            ),
            (
                "\\def\\x{a}\\newcommand{\\f}[1]{#1 b}$\\text{\\f{\\x c}}$",
                "\\text{ac b}",
            ),
            // Nor those an argument starts with, nor in one passed on.
            (
                "\\def\\x{a}\\def\\f#1{\\x#1}$\\text{\\f{ b}}$",
                "\\text{a b}",
            ),
            // Other Unicode spaces are text.
            ("\\def\\x{a}$\\text{\\x\u{a0}b}$", "\\text{a\u{a0}b}"),
            // A private use character of the document's is as it is.
            (
                "\\def\\x{a}\\newcommand{\\f}[1]{#1\u{e000}}$\\text{\u{e000}\\f{\\x\u{e000}}}$",
                "\\text{\u{e000}a\u{e000}\u{e000}}",
            ),
            (
                "\\def\\x{a}\\def\\g#1{#1}\\def\\f#1{\\g{#1 b}}$\\text{\\f{\\x}}$",
                "\\text{a b}",
            ),
            ("\\def\\x{a}$\\text{\\x{} b}$", "\\text{a{} b}"),
            ("\\def\\x{a}$\\text{\\x\\ b}$", "\\text{a\\ b}"),
            ("\\def\\@x{a}$\\text{\\@x b}$", "\\text{ab}"),
            // But after a control symbol.
            ("\\def\\+{a}$\\text{\\+ b}$", "\\text{a b}"),
            // A control word doesn't run on into what's after it.
            ("\\newcommand{\\f}[1]{\\alpha#1}$\\f{b}$", "\\alpha b"),
            ("\\newcommand{\\f}[1]{\\alpha}$\\f{a}b$", "\\alpha b"),
            ("\\newcommand{\\f}[1]{\\\\#1}$\\f{b}$", "\\\\b"),
            // An optional first argument.
            ("\\newcommand{\\f}[2][d]{#1#2}$\\f{a}$", "da"),
            ("\\newcommand{\\f}[2][d]{#1#2}$\\f[c]{a}$", "ca"),
            ("\\newcommand{\\f}[2][d]{#1#2}$\\f[{]}]{a}$", "{]}a"),
            // A macro's last call takes what's after that macro.
            (
                "\\renewcommand{\\sin}[1]{(#1)}\\newcommand{\\wrapped}{\\sin}$\\wrapped{x}$",
                "(x)",
            ),
            ("\\newcommand{\\f}[2]{#1#2}\\def\\g{\\f{a}}$\\g{b}$", "ab"),
            (
                "\\newcommand{\\f}[1]{(#1)}\\def\\g{\\f}\\def\\h{\\g}$\\h{a}b$",
                "(a)b",
            ),
            (
                "\\newcommand{\\f}[2]{#1#2}\\def\\g{\\f a}\\def\\h{\\g b}$\\h c$",
                "abc",
            ),
            ("\\newcommand{\\f}[1]{(#1)}\\def\\g{\\f}$\\g$", "\\f"),
            ("\\newcommand{\\a}[1]{\\a}$\\a{x}{y}{z}$", "\\a"),
            (
                "\\newcommand{\\f}[1]{(#1)}\\newcommand{\\g}[1]{\\f\\alpha}$\\g{}b$",
                "(\\alpha)b",
            ),
            (
                "\\newcommand{\\f}[1]{(#1)}\\newcommand{\\g}[1]{\\f#1}$\\g{\\alpha}b$",
                "(\\alpha)b",
            ),
            (
                "\\newcommand{\\f}[2]{(#1#2)}\\newcommand{\\g}[1]{\\f\\alpha}$\\g{}bc$",
                "(\\alpha b)c",
            ),
            ("\\newcommand{\\f}[1][d]{#1}\\def\\g{\\f}$\\g[c]$", "c"),
            ("\\newcommand{\\f}[1][d]{#1}\\def\\g{\\f}$\\g$", "d"),
            ("\\newcommand{\\f}[1][d]{#1}\\def\\g{\\f}$\\g x$", "dx"),
            // `\\providecommand` leaves what formulas know.
            ("\\providecommand{\\label}[1]{}$a$", "a"),
            ("\\providecommand{\\sin}{q}$\\sin x$", "\\sin x"),
            ("\\renewcommand{\\sin}{q}$\\sin x$", "qx"),
            ("\\providecommand{\\foo}{q}$\\foo x$", "qx"),
            // But for what mitex takes from KaTeX, which LaTeX doesn't have.
            (
                "\\providecommand{\\R}{\\mathbb{R}}$x\\in\\R$",
                "x\\in\\mathbb{R}",
            ),
            ("\\providecommand{\\tg}{T}$\\tg$", "T"),
            ("\\providecommand{\\alpha}{T}$\\alpha$", "\\alpha"),
            // Short of arguments, it's as written.
            ("\\newcommand{\\f}[2]{#1#2}$\\f{a}$", "\\f{a}"),
            // Nor does what's after an argument.
            ("\\newcommand{\\f}[1]{#1x}$\\f{\\alpha}$", "\\alpha x"),
            // What can't be expanded stays.
            ("\\def\\f#1 {[#1]}$\\f xy z$", "\\f xy z"),
            // A formula that defines commands stays, as does a macro that
            // does.
            ("\\def\\x{a}$\\def\\x{b}\\x$", "\\def\\x{b}\\x"),
            ("\\def\\x{a}$\\let\\y\\x$", "\\let\\y\\x"),
            ("\\def\\m{\\def\\y{b}}$\\m$", "\\m"),
            ("\\def\\x{a}$\\definecolor\\x$", "\\definecolor a"),
            ("\\def\\f#1.{a}$\\f b.$", "\\f b."),
            ("\\newcommand{\\be}{\\begin{aligned}}$\\be$", "\\be"),
            ("\\renewcommand{\\emph}{a}$\\emph{b}$", "\\emph{b}"),
            ("$\\y$\\def\\y{a}", "\\y"),
            // `\\DeclareMathOperator`.
            (
                "\\DeclareMathOperator{\\op}{ab}$\\op x$",
                "\\operatorname{ab}x",
            ),
            (
                "\\DeclareMathOperator*{\\op}{ab}$\\op x$",
                "\\operatorname*{ab}x",
            ),
            (
                "\\DeclareMathOperator{\\op}{ab}\\begin{document}$\\op$\\end{document}",
                "\\operatorname{ab}",
            ),
        ] {
            assert_eq!(formula(source), expected, "{source}");
        }
    }

    /// What's cut out as verbatim stays what it was, in a macro or an
    /// argument.
    #[test]
    fn verbatim_text_survives_expansion() {
        for source in [
            "\\def\\x{\\verb|a#1|}$\\x$",
            "\\newcommand{\\f}[1]{#1#1}$\\f{\\verb|a#1|}$",
        ] {
            let tex = formula(source);
            assert!(tex.contains("\\verb!a#1!"), "{source}: {tex}");
        }
    }

    /// Finding a call's arguments past its macro takes only as much of the
    /// long text after it as they need, of what the document may expand to.
    #[test]
    fn arguments_past_a_macro_cost_what_they_take() {
        let source = format!(
            "\\newcommand{{\\f}}[1]{{(#1)}}\\def\\g{{\\f}}\\def\\x{{y}}${}{}\\x$",
            "\\g{a}".repeat(1_000),
            "b ".repeat(100_000)
        );
        assert!(formula(&source).ends_with('y'));
        // Looking at what's after a call, not taking it, costs nothing.
        let source = format!(
            "\\newcommand{{\\f}}[1][d]{{#1}}\\def\\g{{\\f}}${}{}$",
            "\\g a".repeat(950),
            "b".repeat(64)
        );
        assert!(!formula(&source).contains("\\f"));
    }

    /// An argument found past a macro is whole, wherever it falls in what's
    /// looked at for it.
    #[test]
    fn arguments_past_a_macro_are_whole() {
        for spaces in 55..70 {
            let source = format!(
                "\\newcommand{{\\f}}[1]{{(#1)}}\\def\\g{{\\f}}$\\g{}\\alpha$",
                " ".repeat(spaces)
            );
            assert_eq!(formula(&source), "(\\alpha)", "{spaces}");
        }
    }

    /// One calling itself in a formula ends, with the warning for that.
    #[test]
    fn a_formula_that_never_ends_is_cut_short() {
        // Too long to fit what's left, however long it's allowed to be.
        let huge = format!(
            "\\newcommand{{\\a}}[1]{{{}}}$\\a{{{}}}$ done",
            "#1".repeat(1_000),
            "x".repeat(10_000)
        );
        for source in [
            "\\def\\a{\\a}$\\a$ done",
            "\\def\\a{\\a\\a}$\\a$ done",
            "\\newcommand{\\a}[1]{\\a{#1#1}}$\\a{x}$ done",
            &huge,
        ] {
            let (doc, warnings) = read(source);
            assert!(format!("{:?}", doc.blocks).contains("done"), "{source}");
            assert_eq!(warnings.len(), 1, "{source}");
            assert!(warnings[0].one.contains("unexpanded"), "{source}");
        }
    }

    /// Macros can nest a formula far deeper than the source it's from, and
    /// writing it still holds.
    #[test]
    fn a_formula_nested_by_macros_is_written() {
        let mut source = String::from("\\def\\xa{{{{{{{{{{x}}}}}}}}}}");
        let mut last = 'a';
        for name in 'b'..='z' {
            source.push_str(&format!("\\def\\x{name}{{{{\\x{last}}}{{\\x{last}}}}}"));
            last = name;
        }
        source.push_str("$\\xz$ done");
        let (doc, _) = read(&source);
        let tex_fallback = std::collections::HashSet::new();
        for target in [Target::Pdf, Target::TypstFile] {
            let options = Options {
                target,
                fonts: Vec::new(),
                paper: "a4",
                base_dir: std::path::PathBuf::new(),
                tex_fallback: &tex_fallback,
            };
            write(&doc, &options);
        }
    }
}
