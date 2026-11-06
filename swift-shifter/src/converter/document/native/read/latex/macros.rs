//! Macros in math. A formula stays TeX, for writers to read, and they know
//! none of the document's commands, so what those stand for is written into
//! its source.

use super::{DEFINITIONS, MAX_EXPANSION_DEPTH, Macro, Reader};

impl Reader {
    /// `tex`, a formula, with the macros it calls expanded, as far as
    /// [`Reader::expand`] would; a call that goes past that stays as it is,
    /// and is counted for the same warning. A formula that defines commands
    /// stays as it is: which of its calls mean what is its own affair.
    pub(super) fn expanded_math(&mut self, tex: &str) -> String {
        if defines(tex) {
            return tex.to_string();
        }
        let mut expanded = Tex::default();
        expanded.text.reserve(tex.len());
        self.expand_tex(tex, 0, &mut expanded);
        expanded.text
    }

    /// Appends `tex` to `out` with its macros expanded, `depth` macros deep.
    fn expand_tex(&mut self, tex: &str, depth: usize, out: &mut Tex) {
        let mut rest = tex;
        while let Some(at) = rest.find('\\') {
            out.push(&rest[..at]);
            let (name, after) = rest[at..].split_at(command_length(&rest[at..]));
            rest = after;
            let body = self.definitions.get(name).cloned().flatten();
            let body = body.filter(|body| !defines(&body.source));
            let call = body.and_then(|body| Some((arguments(&body, after)?, body)));
            let Some(((arguments, taken), body)) = call else {
                out.push(name);
                continue;
            };
            let text = (depth < MAX_EXPANSION_DEPTH)
                .then(|| substituted(&body.source, &arguments, self.expansion_left))
                .flatten();
            let Some(text) = text else {
                self.unexpanded += 1;
                out.push(name);
                continue;
            };
            self.expansion_left = self.expansion_left.saturating_sub(text.len() + 1);
            self.expand_tex(&text, depth + 1, out);
            rest = &after[taken..];
        }
        out.push(rest);
    }
}

/// TeX being put together, keeping tokens apart where it's joined.
#[derive(Default)]
struct Tex {
    text: String,
    /// Whether `text` ends in a control word, which letters would run on
    /// from.
    in_control_word: bool,
}

impl Tex {
    /// Appends `tex`, whole tokens, with a space before it if it would run
    /// on from a control word. Spaces there mean nothing in math, and an
    /// argument of `\text` that holds one is its own.
    fn push(&mut self, tex: &str) {
        if tex.is_empty() {
            return;
        }
        if self.in_control_word && tex.starts_with(|c: char| c.is_ascii_alphabetic()) {
            self.text.push(' ');
        }
        self.text.push_str(tex);
        let before_letters = tex.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let backslashes = before_letters.len() - before_letters.trim_end_matches('\\').len();
        self.in_control_word = before_letters.len() < tex.len() && backslashes % 2 == 1;
    }
}

/// Whether `tex` calls a command that defines others.
fn defines(tex: &str) -> bool {
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
/// the call as written.
fn arguments(body: &Macro, after: &str) -> Option<(Vec<String>, usize)> {
    let mut arguments = Vec::with_capacity(body.parameters);
    let mut at = 0;
    if let Some(default) = &body.default {
        let next = at + spaces(&after[at..]);
        let given = after[next..].strip_prefix('[').and_then(|inside| {
            let end = closing(inside, ']')?;
            Some((&inside[..end], next + end + 2))
        });
        match given {
            Some((argument, end)) => {
                arguments.push(argument.to_string());
                at = end;
            }
            None => arguments.push(default.clone()),
        }
    }
    while arguments.len() < body.parameters {
        at += spaces(&after[at..]);
        let rest = &after[at..];
        let (argument, length) = if let Some(inside) = rest.strip_prefix('{') {
            let end = closing(inside, '}')?;
            (&inside[..end], end + 2)
        } else if rest.starts_with('\\') {
            let length = command_length(rest);
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

/// The bytes of spaces `tex` starts with.
fn spaces(tex: &str) -> usize {
    tex.len() - tex.trim_start().len()
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

/// `source`, a macro's, with `arguments` for its `#1`, `#2` and so on, and
/// `#` for its `##`. `None` if that's `limit` bytes or more, before it's all
/// put together.
fn substituted(source: &str, arguments: &[String], limit: usize) -> Option<String> {
    let mut text = Tex::default();
    let mut push = |tex: &str| {
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
        if !push(&source[run..at]) {
            return None;
        }
        chars.next();
        if !push(argument.map_or("#", String::as_str)) {
            return None;
        }
        run = at + 2;
    }
    push(&source[run..]).then_some(text.text)
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
            ("\\def\\x{a}$\\x y$", "a y"),
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
            ("\\def\\x{a}$\\text{\\x b}$", "\\text{a b}"),
            // A control word doesn't run on into what's after it.
            ("\\newcommand{\\f}[1]{\\alpha#1}$\\f{b}$", "\\alpha b"),
            ("\\newcommand{\\f}[1]{\\alpha}$\\f{a}b$", "\\alpha b"),
            ("\\newcommand{\\f}[1]{\\\\#1}$\\f{b}$", "\\\\b"),
            // An optional first argument.
            ("\\newcommand{\\f}[2][d]{#1#2}$\\f{a}$", "da"),
            ("\\newcommand{\\f}[2][d]{#1#2}$\\f[c]{a}$", "ca"),
            ("\\newcommand{\\f}[2][d]{#1#2}$\\f[{]}]{a}$", "{]}a"),
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
                "\\operatorname{ab} x",
            ),
            (
                "\\DeclareMathOperator*{\\op}{ab}$\\op x$",
                "\\operatorname*{ab} x",
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
