//! Formulas written right after one another, set apart before the source is
//! parsed. mitex-parser reads `$$` as one token wherever it is, so in
//! `$a$$b$` it takes the second formula's start for the end of a display
//! formula, and every `$` after it for the opposite of what it is. TeX knows
//! it's in a formula, where `$` ends it, whatever follows.

use super::verbatim::{next_command, skipped, tex_letter};

/// A formula being read, and how many groups deep it started: a `$` deeper
/// is in text in it (`$\text{a $b$}$`).
struct Formula {
    display: bool,
    depth: usize,
}

/// `source` with `{}` after any `$` that ends a formula and has another
/// right after it, which shows nothing and takes no argument where it is.
/// What's in comments, or in what the parser skips (see [`skipped`]), is
/// passed by. An empty line ends a formula, as TeX, finding one in it, ends
/// it with an error, so a stray `$` doesn't turn the rest of the document
/// around.
pub(super) fn separate_formulas(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut formulas: Vec<Formula> = Vec::new();
    let mut depth = 0usize;
    let (mut copied, mut at) = (0, 0);
    while let Some(next) = source[at..].find(['\\', '%', '$', '{', '}', '\n', '\r']) {
        at += next;
        let rest = &source[at..];
        match rest.as_bytes()[0] {
            b'\\' => match next_command(rest, tex_letter) {
                Some((0, command)) => {
                    at += command.len();
                    if matches!(command, "\\iffalse" | "\\iftypst") {
                        at += skipped(&source[at..]);
                    }
                }
                _ => at += 1,
            },
            b'%' => at += rest.find(['\n', '\r']).unwrap_or(rest.len()),
            b'{' => {
                depth += 1;
                at += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                formulas.retain(|formula| formula.depth <= depth);
                at += 1;
            }
            b'$' => {
                let doubled = rest.starts_with("$$");
                match formulas.last() {
                    Some(formula) if formula.depth == depth && !formula.display => {
                        formulas.pop();
                        at += 1;
                        if doubled {
                            out.push_str(&source[copied..at]);
                            out.push_str("{}");
                            copied = at;
                        }
                    }
                    Some(formula) if formula.depth == depth => {
                        if doubled {
                            formulas.pop();
                        }
                        at += if doubled { 2 } else { 1 };
                    }
                    _ => {
                        formulas.push(Formula {
                            display: doubled,
                            depth,
                        });
                        at += if doubled { 2 } else { 1 };
                    }
                }
            }
            _ => {
                at += if rest.starts_with("\r\n") { 2 } else { 1 };
                let next_line = source[at..].trim_start_matches([' ', '\t']);
                if next_line.starts_with(['\n', '\r']) {
                    formulas.clear();
                }
            }
        }
    }
    out.push_str(&source[copied..]);
    out
}

/// `source` with `{}` after any `^` or `_` with nothing but spaces, line
/// breaks and comments before a `}`. mitex-parser takes whatever follows
/// them as their argument, a `}` too, and its group then goes on past where
/// it ends: from `\label{a^}`, a valid name, maybe to the document's end
/// (#202). What's in comments is passed by.
///
/// Known limit: a `$` or `\end` after them is taken the same way, though
/// TeX takes no script without an argument there either.
pub(super) fn close_scripts(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let (mut copied, mut at) = (0, 0);
    while let Some(next) = source[at..].find(['\\', '%', '^', '_']) {
        at += next;
        let rest = &source[at..];
        match rest.as_bytes()[0] {
            b'\\' => at += next_command(rest, tex_letter).map_or(1, |(_, name)| name.len()),
            b'%' => at += rest.find(['\n', '\r']).unwrap_or(rest.len()),
            _ => {
                at += 1;
                if past_blanks(&source[at..]).starts_with('}') {
                    out.push_str(&source[copied..at]);
                    out.push_str("{}");
                    copied = at;
                }
            }
        }
    }
    out.push_str(&source[copied..]);
    out
}

/// `text` past the spaces, line breaks and comments it starts with.
fn past_blanks(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        if !text.starts_with('%') {
            return text;
        }
        text = &text[text.find(['\n', '\r']).unwrap_or(text.len())..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formulas_side_by_side_are_set_apart() {
        for (source, expected) in [
            ("$a$$b$", "$a${}$b$"),
            ("$a$$b$$c$", "$a${}$b${}$c$"),
            // A display formula after an inline one.
            ("$a$$$b$$", "$a${}$$b$$"),
            // Text in a formula holds formulas of its own.
            ("$\\text{$a$$b$}$", "$\\text{$a${}$b$}$"),
            ("$$\\text{$a$$b$}$$", "$$\\text{$a${}$b$}$$"),
            // Past an escaped one, as the second of these is a formula's.
            ("\\$$a$$b$", "\\$$a${}$b$"),
        ] {
            assert_eq!(separate_formulas(source), expected, "{source}");
        }
    }

    #[test]
    fn what_isnt_formulas_side_by_side_is_left() {
        for source in [
            "$$a$$",
            "$$a$$ and $$b$$",
            "$$a$$$$b$$",
            "$a$ $b$",
            "\\$$a$ b",
            "\\\\$a$",
            "% $a$$b$\n",
            "\\iffalse $a$$b$\\fi",
            "$a",
            "no formulas",
            "",
        ] {
            assert_eq!(separate_formulas(source), source, "{source}");
        }
    }

    /// A stray `$` is over at the paragraph's end, as it is for TeX.
    #[test]
    fn an_empty_line_ends_a_formula() {
        assert_eq!(
            separate_formulas("a $ b\n\n$$c$$\n \n$d$$e$"),
            "a $ b\n\n$$c$$\n \n$d${}$e$"
        );
        assert_eq!(separate_formulas("$a\r\n\r\n$$b$$"), "$a\r\n\r\n$$b$$");
    }

    #[test]
    fn scripts_before_a_group_s_end_are_closed() {
        for (source, expected) in [
            ("{a^}", "{a^{}}"),
            ("{a_}", "{a_{}}"),
            ("{a^ }", "{a^{} }"),
            ("{a^\n}", "{a^{}\n}"),
            ("{a^\r\n}", "{a^{}\r\n}"),
            ("{a^% c\n% d\n }", "{a^{}% c\n% d\n }"),
            ("{a^_}", "{a^_{}}"),
            ("{a^}{b_}", "{a^{}}{b_{}}"),
            ("{\\\\^}", "{\\\\^{}}"),
        ] {
            assert_eq!(close_scripts(source), expected, "{source}");
        }
    }

    #[test]
    fn what_isnt_a_script_before_a_group_s_end_is_left() {
        for source in [
            "{a^b}", "{a^{b}}", "{a\\^}", "{a\\_}", "% {a^}\n", "{a^ % c", "a^", "a^ ", "",
        ] {
            assert_eq!(close_scripts(source), source, "{source}");
        }
    }

    /// A script at its end leaves a group's end where it is. These tests
    /// are built into the benchmark too, which has no reader to call.
    #[test]
    fn the_parser_ends_a_group_after_a_script() {
        use mitex_parser::CommandSpec;
        use mitex_parser::syntax::SyntaxKind;
        let groups = |source: &str| {
            mitex_parser::parse_without_macro(source, CommandSpec::new(Default::default()))
                .children()
                .filter(|node| node.kind() == SyntaxKind::ItemCurly)
                .count()
        };
        for source in ["{a^}{b}", "{a_ }{b}", "{a^% c\n}{b}"] {
            assert_eq!(groups(source), 1, "{source}");
            assert_eq!(groups(&close_scripts(source)), 2, "{source}");
        }
    }

    /// The parser reads each formula as one.
    #[test]
    fn the_parser_reads_the_formulas_set_apart() {
        use mitex_parser::CommandSpec;
        use mitex_parser::syntax::SyntaxKind;
        let formulas = |source: &str| {
            mitex_parser::parse_without_macro(source, CommandSpec::new(Default::default()))
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ItemFormula)
                .count()
        };
        let source = "a $x$$y$ b $z$ c $w$$v$ d";
        assert_eq!(formulas(&separate_formulas(source)), 5);
    }
}
