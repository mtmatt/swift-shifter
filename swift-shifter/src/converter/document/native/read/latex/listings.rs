//! The `listings` package's code, `\lstinline` and the `lstlisting`
//! environment, cut out of the source before the rest of its verbatim text
//! is (see [`super::verbatim`]): mitex-parser would read it as LaTeX, its
//! braces, `%` and `\end` included. Each is left as a placeholder that the
//! reader puts it back at.
//!
//! This is the reader's own: `verbatim::cut` is also what `files` finds
//! the files read in with (see `inputs`), and is left as it is. So what's in a
//! comment, verbatim text, or what the parser skips is passed by as it
//! passes it by.

use super::verbatim::{self, next_command, tex_letter};

/// The name placeholders start from (see [`Cut::mark`]).
const MARK: &str = "\\SwiftShifterListing";

/// The bytes past `\lstinline` looked at for its code, more than code on a
/// line has: one left unclosed is looked past again by each after it, so
/// a line of them costs no more than this for each.
const MAX_INLINE: usize = 1000;

/// What was cut out.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Listing {
    /// `\lstinline`'s code.
    Inline(String),
    /// A `lstlisting`'s code, and its options as written between brackets.
    Block {
        code: String,
        options: Option<String>,
    },
}

/// A source with its listings cut out (see [`cut`]).
pub(super) struct Cut {
    /// The source, placeholders where the listings were.
    pub(super) source: String,
    /// What was cut out, in order.
    pub(super) listings: Vec<Listing>,
    /// The command a placeholder is, its argument the index of what it
    /// stands for: a control word the source doesn't hold.
    pub(super) mark: String,
}

/// `source` with its listings cut out (see [`cut_into`]).
pub(super) fn cut(source: &str) -> Cut {
    let mark = mark(source);
    let mut listings = Vec::new();
    let source = cut_into(source, &mark, &mut listings);
    Cut {
        source,
        listings,
        mark,
    }
}

/// `source` with its listings cut out, appended to `listings`, each left as
/// a placeholder of `mark`. One left unclosed is left as it is, and so is one
/// whose code or options hold a macro's parameter (`#1`, `##`), for the
/// macro's text, its arguments in it, to be cut again (see
/// `Reader::expand_call`).
pub(super) fn cut_into(source: &str, mark: &str, listings: &mut Vec<Listing>) -> String {
    let mut out = String::with_capacity(source.len());
    let (mut copied, mut at) = (0, 0);
    let mut unclosed = Vec::new();
    let mut listing_unclosed = false;
    while let Some((start, command)) = next_command(&source[at..], tex_letter) {
        let after = at + start + command.len();
        at = after;
        let rest = &source[after..];
        let taken = match command {
            "\\lstinline" => inline(rest),
            "\\begin" => match verbatim::block(rest, &mut unclosed) {
                // Verbatim text, which isn't a listing's.
                Some((_, len)) => {
                    at += len;
                    None
                }
                None if listing_unclosed => None,
                None => {
                    let block = block(rest);
                    listing_unclosed = block.is_none() && is_listing(rest);
                    block
                }
            },
            "\\verb" => {
                at += verbatim::inline(rest).map_or(0, |(_, len)| len);
                None
            }
            "\\iffalse" | "\\iftypst" => {
                at += verbatim::skipped(rest);
                None
            }
            _ => None,
        };
        let taken = taken.filter(|(listing, _)| !holds_parameter(listing));
        if let Some((listing, len)) = taken {
            out.push_str(&source[copied..after - command.len()]);
            out.push_str(&format!("{mark}{{{}}}", listings.len()));
            listings.push(listing);
            copied = after + len;
            at = copied;
        }
    }
    out.push_str(&source[copied..]);
    out
}

/// Whether `listing`'s code, or its options, as its caption, hold a macro's
/// parameter: a `#` and a digit or another `#`, in its options read as TeX
/// (see [`tex_holds_parameter`]).
fn holds_parameter(listing: &Listing) -> bool {
    let code_holds = |code: &str| {
        code.as_bytes()
            .windows(2)
            .any(|pair| pair[0] == b'#' && (pair[1].is_ascii_digit() || pair[1] == b'#'))
    };
    match listing {
        Listing::Inline(code) => code_holds(code),
        Listing::Block { code, options } => {
            code_holds(code) || options.as_deref().is_some_and(tex_holds_parameter)
        }
    }
}

/// Whether `tex` holds a macro's parameter as TeX reads it: not an escaped
/// `\#`, as a caption's `Step \#1`, nor one in a comment.
fn tex_holds_parameter(tex: &str) -> bool {
    let tex = without_comments(tex);
    let mut chars = tex.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '#' if chars
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || *c == '#') =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// The command placeholders in `source` are: [`MARK`] and the first of
/// none, `A`, `B`, …, `Z`, `AA`, … after it that makes no control word of
/// the source's, as `verbatim`'s are named. The letters after each
/// [`MARK`] in the source are read once, so this takes linear time.
fn mark(source: &str) -> String {
    let taken: std::collections::HashSet<&str> = source
        .match_indices(MARK)
        .map(|(at, _)| {
            let after = &source[at + MARK.len()..];
            let letters = after.find(|c: char| !tex_letter(c)).unwrap_or(after.len());
            &after[..letters]
        })
        .collect();
    (0usize..)
        .map(suffix)
        .find(|suffix| !taken.contains(suffix.as_str()))
        .map(|suffix| format!("{MARK}{suffix}"))
        .expect("more names than the source holds")
}

/// `n` in bijective base 26, as spreadsheet columns are named: none, `A`,
/// …, `Z`, `AA`, ….
fn suffix(mut n: usize) -> String {
    let mut letters = Vec::new();
    while n > 0 {
        n -= 1;
        letters.push(b'A' + (n % 26) as u8);
        n /= 26;
    }
    letters.reverse();
    String::from_utf8(letters).expect("ASCII letters")
}

/// The listings of a source, measured in order: what's to be passed by as
/// code before listings are cut (see `conditionals::keep_else`).
#[derive(Default)]
pub(super) struct Lengths {
    /// Whether a `lstlisting` with no `\end` was met: no later one has one
    /// either, so none is looked for, which would look to the source's end
    /// each time, as [`cut_into`] doesn't.
    unclosed: bool,
}

impl Lengths {
    /// How long the listing that `command`, `\lstinline` or `\begin`,
    /// starts is, from `after` it, if it starts one.
    pub(super) fn length(&mut self, command: &str, after: &str) -> Option<usize> {
        let listing = match command {
            "\\lstinline" => inline(after),
            "\\begin" if self.unclosed => None,
            "\\begin" => {
                let block = block(after);
                self.unclosed = block.is_none() && is_listing(after);
                block
            }
            _ => None,
        };
        listing.map(|(_, length)| length)
    }
}

/// `\lstinline`'s code, from what's after its name: a star, its options in
/// brackets, then the code between a delimiter and itself again, as for
/// `\verb`, or in braces, which may nest; and how long all that is. All of
/// it is on its line, and within [`MAX_INLINE`]. The options go, as no
/// inline code holds a language.
fn inline(after: &str) -> Option<(Listing, usize)> {
    let bound = (after.len().min(MAX_INLINE)..=after.len())
        .find(|end| after.is_char_boundary(*end))
        .unwrap_or(after.len());
    let after = &after[..bound];
    let after = &after[..after.find(['\n', '\r']).unwrap_or(after.len())];
    let star = usize::from(after.starts_with('*'));
    let options = match after[star..].strip_prefix('[') {
        Some(inside) => star + options_end(inside)? + 2,
        None => star,
    };
    let rest = &after[options..];
    if let Some(inside) = rest.strip_prefix('{') {
        let end = code_group_end(inside)?;
        let code = inside[..end].to_string();
        return Some((Listing::Inline(code), options + end + 2));
    }
    let delimiter = rest
        .chars()
        .next()
        .filter(|c| !c.is_alphabetic() && !c.is_whitespace())?;
    let opened = delimiter.len_utf8();
    let end = rest[opened..].find(delimiter)?;
    let code = rest[opened..opened + end].to_string();
    Some((Listing::Inline(code), options + opened + end + opened))
}

/// Whether `after`, what's after `\begin`, names a `lstlisting`.
fn is_listing(after: &str) -> bool {
    after
        .trim_start_matches([' ', '\t'])
        .starts_with("{lstlisting}")
}

/// A `lstlisting`'s code, from what's after `\begin`: past its options in
/// brackets right after its name, which may run over lines, and the rest of
/// that line, as `listings` reads it, the lines to its `\end`, without the
/// line break before that; and how long all that is.
fn block(after: &str) -> Option<(Listing, usize)> {
    if !is_listing(after) {
        return None;
    }
    const END: &str = "\\end{lstlisting}";
    let start = after.find("{lstlisting}")? + "{lstlisting}".len();
    let close = after[start..].find(END)?;
    let inside = &after[start..start + close];
    let spaces = inside.len() - inside.trim_start_matches([' ', '\t']).len();
    let bracketed = inside[spaces..].strip_prefix('[');
    let bracket = bracketed.and_then(options_end);
    let options = bracketed
        .zip(bracket)
        .map(|(text, end)| text[..end].to_string());
    let past_options = bracket.map_or(0, |bracket| spaces + bracket + 2);
    // The rest of that line goes, and its line break.
    let code = match inside[past_options..].find(['\n', '\r']) {
        Some(end) => {
            let code = &inside[past_options + end..];
            let code = code.strip_prefix("\r\n").unwrap_or(&code[1..]);
            ["\r\n", "\n", "\r"]
                .into_iter()
                .find_map(|line_end| code.strip_suffix(line_end))
                .unwrap_or(code)
        }
        None => "",
    };
    let code = code.to_string();
    Some((Listing::Block { code, options }, start + close + END.len()))
}

/// Where the `}` closing a group of code is in `text`, its braces counted
/// as they're written, as code's are: `None` if it isn't there.
fn code_group_end(text: &str) -> Option<usize> {
    let mut depth = 0_usize;
    for (at, c) in text.char_indices() {
        match c {
            '}' if depth == 0 => return Some(at),
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Where the `]` closing options in `text` is: they're TeX, so it's outside
/// their groups, and past escaped characters (`\}`) and comments. `None` if
/// it isn't there, or a group closes that isn't open.
fn options_end(text: &str) -> Option<usize> {
    let mut depth = 0_usize;
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '%' => {
                chars.find(|(_, c)| matches!(c, '\n' | '\r'));
            }
            ']' if depth == 0 => return Some(at),
            '{' => depth += 1,
            '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
    }
    None
}

/// The value of `key` among a listing's `options`, `key=value` separated by
/// commas outside groups and comments, a value's braces around all of it
/// gone. A key is read without its comments; a value is TeX as written, for
/// the parser to read its comments as it reads any.
pub(super) fn option(options: &str, key: &str) -> Option<String> {
    let mut depth = 0_usize;
    let mut start = 0;
    let mut parts = Vec::new();
    let mut chars = options.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '%' => {
                chars.find(|(_, c)| matches!(c, '\n' | '\r'));
            }
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&options[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&options[start..]);
    parts.into_iter().find_map(|part| {
        let (name, value) = split_at_equals(part)?;
        if without_comments(name).trim() != key {
            return None;
        }
        let value = value.trim();
        // Braces around all of it, not `{a}{b}`.
        let unbraced = value
            .strip_prefix('{')
            .and_then(|inside| inside.strip_suffix('}'))
            .filter(|inside| code_group_end(inside).is_none());
        Some(unbraced.unwrap_or(value).to_string())
    })
}

/// The language a listing's `options` say its code is in, as writers name
/// languages: lowercase, without its comments, and with a dialect in
/// brackets before it, as `[ANSI]C`'s, only where it makes another language
/// (see [`DIALECTS`]). `C++` is `cpp`. A name Typst wouldn't take whole is
/// none: it takes an identifier, and the rest would go into the code, as
/// `C++`'s `++`.
pub(super) fn language(options: &str) -> Option<String> {
    let language = without_comments(&option(options, "language")?).to_lowercase();
    let language = language.trim();
    let language = match language.strip_prefix('[') {
        Some(dialected) => {
            let (dialect, language) = dialected.split_once(']')?;
            let (dialect, language) = (dialect.trim(), language.trim());
            DIALECTS
                .iter()
                .find(|(of, base, _)| *of == dialect && *base == language)
                .map_or(language, |(_, _, other)| other)
        }
        None => language,
    };
    let language = if language == "c++" { "cpp" } else { language };
    is_typst_language(language).then(|| language.to_string())
}

/// `listings`'s dialects that make another language, lowercase: the dialect,
/// its language, and the language it makes, as writers name it. `cs`, not
/// `csharp`, which Typst doesn't know.
const DIALECTS: [(&str, &str, &str); 4] = [
    ("sharp", "c", "cs"),
    ("objective", "c", "objective-c"),
    ("objective", "caml", "ocaml"),
    ("visual", "basic", "vb"),
];

/// Whether Typst takes all of `name` as a raw block's language: whether it's
/// an identifier, here only of ASCII letters, digits, `_` and `-`.
fn is_typst_language(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// `part`, an option, split at its `=`, the first that's not in a comment.
fn split_at_equals(part: &str) -> Option<(&str, &str)> {
    let mut chars = part.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '%' => {
                chars.find(|(_, c)| matches!(c, '\n' | '\r'));
            }
            '=' => return Some((&part[..at], &part[at + 1..])),
            _ => {}
        }
    }
    None
}

/// `tex`, a key or a language, without its comments.
fn without_comments(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len());
    let mut chars = tex.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                out.extend(chars.next());
            }
            '%' => {
                chars.find(|c| matches!(c, '\n' | '\r'));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(code: &str) -> Listing {
        Listing::Inline(code.into())
    }

    fn block(code: &str, options: Option<&str>) -> Listing {
        Listing::Block {
            code: code.into(),
            options: options.map(Into::into),
        }
    }

    /// Each row: the source, what it's left as, and what's cut out.
    #[test]
    fn listings_are_cut_out_as_listings_reads_them() {
        let mark = |n: &str| format!("{MARK}{{{n}}}");
        let cases: Vec<(&str, String, Vec<Listing>)> = vec![
            (
                "\\begin{lstlisting}\n%x $y$ {\n\\end{verbatim}\n\\end{lstlisting}\nafter",
                format!("{}\nafter", mark("0")),
                vec![block("%x $y$ {\n\\end{verbatim}", None)],
            ),
            (
                "\\begin{lstlisting}[language=C, caption={a, [b]}] gone\nint x;\n\\end{lstlisting}",
                mark("0"),
                vec![block("int x;", Some("language=C, caption={a, [b]}"))],
            ),
            (
                "\\begin{lstlisting} [language=C,\n numbers=left] gone\nx\n\\end{lstlisting}",
                mark("0"),
                vec![block("x", Some("language=C,\n numbers=left"))],
            ),
            (
                "\\begin{lstlisting}[caption={\\}]}]\nx\n\\end{lstlisting}",
                mark("0"),
                vec![block("x", Some("caption={\\}]}"))],
            ),
            (
                "\\begin{lstlisting}[unclosed\nx\n\\end{lstlisting}",
                mark("0"),
                vec![block("x", None)],
            ),
            (
                "a \\lstinline|x{%| \\lstinline[language=C]{a{b}c} \\lstinline!y!",
                format!("a {} {} {}", mark("0"), mark("1"), mark("2")),
                vec![inline("x{%"), inline("a{b}c"), inline("y")],
            ),
            (
                "\\lstinline*|x| \\lstinline[caption={\\{}]|y| $a*b$",
                format!("{} {} $a*b$", mark("0"), mark("1")),
                vec![inline("x"), inline("y")],
            ),
        ];
        for (source, left, listings) in cases {
            let cut = cut(source);
            assert_eq!((cut.source, cut.listings), (left, listings), "{source}");
        }
    }

    /// In a comment, verbatim text or what the parser skips, unclosed, or
    /// across a line's end, it's left as it is.
    #[test]
    fn what_is_no_listing_is_left() {
        for source in [
            "% \\lstinline|x| \\begin{lstlisting}x\\end{lstlisting}",
            "\\verb|\\lstinline|x| \\begin{verbatim}\\lstinline|y|\\end{verbatim}",
            "\\iffalse \\lstinline|x| \\fi",
            "\\begin{lstlisting}[x] unclosed",
            "\\lstinline|x",
            "\\lstinline{x",
            "\\lstinline{x\ny}",
            "\\lstinline[x|y|",
            "\\begin{lstlistingx}a\\end{lstlistingx}",
            // A macro's parameter in it is the macro's.
            "\\lstinline{#1} \\lstinline|##|",
        ] {
            let cut = cut(source);
            assert_eq!(
                (cut.source.as_str(), cut.listings),
                (source, Vec::new()),
                "{source}"
            );
        }
        // Past the bound, it's none.
        let long = format!("\\lstinline{{{}}}", "x".repeat(MAX_INLINE));
        assert!(cut(&long).listings.is_empty());
    }

    /// Listings are measured as [`cut_into`] cuts them: after one with no
    /// `\end`, none is looked for.
    #[test]
    fn listings_are_measured_in_order() {
        let closed = "{lstlisting}\nx\n\\end{lstlisting}";
        let mut lengths = Lengths::default();
        assert_eq!(lengths.length("\\lstinline", "|a| b"), Some(3));
        assert_eq!(lengths.length("\\begin", closed), Some(closed.len()));
        assert_eq!(
            lengths.length("\\begin", "{verbatim}x\\end{verbatim}"),
            None
        );
        assert_eq!(lengths.length("\\begin", "{lstlisting}\nx"), None);
        assert_eq!(lengths.length("\\begin", closed), None);
        assert_eq!(lengths.length("\\lstinline", "|a| b"), Some(3));
    }

    /// A placeholder is a command the source doesn't hold.
    #[test]
    fn placeholders_are_named_apart_from_the_source() {
        let source = format!("{MARK} {MARK}A \\lstinline|x|");
        let named = cut(&source);
        assert_eq!(named.mark, format!("{MARK}B"));
        assert_eq!(named.source, format!("{MARK} {MARK}A {MARK}B{{0}}"));
        // A longer name it starts is no placeholder's.
        let source = format!("{MARK}AB \\lstinline|x|");
        assert_eq!(cut(&source).mark, MARK);
    }

    /// Many names a placeholder's would be take no more than reading them,
    /// and the next is the placeholder's.
    #[test]
    fn many_names_taken_take_linear_time() {
        let names: Vec<String> = (0..20_000)
            .map(|n| format!("{MARK}{}", suffix(n)))
            .collect();
        let source = format!("{} \\lstinline|x|", names.join(" "));
        let start = std::time::Instant::now();
        assert_eq!(cut(&source).mark, format!("{MARK}{}", suffix(20_000)));
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    /// Each row: options, and the language they say.
    #[test]
    fn languages_are_read_as_writers_name_them() {
        for (options, expected) in [
            ("language=Python", Some("python")),
            ("language = {C++} ", Some("cpp")),
            ("language={[Visual]C++}", Some("cpp")),
            // A dialect that makes another language is that language.
            ("language={[Sharp]C}", Some("cs")),
            ("language={[objective] c}", Some("objective-c")),
            ("language={[Objective]Caml}", Some("ocaml")),
            ("language={[Visual]Basic}", Some("vb")),
            ("language={[Sharp]Basic}", Some("basic")),
            ("language=[x86masm]Assembler", Some("assembler")),
            ("language={[ANSI]C}", Some("c")),
            ("language=Modula-2", Some("modula-2")),
            ("language={}", None),
            ("language={[Sharp]}", None),
            ("language=Py% note\nthon", Some("python")),
            // Typst wouldn't take all of these.
            ("language={C ++}", None),
            ("language=a`b", None),
            ("language=PL/I", None),
            ("language=C\\#", None),
            ("language=2c", None),
            ("caption=x", None),
        ] {
            assert_eq!(language(options).as_deref(), expected, "{options}");
        }
    }

    /// The language Typst gets is the listing's, all of it.
    #[test]
    fn languages_are_written_whole_to_typst() {
        use crate::converter::document::native::write::typst::{Options, Target, write};
        let tex_fallback = std::collections::HashSet::new();
        let options = Options {
            target: Target::TypstFile,
            fonts: Vec::new(),
            paper: "a4",
            base_dir: std::path::PathBuf::new(),
            tex_fallback: &tex_fallback,
        };
        for (language, fence) in [
            ("C++", "```cpp\nx\n```"),
            ("{[Sharp]C}", "```cs\nx\n```"),
            ("PL/I", "```\nx\n```"),
        ] {
            let source =
                format!("\\begin{{lstlisting}}[language={language}]\nx\n\\end{{lstlisting}}");
            let (doc, _) = super::super::read(&source);
            let typst = write(&doc, &options).source;
            assert!(typst.contains(fence), "{language}: {typst}");
        }
    }

    /// Each row: options, a key, and its value.
    #[test]
    fn options_are_read_by_key() {
        for (options, key, value) in [
            ("language=C, caption=x", "language", Some("C")),
            ("language = C ,caption={a, b}", "caption", Some("a, b")),
            ("caption={a}{b}", "caption", Some("{a}{b}")),
            ("caption={a\\,b}", "caption", Some("a\\,b")),
            ("numbers=left", "language", None),
            (
                "language=C,% x, caption=no\ncaption=y",
                "caption",
                Some("y"),
            ),
            (
                "caption={\\TeX% note\nbook}",
                "caption",
                Some("\\TeX% note\nbook"),
            ),
            ("caption % =no\n=y", "caption", Some("y")),
            ("caption=50\\%", "caption", Some("50\\%")),
        ] {
            assert_eq!(option(options, key).as_deref(), value, "{options}");
        }
    }
}
