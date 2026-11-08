//! What of a document's text the reader may lose without a word: the
//! words of its source's text that its reading doesn't hold. Run on the
//! bench's documents before a change goes to review:
//!
//! `cargo test --release --bin swift-shifter text_loss -- --ignored --nocapture`
//!
//! A word is three letters or more. One a command splits, as an accent's
//! (`Erd\H{o}s`), is looked for within the reading's words too.

use std::collections::HashSet;

/// Commands whose arguments are no text, and how many to pass by, each a
/// group or a command, after a star and arguments in brackets.
const NO_TEXT: &[(&str, usize)] = &[
    ("\\label", 1),
    ("\\ref", 1),
    ("\\eqref", 1),
    ("\\pageref", 1),
    ("\\autoref", 1),
    ("\\cref", 1),
    ("\\Cref", 1),
    ("\\cite", 1),
    ("\\citep", 1),
    ("\\citet", 1),
    ("\\nocite", 1),
    ("\\end", 1),
    ("\\includegraphics", 1),
    ("\\usepackage", 1),
    ("\\bibliographystyle", 1),
    ("\\bibliography", 1),
    ("\\vspace", 1),
    ("\\hspace", 1),
    ("\\color", 1),
    ("\\textcolor", 1),
    ("\\setcounter", 1),
    ("\\newtheorem", 1),
    ("\\setlength", 2),
    ("\\addcontentsline", 3),
    ("\\newcommand", 2),
    ("\\renewcommand", 2),
    ("\\providecommand", 2),
    ("\\DeclareMathOperator", 2),
    ("\\newenvironment", 3),
    ("\\renewenvironment", 3),
    // Its name, then its parameters and its text (see `no_text_length`).
    ("\\def", 1),
];

/// Environments whose arguments after their name are no text, and how many.
const NO_TEXT_ENVIRONMENTS: &[(&str, usize)] = &[
    ("tabular", 1),
    ("array", 1),
    ("longtable", 1),
    ("tabular*", 2),
    ("tabularx", 2),
    // The kind of statement it is, and the name it's restated by.
    ("restatable", 2),
];

/// A word of a source's text (see [`source_words`]).
struct Word<'a> {
    text: &'a str,
    /// Where it is in the source.
    at: usize,
    /// Whether a command splits it, right before or after it.
    split: bool,
}

/// `source` with its comments as spaces, each of the bytes it takes, so
/// that what's after them is where it was.
fn without_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut in_comment = false;
    let mut backslashes = 0;
    for c in source.chars() {
        if c == '\n' {
            in_comment = false;
        } else if c == '%' && backslashes % 2 == 0 {
            in_comment = true;
        }
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
        if in_comment {
            out.extend(std::iter::repeat_n(' ', c.len_utf8()));
        } else {
            out.push(c);
        }
    }
    out
}

/// The words of `source`'s text: of its body, and its titles, authors and
/// dates, without comments, commands' names and the arguments that are no
/// text (see [`NO_TEXT`]).
fn source_words(source: &str) -> Vec<Word<'_>> {
    let text = without_comments(source);
    let body_start = text.find("\\begin{document}").unwrap_or(0);
    let body_end = text[body_start..]
        .find("\\end{document}")
        .map_or(text.len(), |end| body_start + end);
    let mut spans = vec![(body_start, body_end)];
    for field in ["\\title", "\\author", "\\date"] {
        for (at, _) in text[..body_start].match_indices(field) {
            let start = at + field.len();
            spans.push((start, start + argument_length(&text[start..])));
        }
    }
    let mut words = Vec::new();
    for (start, end) in spans {
        let mut at = start;
        // Whether a word here runs on from a letter's command before it,
        // past spaces and braces.
        let mut after_letter = false;
        while let Some(c) = text[at..end].chars().next() {
            if c == '\\' {
                let command = &text[at..at + command_length(&text[at..])];
                at += command.len();
                at += no_text_length(command, &text[at..end]);
                after_letter = is_letter(command);
            } else if c.is_alphabetic() {
                let length = text[at..end]
                    .find(|c: char| !c.is_alphabetic())
                    .unwrap_or(end - at);
                let after = &text[at + length..];
                let runs_on = after.starts_with('\\') && is_letter(&after[..command_length(after)])
                    || after.starts_with('}') && after[1..].starts_with(char::is_alphabetic);
                if text[at..at + length].chars().count() >= 3 {
                    words.push(Word {
                        text: &source[at..at + length],
                        at,
                        // Or on from a group, as `\"{o}dinger` and `{G}rothendieck`.
                        split: after_letter || runs_on || text[..at].ends_with('}'),
                    });
                }
                at += length;
                after_letter = false;
            } else {
                after_letter &= matches!(c, ' ' | '{' | '}');
                at += c.len_utf8();
            }
        }
    }
    words
}

/// Whether `command` makes a letter of a word, or a word's letter of the
/// next: an accent's, as `\'` or `\H`, or one that's a letter, as `\ss`.
fn is_letter(command: &str) -> bool {
    matches!(
        &command[1..],
        "'" | "\""
            | "^"
            | "`"
            | "~"
            | "="
            | "."
            | "H"
            | "c"
            | "v"
            | "u"
            | "d"
            | "b"
            | "k"
            | "r"
            | "t"
            | "ss"
            | "l"
            | "L"
            | "o"
            | "O"
            | "aa"
            | "AA"
            | "ae"
            | "AE"
            | "oe"
            | "OE"
            | "i"
            | "j"
    )
}

/// How long the command `text` starts with is: a name of letters and `@`,
/// as in a package's, or a character.
fn command_length(text: &str) -> usize {
    let name = text[1..]
        .find(|c: char| !c.is_ascii_alphabetic() && c != '@')
        .unwrap_or(text.len() - 1);
    let symbol = text[1..].chars().next().map_or(0, char::len_utf8);
    1 + if name > 0 { name } else { symbol }
}

/// How much of `text`, after `command`, is its arguments that are no text.
fn no_text_length(command: &str, text: &str) -> usize {
    if command == "\\begin" {
        return environment_length(text);
    }
    let Some((_, count)) = NO_TEXT.iter().find(|(no_text, _)| *no_text == command) else {
        return 0;
    };
    let mut at = 0;
    for _ in 0..*count {
        at += argument_length(&text[at..]);
    }
    if command == "\\def" {
        // Its parameters, to its text's group.
        at += text[at..].find('{').unwrap_or(text.len() - at);
        at += argument_length(&text[at..]);
    }
    at
}

/// How much of `text`, after `\begin`, is no text: the environment's name,
/// its options in brackets, and a table's columns (see
/// [`NO_TEXT_ENVIRONMENTS`]).
fn environment_length(text: &str) -> usize {
    let name_length = argument_length(text);
    let name = text[..name_length].trim().trim_matches(['{', '}']);
    let mut at = name_length + brackets_length(&text[name_length..]);
    let found = NO_TEXT_ENVIRONMENTS
        .iter()
        .find(|(environment, _)| *environment == name);
    for _ in 0..found.map_or(0, |(_, count)| *count) {
        at += argument_length(&text[at..]);
    }
    at
}

/// How long `text` is past spaces, a star and arguments in brackets.
fn brackets_length(text: &str) -> usize {
    let mut at = text.len() - text.trim_start().len();
    if text[at..].starts_with('*') {
        at += 1;
    }
    loop {
        at += text[at..].len() - text[at..].trim_start().len();
        if !text[at..].starts_with('[') {
            return at;
        }
        match text[at..].find(']') {
            Some(close) => at += close + 1,
            None => return at,
        }
    }
}

/// How long `text` is to the end of the argument it starts with, past
/// [`brackets_length`]: a group, or a command; none if it starts with
/// neither.
fn argument_length(text: &str) -> usize {
    let at = brackets_length(text);
    if text[at..].starts_with('\\') {
        return at + command_length(&text[at..]);
    }
    if !text[at..].starts_with('{') {
        return at;
    }
    let mut depth = 0;
    let mut chars = text[at..].char_indices();
    while let Some((offset, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return at + offset + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

/// The strings of `debug`, a value's `Debug` form, as they are: what's
/// between its quotes, unescaped, and none of its types' names.
fn debug_strings(debug: &str) -> String {
    let mut out = String::new();
    let mut chars = debug.chars();
    while chars.by_ref().any(|c| c == '"') {
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('u') => {
                        let hex: String =
                            chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                        out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
                    }
                    Some(escaped) => out.push(escaped),
                    None => {}
                },
                c => out.push(c),
            }
        }
        out.push('\n');
    }
    out
}

/// The words of `source` that `reading`, all the text of a reading of it,
/// doesn't hold: each once, with where it first is.
fn lost_words<'a>(source: &'a str, reading: &str) -> Vec<Word<'a>> {
    let read: HashSet<&str> = reading
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    let mut seen = HashSet::new();
    let mut lost = Vec::new();
    for word in source_words(source) {
        // After an accent's command, its first letter is the accent's:
        // `M\"obius` is read `Möbius`.
        let first = word.text.chars().next().map_or(0, char::len_utf8);
        let kept = read.contains(word.text)
            || word.split && (reading.contains(word.text) || reading.contains(&word.text[first..]));
        if kept || !seen.insert(word.text) {
            continue;
        }
        lost.push(word);
    }
    lost
}

#[cfg(test)]
mod tests {
    use super::super::{inputs, read};
    use super::*;

    fn words(source: &str) -> Vec<&str> {
        source_words(source)
            .into_iter()
            .map(|word| word.text)
            .collect()
    }

    #[test]
    fn source_words_are_its_texts() {
        let source = "\\title{The Title}\\begin{document}Some text % a comment {\n\\label{key}\\cite[p.~3]{ref} \\textbf{bold} \\begin{figure}[h]end\\end{figure}\\end{document} after";
        assert_eq!(
            words(source),
            ["Some", "text", "bold", "end", "The", "Title"]
        );
        let source = "\\author{Ann Bee}\\author{Cid Dee}\\begin{document}\\def\\x@y#1{hidden}\\begin{tabular}{lrr}cell\\end{tabular}\\providecommand \\a@b [1]{gone}\\end{document}";
        assert_eq!(words(source), ["cell", "Ann", "Bee", "Cid", "Dee"]);
        // An end in a comment is none.
        let source = "\\begin{document}One % \\end{document}\nTwo 50\\% Three\\end{document}";
        assert_eq!(words(source), ["One", "Two", "Three"]);
    }

    #[test]
    fn lost_words_are_those_the_reading_lacks() {
        let source = "\\begin{document}Kept, Erd\\H{o}s, M\\\"obius, Ha\\ss ler, {TQFT}s, Schr\\\"{o}dinger and lost, lost, cat\\end{document}";
        let lost = lost_words(
            source,
            "Kept, Erdős, Möbius, Haßler, TQFTs, Schrödinger and concatenate",
        );
        let lost: Vec<&str> = lost.iter().map(|word| word.text).collect();
        assert_eq!(lost, ["lost", "cat"]);
    }

    #[test]
    fn debug_strings_are_its_text_alone() {
        let debug = format!("{:?}", vec![("Ann \"B\"\\", "e\u{301}\n")]);
        assert_eq!(debug_strings(&debug), "Ann \"B\"\\\ne\u{301}\n\n");
    }

    /// The bench's documents' words their readings lack, and their
    /// warnings.
    #[test]
    #[ignore = "a report on the bench's documents, run by hand"]
    fn text_loss() {
        let bench = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench");
        let manifest = std::fs::read_to_string(bench.join("documents.toml")).unwrap();
        let mut dir = String::new();
        for line in manifest.lines() {
            let value = line.split('"').nth(1).unwrap_or("");
            if line.starts_with("dir =") {
                dir = value.to_string();
            } else if line.starts_with("main =") {
                let folder = bench.join("documents").join(&dir);
                let bytes = std::fs::read(folder.join(value)).unwrap();
                let inlined = inputs::inlined(&String::from_utf8_lossy(&bytes), &folder);
                let (doc, mut warnings) = read(&inlined.source);
                warnings.extend(inlined.warnings());
                let source = &inlined.source;
                let lost = lost_words(source, &debug_strings(&format!("{doc:?}")));
                let warned: usize = warnings.iter().map(|warning| warning.count).sum();
                println!("{dir}: {} lost, {warned} warned", lost.len());
                for word in lost.iter().take(10) {
                    let start = source.floor_char_boundary(word.at.saturating_sub(30));
                    let end = source.ceil_char_boundary((word.at + 30).min(source.len()));
                    println!("  {}: {:?}", word.text, &source[start..end]);
                }
            }
        }
    }
}
