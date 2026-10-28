//! Verbatim text, `\verb|…|` and the `verbatim` environment, cut out of
//! the source before it's parsed: mitex-parser would read it as LaTeX,
//! its braces, `%` and `\end` included. Each is left as a placeholder
//! (see [`Cut::mark`]) that the reader puts it back at.

use std::ops::Range;

/// The name placeholders start from (see [`Cut::mark`]).
const MARK: &str = "\\SwiftShifterVerbatim";

/// What was cut out: `\verb`'s text, or the `verbatim` environment's.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Verbatim {
    Inline(String),
    Block(String),
}

/// A source with its verbatim text cut out (see [`cut`]).
#[derive(Debug, PartialEq)]
pub(super) struct Cut {
    /// The source, placeholders where the verbatim text was.
    pub(super) source: String,
    /// What was cut out, in order.
    pub(super) verbatims: Vec<Verbatim>,
    /// The command a placeholder is, its argument the index of what it
    /// stands for, and the line breaks that stood there, so lines are
    /// counted as in the source. It's a control word the source doesn't
    /// hold, so no command of its own is taken for one.
    pub(super) mark: String,
    /// Where each placeholder is in [`Cut::source`], and where what it
    /// stands for was in the source, in the order of [`Cut::verbatims`].
    pub(super) spans: Vec<(Range<usize>, Range<usize>)>,
}

impl Cut {
    /// Where `at`, a place in [`Cut::source`] outside its placeholders, is
    /// in the source.
    pub(super) fn in_source(&self, at: usize) -> usize {
        // As far past the last placeholder before it as past what that
        // stands for.
        let before = self
            .spans
            .partition_point(|(placeholder, _)| placeholder.end <= at);
        match before.checked_sub(1).map(|last| &self.spans[last]) {
            Some((placeholder, verbatim)) => verbatim.end + (at - placeholder.end),
            None => at,
        }
    }
}

/// `source` with its verbatim text cut out. What's in a comment or in
/// what the parser skips (`\iffalse … \fi`, see [`skipped`]) isn't cut,
/// and `\verb` or `\begin{verbatim}` left unclosed is left as it is.
///
/// A placeholder has three characters other than letters, digits and
/// spaces (`\`, `{`, `}`), and what it stands for has at least as many
/// (`\`, and `\verb`'s two delimiters or `\begin`'s braces), so the reader
/// nests no deeper than the source's bound says (see `nesting_bound`).
pub(super) fn cut(source: &str) -> Cut {
    let mark = mark(source);
    let mut out = String::with_capacity(source.len());
    let mut cut = Vec::new();
    let mut spans = Vec::new();
    // What's past `copied` isn't in `out` yet; what's past `at` isn't
    // scanned.
    let (mut copied, mut at) = (0, 0);
    let mut unclosed = Vec::new();
    while let Some((start, command)) = next_command(&source[at..], tex_letter) {
        let after = at + start + command.len();
        at = after;
        let taken = match command {
            "\\verb" => inline(&source[after..]),
            "\\begin" => block(&source[after..], &mut unclosed),
            "\\iffalse" | "\\iftypst" => {
                at += skipped(&source[after..]);
                None
            }
            _ => None,
        };
        if let Some((verbatim, len)) = taken {
            let breaks: String = source[after..after + len]
                .chars()
                .filter(|c| matches!(c, '\n' | '\r'))
                .collect();
            let start = after - command.len();
            out.push_str(&source[copied..start]);
            let placeholder = out.len();
            out.push_str(&format!("{mark}{{{}{breaks}}}", cut.len()));
            spans.push((placeholder..out.len(), start..after + len));
            cut.push(verbatim);
            copied = after + len;
            at = copied;
        }
    }
    out.push_str(&source[copied..]);
    Cut {
        source: out,
        verbatims: cut,
        mark,
        spans,
    }
}

/// The command placeholders in `source` are (see [`Cut::mark`]): [`MARK`]
/// and the first of `A`, `B`, …, `Z`, `AA`, … after it that makes no
/// control word of the source's. It's never longer than the count of those
/// starting with [`MARK`] takes to write, so placeholders stay short.
fn mark(source: &str) -> String {
    let taken: std::collections::HashSet<&str> = source
        .match_indices(MARK)
        .map(|(at, _)| {
            let letters = source[at + MARK.len()..]
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(source.len() - at - MARK.len());
            &source[at + MARK.len()..at + MARK.len() + letters]
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
        .find(|suffix| !taken.contains(suffix.as_str()))
        .map(|suffix| format!("{MARK}{suffix}"))
        .expect("more names than the source holds")
}

/// The next command in `text`, past comments: where it starts, and it, a
/// control word of the characters `in_name` takes, or a control symbol
/// (`\\`, `\%`), whose second character starts nothing.
pub(super) fn next_command(text: &str, in_name: fn(char) -> bool) -> Option<(usize, &str)> {
    let mut at = 0;
    loop {
        at += text[at..].find(['\\', '%'])?;
        if text[at..].starts_with('%') {
            at += text[at..].find(['\n', '\r'])?;
            continue;
        }
        let rest = &text[at + 1..];
        let name = match rest.find(|c: char| !in_name(c)) {
            Some(0) => rest.chars().next().map_or(0, char::len_utf8),
            Some(letters) => letters,
            None => rest.len(),
        };
        return Some((at, &text[at..at + 1 + name]));
    }
}

/// What a control word is made of in a document, as TeX reads it.
pub(super) fn tex_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
}

/// What the parser takes a command's name to be made of: `@` too, as in
/// `\@ifstar`.
pub(super) fn parser_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '@'
}

/// How much of `text`, after `\iffalse`, the parser skips, as it does: to
/// the `\fi` that ends it, past those of the conditionals it holds, or to
/// the end, its commands named as it names them.
pub(super) fn skipped(text: &str) -> usize {
    let mut nested = 0;
    let mut at = 0;
    while let Some((start, command)) = next_command(&text[at..], parser_letter) {
        at += start + command.len();
        if command == "\\fi" {
            if nested == 0 {
                return at;
            }
            nested -= 1;
        } else if CONDITIONALS.contains(&command) {
            nested += 1;
        }
    }
    text.len()
}

/// The conditionals the parser nests when it skips (see [`skipped`]).
pub(super) const CONDITIONALS: [&str; 18] = [
    "\\@ifstar",
    "\\if",
    "\\iftypst",
    "\\iffalse",
    "\\iftrue",
    "\\ifcase",
    "\\ifnum",
    "\\ifcat",
    "\\ifx",
    "\\ifvoid",
    "\\ifhbox",
    "\\ifvbox",
    "\\ifhmode",
    "\\ifmmode",
    "\\ifvmode",
    "\\ifinner",
    "\\ifdim",
    "\\ifeof",
];

/// `\verb`'s text, from what's after its name: a star, then a delimiter
/// (no letter or space), the text, and the delimiter again on its line;
/// and how long all that is.
pub(super) fn inline(after: &str) -> Option<(Verbatim, usize)> {
    let star = usize::from(after.starts_with('*'));
    let delimiter = after[star..]
        .chars()
        .next()
        .filter(|c| !c.is_alphabetic() && !c.is_whitespace())?;
    let opened = star + delimiter.len_utf8();
    let end = after[opened..].find([delimiter, '\n', '\r'])?;
    after[opened + end..].starts_with(delimiter).then(|| {
        let text = after[opened..opened + end].to_string();
        (Verbatim::Inline(text), opened + end + delimiter.len_utf8())
    })
}

#[cfg(test)]
thread_local! {
    /// How many times `block` has looked for an `\end` on this thread, for
    /// tests to bound it without a clock.
    static END_SEARCHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The `verbatim` environment's text, from what's after `\begin`: past
/// what TeX skips before an argument (see [`before_argument`]),
/// `{verbatim}` (or `{verbatim*}`) to the first `\end` of it, without the
/// line breaks right after its start and right before its end, as LaTeX
/// sets it; and how long all that is. `unclosed` holds the environments
/// found with no `\end`, which aren't looked for again: there's none past
/// a later `\begin` either.
pub(super) fn block(after: &str, unclosed: &mut Vec<&'static str>) -> Option<(Verbatim, usize)> {
    let skipped = before_argument(after);
    let after_name = &after[skipped..];
    let name = ["{verbatim}", "{verbatim*}"]
        .into_iter()
        .find(|name| after_name.starts_with(name))?;
    if unclosed.contains(&name) {
        return None;
    }
    #[cfg(test)]
    END_SEARCHES.with(|n| n.set(n.get() + 1));
    let end = format!("\\end{name}");
    let start = skipped + name.len();
    let Some(close) = after[start..].find(&end) else {
        unclosed.push(name);
        return None;
    };
    let text = &after[start..start + close];
    let text = ["\r\n", "\n", "\r"]
        .into_iter()
        .find_map(|line_end| text.strip_prefix(line_end))
        .unwrap_or(text);
    let text = ["\r\n", "\n", "\r"]
        .into_iter()
        .find_map(|line_end| text.strip_suffix(line_end))
        .unwrap_or(text);
    Some((Verbatim::Block(text.to_string()), start + close + end.len()))
}

/// How much of `text`, after a command's name, TeX skips before its
/// argument: spaces, a line's end and comments, but not an empty line,
/// which would be the argument.
fn before_argument(text: &str) -> usize {
    let mut at = 0;
    // A line has ended since the name, so another end makes an empty line.
    let mut line_ended = false;
    loop {
        let rest = &text[at..];
        if rest.starts_with([' ', '\t']) {
            at += 1;
        } else if rest.starts_with('%') {
            // A comment's line end makes no space, but ends the line.
            at += rest.find(['\n', '\r']).unwrap_or(rest.len());
            at += line_end(&text[at..]);
            line_ended = true;
        } else if line_end(rest) > 0 && !line_ended {
            at += line_end(rest);
            line_ended = true;
        } else {
            return at;
        }
    }
}

/// How long the line end at the start of `text` is: `\r\n`, `\n` or `\r`.
fn line_end(text: &str) -> usize {
    if text.starts_with("\r\n") {
        2
    } else {
        usize::from(text.starts_with(['\n', '\r']))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(text: &str) -> Verbatim {
        Verbatim::Inline(text.into())
    }

    fn block(text: &str) -> Verbatim {
        Verbatim::Block(text.into())
    }

    /// Each row: the source, what it's left as, and what's cut out.
    #[test]
    fn verbatim_text_is_cut_out_as_latex_reads_it() {
        let mark = |n: &str| format!("{MARK}{{{n}}}");
        let cases: Vec<(&str, String, Vec<Verbatim>)> = vec![
            (
                "a \\verb|x{y}%z| b",
                format!("a {} b", mark("0")),
                vec![inline("x{y}%z")],
            ),
            ("\\verb*+a b+", mark("0"), vec![inline("a b")]),
            (
                "\\verb!1!\\verb\"2\"",
                format!("{}{}", mark("0"), mark("1")),
                vec![inline("1"), inline("2")],
            ),
            (
                "\\begin{verbatim}\n{%x}\\end{y}\n  z\n\\end{verbatim}\nafter",
                format!("{}\nafter", mark("0\n\n\n")),
                vec![block("{%x}\\end{y}\n  z")],
            ),
            (
                "\\begin{verbatim*}a b\\end{verbatim*}",
                mark("0"),
                vec![block("a b")],
            ),
            (
                "\\begin{verbatim}\r\nx\r\n\\end{verbatim}",
                mark("0\r\n\r\n"),
                vec![block("x")],
            ),
            // Spaces, a line's end and comments may come before the name,
            // as before any argument (#167); an empty line may not.
            (
                "\\begin {verbatim}\n$x$\n\\end{verbatim}",
                mark("0\n\n"),
                vec![block("$x$")],
            ),
            (
                "\\begin\n  {verbatim}x\\end{verbatim}",
                mark("0\n"),
                vec![block("x")],
            ),
            (
                "\\begin%c\n{verbatim*}x\\end{verbatim*}",
                mark("0\n"),
                vec![block("x")],
            ),
            (
                "\\begin\n\n{verbatim}x\\end{verbatim}",
                "\\begin\n\n{verbatim}x\\end{verbatim}".to_string(),
                vec![],
            ),
        ];
        for (source, left, verbatims) in cases {
            let cut = cut(source);
            assert_eq!((cut.source, cut.verbatims), (left, verbatims), "{source}");
        }
    }

    /// A placeholder is a command the source doesn't hold, so one of the
    /// source's own, of the name placeholders start from, is no placeholder.
    #[test]
    fn placeholders_are_named_apart_from_the_source() {
        let source = format!("{MARK}{{0}} {MARK}A {MARK}AB \\verb|x|");
        let cut = cut(&source);
        assert_eq!(cut.mark, format!("{MARK}B"));
        assert_eq!(
            cut.source,
            format!("{MARK}{{0}} {MARK}A {MARK}AB {MARK}B{{0}}")
        );
        assert_eq!(cut.verbatims, [inline("x")]);
        // A long name of the source's doesn't make every placeholder long.
        let source = format!("%{MARK}{}\n{}", "X".repeat(5000), "\\verb|x|".repeat(1000));
        let cut = super::cut(&source);
        assert_eq!(cut.mark, MARK);
        assert!(cut.source.len() < 4 * source.len());
    }

    /// Unclosed environments, one after another, are looked for an `\end`
    /// once, not once each.
    #[test]
    fn unclosed_environments_are_looked_for_once() {
        let source = "\\begin{verbatim}x\\end{other}".repeat(1000);
        END_SEARCHES.with(|n| n.set(0));
        let cut = cut(&source);
        assert_eq!(cut.source, source);
        assert_eq!(END_SEARCHES.with(std::cell::Cell::get), 1);
    }

    /// What the parser skips, `\iffalse` to its `\fi`, is left as it is,
    /// past the conditionals it holds, so the parser finds its end.
    #[test]
    fn what_the_parser_skips_is_left() {
        for source in [
            "\\iffalse\\verb|\\fi| visible",
            // Named as the parser names it: `\@ifstar` is a conditional.
            "\\iffalse \\@ifstar \\fi \\verb|\\fi| shown",
            "\\iffalse \\ifx a\\fi % \\fi\n\\begin{verbatim}x\\end{verbatim}\\fi",
        ] {
            let cut = cut(source);
            assert_eq!(
                (cut.source.as_str(), cut.verbatims),
                (source, Vec::new()),
                "{source}"
            );
        }
        let cut = cut("\\iffalse x\\fi\\verb|y|");
        assert_eq!(cut.source, format!("\\iffalse x\\fi{MARK}{{0}}"));
    }

    /// In a comment, after `\\`, unclosed, or across a line's end, it's
    /// left as it is.
    #[test]
    fn what_is_no_verbatim_is_left() {
        for source in [
            "% \\verb|x| \\begin{verbatim}",
            "\\\\verb|x|",
            "\\verb|x",
            "\\verb|x\n|",
            "\\verb x",
            "\\verb",
            "\\verbatim|x|",
            "\\begin{verbatim} unclosed",
            "\\begin{Verbatim}x\\end{Verbatim}",
            "50\\% \\%verb|x|",
        ] {
            let cut = cut(source);
            assert_eq!(
                (cut.source.as_str(), cut.verbatims),
                (source, Vec::new()),
                "{source}"
            );
        }
    }
}
