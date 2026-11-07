//! The branches TeX reads of an `\iffalse` and an `\iftrue`, kept before
//! the source is parsed, and only those: mitex-parser skips from `\iffalse`
//! to its `\fi`, where TeX reads on from `\else` (#185), and reads all of an
//! `\iftrue`, its `\else` branch too.
//!
//! This runs on the source as written, before [`super::verbatim::cut`], so
//! that verbatim text in the branch is cut out as any other is.

use super::verbatim::{self, CONDITIONALS, next_command, parser_letter, tex_letter};

/// What the parser reads as nothing and the spaces after it gone, as after
/// the control words it stands for.
const NOTHING: &str = "\\iffalse\\fi";

/// A conditional open where [`keep_else`] is.
#[derive(PartialEq)]
enum Open {
    /// An `\iffalse`'s `\else` branch, whose `\fi` goes.
    Kept,
    /// An `\iftrue`, whose `\else` branch goes, and its `\fi`.
    True,
    /// Any other: which branch TeX reads isn't known here, so both are read.
    Other,
}

/// `source` with each `\iffalse … \else`, and the `\fi` of the `\else`,
/// shown as nothing, and each `\iftrue`, its `\else … \fi` and its `\fi`
/// too. An `\iffalse` with no `\else` is left to the parser. What's in
/// comments and verbatim text is passed by, as is what TeX doesn't read.
pub(super) fn keep_else(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let (mut copied, mut at) = (0, 0);
    // The conditionals open where the scan is, innermost last.
    let mut open: Vec<Open> = Vec::new();
    let mut unclosed = Vec::new();
    while let Some((start, name)) = next_command(&source[at..], tex_letter) {
        let command = at + start;
        at = command + name.len();
        match name {
            "\\iffalse" => {
                let (len, at_else) = branch(&source[at..], true);
                at += len;
                if at_else {
                    out.push_str(&source[copied..command]);
                    push_nothing(&mut out, &source[at..]);
                    copied = at;
                    open.push(Open::Kept);
                }
            }
            "\\iftypst" => at += verbatim::skipped(&source[at..]),
            "\\else" if open.last() == Some(&Open::True) => {
                at += branch(&source[at..], false).0;
                open.pop();
                out.push_str(&source[copied..command]);
                push_nothing(&mut out, &source[at..]);
                copied = at;
            }
            "\\fi" => {
                if matches!(open.pop(), Some(Open::Kept | Open::True)) {
                    out.push_str(&source[copied..command]);
                    push_nothing(&mut out, &source[at..]);
                    copied = at;
                }
            }
            "\\verb" => at += verbatim::inline(&source[at..]).map_or(0, |(_, len)| len),
            "\\begin" => {
                at += verbatim::block(&source[at..], &mut unclosed).map_or(0, |(_, len)| len);
            }
            "\\iftrue" => {
                out.push_str(&source[copied..command]);
                push_nothing(&mut out, &source[at..]);
                copied = at;
                open.push(Open::True);
            }
            _ if CONDITIONALS.contains(&name) => open.push(Open::Other),
            _ => {}
        }
    }
    out.push_str(&source[copied..]);
    out
}

/// Appends [`NOTHING`] to `out`, before `after`: with `{}` to end it where
/// `after` starts with `@`, which the parser would take into `\fi`'s name,
/// so that it skipped on past it.
fn push_nothing(out: &mut String, after: &str) {
    out.push_str(NOTHING);
    if after.starts_with('@') {
        out.push_str("{}");
    }
}

/// How much of `text`, a conditional's branch, TeX skips: to the `\fi` that
/// ends it, or, `to_else`, an `\else`, past those of the conditionals it
/// holds; or to the end. Whether it ends at an `\else`, after which TeX
/// reads. Commands are named as the parser names them, and verbatim text is
/// no different here, as TeX, skipping, doesn't see it; but an `\else` or
/// `\fi` is one as TeX names it, so `\fi@x` is `\fi` and the text `@x`.
///
/// `files.rs` has the same `branch`, for pandoc. That file finds what the
/// app really converts today, so it isn't touched for a reader that isn't
/// in use yet.
fn branch(text: &str, to_else: bool) -> (usize, bool) {
    let mut nested = 0;
    let mut at = 0;
    while let Some((start, command)) = next_command(&text[at..], parser_letter) {
        at += start + command.len();
        let letters = command[1..]
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(command.len() - 1);
        let tex_name = &command[..1 + letters];
        let command = if matches!(tex_name, "\\else" | "\\fi") {
            at -= command.len() - tex_name.len();
            tex_name
        } else {
            command
        };
        match command {
            "\\else" if nested == 0 && to_else => return (at, true),
            "\\fi" if nested == 0 => return (at, false),
            "\\fi" => nested -= 1,
            _ if CONDITIONALS.contains(&command) => nested += 1,
            _ => {}
        }
    }
    (text.len(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_else_branch_is_kept() {
        for (source, expected) in [
            (
                "\\iffalse A\\else B\\fi C",
                "\\iffalse\\fi B\\iffalse\\fi C",
            ),
            // Past the conditionals the false branch holds.
            (
                "\\iffalse\\ifx a\\else b\\fi\\else B\\fi",
                "\\iffalse\\fi B\\iffalse\\fi",
            ),
            // Its own conditionals keep their `\fi`.
            (
                "\\iffalse A\\else\\ifx a b\\fi\\fi",
                "\\iffalse\\fi\\ifx a b\\fi\\iffalse\\fi",
            ),
            (
                "\\iffalse A\\else\\iffalse B\\else C\\fi\\fi",
                "\\iffalse\\fi\\iffalse\\fi C\\iffalse\\fi\\iffalse\\fi",
            ),
            // In one that TeX reads, and with no `\fi` to end it.
            (
                "\\ifx a\\iffalse A\\else B\\fi\\fi",
                "\\ifx a\\iffalse\\fi B\\iffalse\\fi\\fi",
            ),
            ("\\iffalse A\\else B", "\\iffalse\\fi B"),
            // An `\iftrue`'s first branch alone, and in what it reads.
            (
                "\\iftrue A\\else X\\fi\\iffalse Y\\else Z\\fi",
                "\\iffalse\\fi A\\iffalse\\fi\\iffalse\\fi Z\\iffalse\\fi",
            ),
            (
                "\\iftrue\\iffalse Y\\else Z\\fi\\fi",
                "\\iffalse\\fi\\iffalse\\fi Z\\iffalse\\fi\\iffalse\\fi",
            ),
            ("\\iftrue A\\fi B", "\\iffalse\\fi A\\iffalse\\fi B"),
            (
                "\\iftrue A\\else\\iffalse X\\else B\\fi\\fi",
                "\\iffalse\\fi A\\iffalse\\fi",
            ),
            (
                "\\iftrue\\iftrue A\\else\\iffalse X\\else B\\fi\\fi\\fi",
                "\\iffalse\\fi\\iffalse\\fi A\\iffalse\\fi\\iffalse\\fi",
            ),
            // `@` after a `\fi` isn't of its name, as TeX names it.
            (
                "\\iftrue A\\fi@b.c d",
                "\\iffalse\\fi A\\iffalse\\fi{}@b.c d",
            ),
            (
                "\\iftrue A\\else B\\fi@b.c d",
                "\\iffalse\\fi A\\iffalse\\fi{}@b.c d",
            ),
            (
                "\\iffalse A\\fi@x\\else B\\fi",
                "\\iffalse A\\fi@x\\else B\\fi",
            ),
            (
                "\\iffalse A\\else B\\fi@x",
                "\\iffalse\\fi B\\iffalse\\fi{}@x",
            ),
            // In an `\iffalse`'s `\else` branch (#73 item 18b).
            (
                "\\iffalse A\\else\\iftrue B\\else C\\fi\\fi D",
                "\\iffalse\\fi\\iffalse\\fi B\\iffalse\\fi\\iffalse\\fi D",
            ),
        ] {
            assert_eq!(keep_else(source), expected, "{source}");
        }
    }

    #[test]
    fn what_is_no_else_branch_is_left() {
        for source in [
            "no conditionals",
            "",
            "\\iffalse A\\fi B",
            "\\iffalse \\ifx a\\else b\\fi\\fi C",
            "\\iffalse A",
            // In what the parser skips, or isn't LaTeX.
            "\\iftypst\\iffalse A\\else B\\fi\\fi",
            "% \\iffalse A\\else B\\fi\n",
            "\\verb|\\iffalse| A\\else B\\fi",
            "\\begin{verbatim}\\iffalse A\\else B\\fi\\end{verbatim}",
            // No `\else` of an `\iffalse`.
            "\\ifx a\\else b\\fi",
            "a\\fi b",
        ] {
            assert_eq!(keep_else(source), source, "{source}");
        }
    }

    /// `\else` in a comment isn't one; in `\verb` it is, as TeX, skipping,
    /// doesn't see a `\verb`.
    #[test]
    fn a_false_branch_is_read_as_tex_skips_it() {
        assert_eq!(
            keep_else("\\iffalse% \\else\nA\\fi B"),
            "\\iffalse% \\else\nA\\fi B"
        );
        assert_eq!(
            keep_else("\\iffalse\\verb|\\else|B\\fi"),
            "\\iffalse\\fi|B\\iffalse\\fi"
        );
    }
}
