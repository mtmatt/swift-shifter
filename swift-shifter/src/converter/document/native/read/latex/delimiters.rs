//! A `\left` or `\right` right before a `}`, as `\def\l{\left}`'s, which
//! takes its delimiter from after the macro's call. The parser would take
//! the `}` for its delimiter, and all after it, to a `\right` or the
//! document's end, for what it encloses: a document's title and the rest.
//! So it's put as a command of a name the parser doesn't know, and put back
//! where a macro's TeX is kept (see [`unmarked`]).

use super::verbatim::{next_command, tex_letter};

const MARK: &str = "\\SwiftShifterDelimiter";

/// What a marked command's name ends in, after the mark, and the command.
const MARKED: [(&str, &str); 2] = [("Left", "\\left"), ("Right", "\\right")];

/// `source` with such commands marked, and the mark.
pub(super) struct Marked {
    pub(super) source: String,
    pub(super) mark: String,
}

/// `source` with each `\left` and `\right` that a `}` is next after, past
/// spaces, as the mark (see [`mark_in`]) and `Left` or `Right`.
pub(super) fn mark(source: &str) -> Marked {
    let mark = mark_in(source);
    let mut marked = String::with_capacity(source.len());
    let (mut copied, mut at) = (0, 0);
    while let Some((start, command)) = next_command(&source[at..], tex_letter) {
        let command_start = at + start;
        let command_end = command_start + command.len();
        at = command_end;
        let Some((ending, _)) = MARKED.iter().find(|(_, marked)| *marked == command) else {
            continue;
        };
        let before_brace = source[command_end..]
            .trim_start_matches([' ', '\t', '\n', '\r'])
            .starts_with('}');
        if before_brace {
            marked.push_str(&source[copied..command_start]);
            marked.push_str(&mark);
            marked.push_str(ending);
            copied = command_end;
        }
    }
    marked.push_str(&source[copied..]);
    Marked {
        source: marked,
        mark,
    }
}

/// `text` with what [`mark`] marked with `mark` as it was: whole control
/// words only, so that none of a document's own is taken for one.
pub(super) fn unmarked(text: &str, mark: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (at, _) in text.match_indices(mark) {
        let after_mark = &text[at + mark.len()..];
        let Some((ending, command)) = MARKED
            .iter()
            .find(|(ending, _)| after_mark.starts_with(ending))
        else {
            continue;
        };
        let end = at + mark.len() + ending.len();
        if text[end..].starts_with(tex_letter) {
            continue;
        }
        out.push_str(&text[copied..at]);
        out.push_str(command);
        copied = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// [`MARK`] and the first of none, `A`, `B`, …, `Z`, `AA`, … after it that,
/// with `Left` or `Right` after, makes no control word of the source's: so
/// no command of the document's is taken for one marked. The letters after
/// each [`MARK`] in the source are read once, so this takes linear time.
fn mark_in(source: &str) -> String {
    let taken: std::collections::HashSet<&str> = source
        .match_indices(MARK)
        .map(|(at, _)| {
            let after = &source[at + MARK.len()..];
            let letters = after.find(|c: char| !tex_letter(c)).unwrap_or(after.len());
            &after[..letters]
        })
        .collect();
    (0usize..)
        .map(|n| {
            // `n` in bijective base 26, as spreadsheet columns are named;
            // 0 is none.
            let mut suffix = Vec::new();
            let mut n = n;
            while n > 0 {
                n -= 1;
                suffix.push(b'A' + (n % 26) as u8);
                n /= 26;
            }
            suffix.reverse();
            String::from_utf8(suffix).expect("ASCII letters")
        })
        .find(|suffix| {
            MARKED
                .iter()
                .all(|(ending, _)| !taken.contains(format!("{suffix}{ending}").as_str()))
        })
        .map(|suffix| format!("{MARK}{suffix}"))
        .expect("more names than the source holds")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each row: a source, and it marked, with `M` for the mark.
    #[test]
    fn delimiters_before_a_brace_are_marked() {
        for (source, expected) in [
            ("\\def\\l{\\left}", "\\def\\l{MLeft}"),
            ("\\def\\r{\\right }", "\\def\\r{MRight }"),
            ("{\\left\n}", "{MLeft\n}"),
            // Not a delimiter of its own, nor another command.
            ("$\\left( x \\right)$", "$\\left( x \\right)$"),
            ("$\\left\\{ x \\right\\}$", "$\\left\\{ x \\right\\}$"),
            ("{\\leftarrow}", "{\\leftarrow}"),
            // `\\` is a line break, and `left` text.
            ("{a\\\\left}", "{a\\\\left}"),
            ("{\\\\\\left}", "{\\\\MLeft}"),
            ("é\\left}", "éMLeft}"),
            // A comment is no part of it.
            ("% \\left}\n", "% \\left}\n"),
        ] {
            let marked = mark(source);
            let expected = expected.replace('M', &marked.mark);
            assert_eq!(marked.source, expected, "{source}");
            assert_eq!(unmarked(&marked.source, &marked.mark), source, "{source}");
        }
    }

    /// A mark makes no name the document holds, and a name of the
    /// document's that it starts isn't put back.
    #[test]
    fn the_mark_is_new() {
        for (source, expected) in [
            (
                "\\SwiftShifterDelimiterLeft \\def\\l{\\left}",
                "\\SwiftShifterDelimiterA",
            ),
            (
                "\\SwiftShifterDelimiterRight \\def\\l{\\left}",
                "\\SwiftShifterDelimiterA",
            ),
            (
                "\\SwiftShifterDelimiterLeftX \\def\\l{\\left}",
                "\\SwiftShifterDelimiter",
            ),
        ] {
            let marked = mark(source);
            assert_eq!(marked.mark, expected, "{source}");
            assert_eq!(unmarked(&marked.source, &marked.mark), source, "{source}");
        }
    }

    /// A long name after the mark's start takes no more than reading it.
    #[test]
    fn a_long_name_takes_linear_time() {
        let source = format!("%{MARK}{}\n\\def\\l{{\\left}}", "A".repeat(80_000));
        let start = std::time::Instant::now();
        let marked = mark(&source);
        assert_eq!(marked.mark, MARK);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }
}
