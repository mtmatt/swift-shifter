//! Where a LaTeX source names files: `\input`, `\include`, the `import`
//! package's commands and `\includegraphics`, with where their arguments
//! are. It's for converting a document whose files are in folders of their
//! own (#170): their paths are rewritten, and pandoc reads the files in
//! itself, so commands are found where pandoc 3.9 reads them: past
//! comments, verbatim text (taken as the reader takes it, see
//! [`super::verbatim`]), the false branch of an `\iffalse`, though not its
//! `\else` (#185), and an `\iftrue`'s `\else`.
//!
//! Known limit: `\input file` without braces isn't found.

use std::ops::Range;

use super::verbatim::{self, CONDITIONALS, next_command, parser_letter, tex_letter};

/// A command that names files, and where its arguments are.
#[derive(Debug, PartialEq)]
pub struct Named {
    /// As written, without its star: `\input`, `\subimport`, ….
    pub command: &'static str,
    /// Where it is in the source, from its name to its last argument's
    /// `}`.
    pub range: Range<usize>,
    /// Each file or folder it names: where its text is in the source,
    /// between its braces, in order (an `\import`'s folder, then its file).
    pub args: Vec<Range<usize>>,
}

/// The commands that name files, and how many they name: an `\import`, a
/// folder and a file in it.
const COMMANDS: [(&str, usize); 9] = [
    ("\\input", 1),
    ("\\include", 1),
    ("\\includegraphics", 1),
    ("\\import", 2),
    ("\\subimport", 2),
    ("\\inputfrom", 2),
    ("\\subinputfrom", 2),
    ("\\includefrom", 2),
    ("\\subincludefrom", 2),
];

/// The commands in `source` that name files, in order.
// Read by the conversions of `.tex` in a later PR (#170).
#[allow(dead_code)]
pub fn named(source: &str) -> Vec<Named> {
    let cut = verbatim::cut(source);
    let text = cut.source.as_str();
    let mut named = Vec::new();
    let mut at = 0;
    // The conditionals open where the scan is, innermost last: whether
    // each is an `\iftrue`, whose `\else` pandoc doesn't read.
    let mut open: Vec<bool> = Vec::new();
    let mut unclosed = Vec::new();
    while let Some((start, name)) = next_command(&text[at..], tex_letter) {
        at += start + name.len();
        match name {
            "\\iffalse" => {
                let (len, at_else) = branch(&text[at..], true);
                at += len;
                if at_else {
                    open.push(false);
                }
                continue;
            }
            "\\else" if open.last() == Some(&true) => {
                at += branch(&text[at..], false).0;
                open.pop();
                continue;
            }
            "\\fi" => {
                open.pop();
                continue;
            }
            // Verbatim text the cut left, in what it skips (an `\iffalse`'s
            // `\else`, `\iftypst`), as the cut would take it.
            "\\verb" => {
                at += verbatim::inline(&text[at..]).map_or(0, |(_, len)| len);
                continue;
            }
            "\\begin" => {
                at += verbatim::block(&text[at..], &mut unclosed).map_or(0, |(_, len)| len);
                continue;
            }
            _ if CONDITIONALS.contains(&name) => {
                open.push(name == "\\iftrue");
                continue;
            }
            _ => {}
        }
        let Some(&(command, count)) = COMMANDS.iter().find(|(command, _)| *command == name) else {
            continue;
        };
        if let Some((len, args)) = arguments(&text[at..], command, count) {
            let args = args
                .into_iter()
                .map(|arg| cut.in_source(at + arg.start)..cut.in_source(at + arg.end))
                .collect();
            let range = cut.in_source(at - name.len())..cut.in_source(at + len);
            named.push(Named {
                command,
                range,
                args,
            });
            at += len;
        }
    }
    named
}

/// How much of `text`, a conditional's branch, pandoc leaves unread: to
/// the `\fi` that ends it, or, `to_else`, an `\else`, past those of the
/// conditionals it holds; or to the end. Whether it ends at an `\else`,
/// after which pandoc reads.
fn branch(text: &str, to_else: bool) -> (usize, bool) {
    let mut nested = 0;
    let mut at = 0;
    while let Some((start, command)) = next_command(&text[at..], parser_letter) {
        at += start + command.len();
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

/// `command`'s arguments at the start of `text`, after its name: how long
/// they are, and where the text of each of the `count` it names files with
/// is. A star, and `\includegraphics`'s options in brackets, come first.
fn arguments(text: &str, command: &str, count: usize) -> Option<(usize, Vec<Range<usize>>)> {
    let mut at = skip(text)?;
    if text[at..].starts_with('*') {
        at += 1;
    }
    if command == "\\includegraphics" {
        // `[options]`, or `[lower left][upper right]`.
        for _ in 0..2 {
            let before = at + skip(&text[at..])?;
            if !text[before..].starts_with('[') {
                break;
            }
            at = before + closing(&text[before..], ']')? + 1;
        }
    }
    let mut args = Vec::new();
    for _ in 0..count {
        at += skip(&text[at..])?;
        let inner = group(&text[at..])?;
        args.push(at + inner.start..at + inner.end);
        at += inner.end + 1;
    }
    Some((at, args))
}

/// How much of `text` TeX skips before an argument: spaces, a line's end
/// and comments, but not an empty line, which ends a paragraph and with it
/// the command. None where there's such a line.
fn skip(text: &str) -> Option<usize> {
    let mut at = 0;
    // A line's end has been passed, so another would make an empty line.
    let mut line_ended = false;
    loop {
        let rest = &text[at..];
        match rest.chars().next() {
            Some(' ' | '\t') => at += 1,
            // A comment's line end makes no space, but ends a line.
            Some('%') => {
                at += rest.find(['\n', '\r']).unwrap_or(rest.len());
                at += line_end(&text[at..]);
                line_ended = true;
            }
            Some('\n' | '\r') if line_ended => return None,
            Some('\n' | '\r') => {
                at += line_end(rest);
                line_ended = true;
            }
            _ => return Some(at),
        }
    }
}

/// Where the text of the group at the start of `text` is, between its
/// braces, the groups it holds kept.
fn group(text: &str) -> Option<Range<usize>> {
    if !text.starts_with('{') {
        return None;
    }
    Some(1..closing(text, '}')?)
}

/// How far an argument that names a file, or its options, is looked for:
/// none is longer, and an unclosed one is looked past no further, so that
/// a source of them is scanned in a time its length bounds.
const MAX_ARGUMENT: usize = 4 << 10;

/// Where the `close` that ends what opens at the start of `text` (`{` or
/// `[`) is: past the groups in it, comments and escaped characters (`\}`,
/// `\%`), as TeX reads them, within [`MAX_ARGUMENT`].
fn closing(text: &str, close: char) -> Option<usize> {
    let mut depth = 0;
    let mut chars = text
        .char_indices()
        .take_while(|&(at, _)| at < MAX_ARGUMENT)
        .skip(1);
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '%' => {
                chars.find(|&(_, c)| matches!(c, '\n' | '\r'));
            }
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if c == close && depth == 0 => return Some(at),
            // A `}` closing what's around: no argument here.
            '}' => return None,
            _ => {}
        }
    }
    None
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

    /// `source`'s commands that name files, and their arguments' text.
    fn found(source: &str) -> Vec<(&'static str, Vec<&str>)> {
        named(source)
            .into_iter()
            .map(|named| {
                let args = named.args.into_iter().map(|arg| &source[arg]).collect();
                (named.command, args)
            })
            .collect()
    }

    /// A command is where it is in the source, past verbatim text, from
    /// its name to its last `}`, a star and options in it.
    #[test]
    fn a_command_is_found_whole() {
        let source = "\\verb|x| a \\subimport* {ch/}%c\n{b} \\includegraphics[w=1]{f} c";
        let whole: Vec<_> = named(source)
            .into_iter()
            .map(|named| &source[named.range])
            .collect();
        assert_eq!(
            whole,
            ["\\subimport* {ch/}%c\n{b}", "\\includegraphics[w=1]{f}"]
        );
    }

    #[test]
    fn commands_that_name_files_are_found_with_their_arguments() {
        let source = "\\input{one}\\include{ch/two}\n\
                      \\import{Files/-Preamble/}{-Preamble}\n\
                      \\subincludefrom{Files/01CodingStyle/}{CodingStyle}\n\
                      \\subimport*{a}{b} \\inputfrom{c}{d} \\subinputfrom{e}{f}\n\
                      \\includefrom{g}{h} \\subimport{i}{j}\n\
                      \\includegraphics[width=3cm]{../Images/BIT.png}\n\
                      \\includegraphics*[1,2][3,4]{fig} \\inputs{no}";
        assert_eq!(
            found(source),
            [
                ("\\input", vec!["one"]),
                ("\\include", vec!["ch/two"]),
                ("\\import", vec!["Files/-Preamble/", "-Preamble"]),
                (
                    "\\subincludefrom",
                    vec!["Files/01CodingStyle/", "CodingStyle"]
                ),
                ("\\subimport", vec!["a", "b"]),
                ("\\inputfrom", vec!["c", "d"]),
                ("\\subinputfrom", vec!["e", "f"]),
                ("\\includefrom", vec!["g", "h"]),
                ("\\subimport", vec!["i", "j"]),
                ("\\includegraphics", vec!["../Images/BIT.png"]),
                ("\\includegraphics", vec!["fig"]),
            ]
        );
    }

    #[test]
    fn arguments_are_found_as_tex_finds_them() {
        for (source, command, args) in [
            ("\\input  {one}", "\\input", vec!["one"]),
            ("\\input\n {one}", "\\input", vec!["one"]),
            ("\\input%why\n{one}", "\\input", vec!["one"]),
            ("\\import{a}  %why\n  {b}", "\\import", vec!["a", "b"]),
            // A group is kept whole, a comment in it too.
            ("\\input{\\jobname-x}", "\\input", vec!["\\jobname-x"]),
            ("\\input{{a}b}", "\\input", vec!["{a}b"]),
            ("\\input{ch/on%c\ne}", "\\input", vec!["ch/on%c\ne"]),
            (
                "\\input{chap% } ignored\nter}",
                "\\input",
                vec!["chap% } ignored\nter"],
            ),
            ("\\input{a\\}b}", "\\input", vec!["a\\}b"]),
            // The star and options come after spaces too, and a `]` in a
            // group or comment doesn't end the options.
            ("\\includegraphics *{fig}", "\\includegraphics", vec!["fig"]),
            (
                "\\includegraphics[width=1cm,% ] ignored\nclip={a]}]{fig}",
                "\\includegraphics",
                vec!["fig"],
            ),
        ] {
            assert_eq!(found(source), [(command, args)], "{source:?}");
        }
        // An empty line ends the command; a group isn't optional.
        for source in [
            "\\input\n\n{one}",
            "\\input%why\n\n{one}",
            "\\input one",
            "\\import{a}\n\n{b}",
        ] {
            assert_eq!(found(source), [], "{source:?}");
        }
    }

    #[test]
    fn commands_in_comments_verbatim_or_skipped_text_are_left() {
        let source = "% \\input{a}\n\\verb|%\\input{b}|\n\
                      \\begin{verbatim}\n\\input{c}\n\\end{verbatim}\n\
                      \\begin {verbatim}\n\\input{c}\n\\end{verbatim}\n\
                      \\iffalse \\input{d} \\fi\n\\verb|x| 中文 \\input{f}";
        assert_eq!(found(source), [("\\input", vec!["f"])]);
    }

    /// As pandoc 3.9 reads them: an `\iffalse`'s `\else`, past the
    /// conditionals in its false branch, and mitex's `\iftypst`, which
    /// pandoc doesn't know.
    #[test]
    fn commands_are_found_where_pandoc_reads_them() {
        let source = "\\iffalse \\input{a} \\else \\input{b} \\fi\n\
                      \\iffalse A \\iffalse B \\else \\input{c} \\fi D \\else \\input{d} \\fi\n\
                      \\iftypst \\input{e} \\fi \\iffalse \\input{f}";
        assert_eq!(
            found(source),
            [
                ("\\input", vec!["b"]),
                ("\\input", vec!["d"]),
                ("\\input", vec!["e"]),
            ]
        );
        // An `\iftrue`'s `\else` pandoc doesn't read, past what it holds;
        // what's after its `\fi` it does.
        let source = "\\iftrue \\input{g} \\iffalse x \\else \\input{h} \\fi \
                      \\else \\input{i} \\iftrue \\input{j} \\fi \\fi \\input{k}";
        assert_eq!(
            found(source),
            [
                ("\\input", vec!["g"]),
                ("\\input", vec!["h"]),
                ("\\input", vec!["k"]),
            ]
        );
        // Verbatim text where pandoc reads, which the reader's cut skips,
        // is no command either.
        let source = "\\iffalse x \\else \\verb|\\input{l}| \
                      \\begin{verbatim}\\input{m}\\end{verbatim} \\fi \
                      \\iftypst \\verb|\\input{n}| \\fi \\input{o}";
        assert_eq!(found(source), [("\\input", vec!["o"])]);
    }

    /// An unclosed argument is looked past no further than an argument
    /// goes, so a source of them doesn't take its length squared to scan.
    #[test]
    fn unclosed_arguments_are_looked_for_within_their_length() {
        let source = "\\input{".repeat(10_000);
        assert_eq!(found(&source), []);
        let long = format!("\\input{{{}}}", "x".repeat(MAX_ARGUMENT));
        assert_eq!(found(&long), []);
    }

    /// Where arguments are is told in the source, past verbatim text of
    /// other lengths than what stands for it in the cut.
    #[test]
    fn arguments_are_where_they_are_in_the_source() {
        let source = (0..50)
            .map(|n| format!("\\verb|{}| \\input{{f{n}}} ", "x".repeat(n)))
            .collect::<String>();
        let expected: Vec<_> = (0..50)
            .map(|n| ("\\input", vec![format!("f{n}")]))
            .collect();
        let found: Vec<_> = found(&source)
            .into_iter()
            .map(|(command, args)| (command, args.into_iter().map(String::from).collect()))
            .collect();
        assert_eq!(found, expected);
    }
}
