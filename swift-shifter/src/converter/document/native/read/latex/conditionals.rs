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
/// shown as nothing, each `\iffalse … \fi` with no `\else` too, and each
/// `\iftrue`, its `\else … \fi` and its `\fi`. An `\iffalse` with neither
/// is left to the parser, which skips to the end, as TeX does. What's in
/// comments, verbatim text and listings is passed by, as is what TeX
/// doesn't read: `listing_length` says how long a listing is that a
/// command, `\lstinline` or `\begin`, starts, given what's after it, asked
/// of each in order.
pub(super) fn keep_else(
    source: &str,
    mut listing_length: impl FnMut(&str, &str) -> Option<usize>,
) -> String {
    let mut out = String::with_capacity(source.len());
    let (mut copied, mut at) = (0, 0);
    // The conditionals open where the scan is, innermost last.
    let mut open: Vec<Open> = Vec::new();
    let mut unclosed = Vec::new();
    while let Some((start, name)) = next_command(&source[at..], tex_letter) {
        let command = at + start;
        at = command + name.len();
        match name {
            // The parser would skip it too, but to a `\fi` as it names one:
            // past `\fi@x`, which is TeX's `\fi` and the text `@x`.
            "\\iffalse" => {
                let (len, end) = branch(&source[at..], true);
                at += len;
                if end != BranchEnd::None {
                    out.push_str(&source[copied..command]);
                    push_nothing(&mut out, &source[at..]);
                    copied = at;
                }
                if end == BranchEnd::Else {
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
            "\\lstinline" | "\\begin" if let Some(len) = listing_length(name, &source[at..]) => {
                at += len;
            }
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

/// Where a conditional's branch that TeX skips ends (see [`branch`]).
#[derive(PartialEq)]
enum BranchEnd {
    /// At an `\else`, after which TeX reads.
    Else,
    /// At its `\fi`.
    Fi,
    /// At the end of the text: none ends it.
    None,
}

/// How much of `text`, a conditional's branch, TeX skips: to the `\fi` that
/// ends it, or, `to_else`, an `\else`, past those of the conditionals it
/// holds; or to the end. And which it ends at. Commands are named as the
/// parser names them, and verbatim text is no different here, as TeX,
/// skipping, doesn't see it; but an `\else` or `\fi` is one as TeX names
/// it, so `\fi@x` is `\fi` and the text `@x`.
///
/// `files.rs` has the same `branch`, for pandoc. That file finds what the
/// app really converts today, so it isn't touched for a reader that isn't
/// in use yet.
fn branch(text: &str, to_else: bool) -> (usize, BranchEnd) {
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
            "\\else" if nested == 0 && to_else => return (at, BranchEnd::Else),
            "\\fi" if nested == 0 => return (at, BranchEnd::Fi),
            "\\fi" => nested -= 1,
            _ if CONDITIONALS.contains(&command) => nested += 1,
            _ => {}
        }
    }
    (text.len(), BranchEnd::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `source` kept as [`keep_else`] keeps it, in a document with no
    /// listings.
    fn kept(source: &str) -> String {
        keep_else(source, |_, _| None)
    }

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
                "\\iffalse\\fi{}@x\\else B\\fi",
            ),
            // One with no `\else` (#73 item 18c), in an `\iftrue` too.
            ("\\iffalse A\\fi B", "\\iffalse\\fi B"),
            ("\\iffalse \\ifx a\\else b\\fi\\fi C", "\\iffalse\\fi C"),
            ("\\iffalse A\\fi@x B", "\\iffalse\\fi{}@x B"),
            (
                "\\iftrue\\iffalse X\\fi@x\\fi Tail",
                "\\iffalse\\fi\\iffalse\\fi{}@x\\iffalse\\fi Tail",
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
            assert_eq!(kept(source), expected, "{source}");
        }
    }

    #[test]
    fn what_is_no_else_branch_is_left() {
        for source in [
            "no conditionals",
            "",
            // No `\fi`: the parser skips to the end, as TeX.
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
            assert_eq!(kept(source), source, "{source}");
        }
    }

    /// A listing, as `listing_length` finds one, is passed by.
    #[test]
    fn listings_are_passed_by() {
        // A listing between bars.
        let listing_length = |command: &str, after: &str| {
            if command != "\\lstinline" {
                return None;
            }
            let code = after.strip_prefix('|')?;
            code.find('|').map(|end| end + 2)
        };
        let source = "\\lstinline|\\iffalse X\\fi| Y";
        assert_eq!(keep_else(source, listing_length), source);
    }

    /// `\else` in a comment isn't one; in `\verb` it is, as TeX, skipping,
    /// doesn't see a `\verb`.
    #[test]
    fn a_false_branch_is_read_as_tex_skips_it() {
        assert_eq!(kept("\\iffalse% \\else\nA\\fi B"), "\\iffalse\\fi B");
        assert_eq!(
            kept("\\iffalse\\verb|\\else|B\\fi"),
            "\\iffalse\\fi|B\\iffalse\\fi"
        );
    }
}
