//! The files a document reads in, read into its source before it's parsed:
//! what `\input`, `\include` and the `import` package's commands name, found
//! as LaTeX finds them (see [`paths`]) where pandoc would see the commands
//! (see [`files`]), each file's own read in with the folders it looks in. An
//! `\includegraphics` names its image as found, from the document's folder,
//! as the image is read from there.
//!
//! Files are read wherever their names lead, outside the document's folder
//! too, as LaTeX and pandoc read them. Only an `\includegraphics` is
//! renamed, and only to a file inside the folder (see
//! [`Reading::is_inside`]). A name not found as written is found with the
//! macros with no arguments defined before it expanded
//! (`\input{\chapterfile}`). Past [`MAX_DEPTH`], [`MAX_FILES`] or what the
//! source may grow to, a command is left unread. Each such command, and
//! one naming a file that isn't there, shows nothing, and is counted for a
//! warning. `TEXINPUTS` isn't looked in.
//!
//! Known limit: a chapter read in by a macro with arguments
//! (`\newcommand{\load}[1]{\input{chapters/#1}}` then `\load{one}`) is
//! lost: the reader finds files before macros with arguments are expanded.
//! Reading such calls before parsing was tried and left out (#73 item 22c,
//! four review rounds). A call read there ran where LaTeX wouldn't run it,
//! as in an optional argument's default or a definition's body, or took a
//! definition `\begingroup` makes local for one that holds after it, and so
//! would change text the reader reads correctly today. Writing the `\input`
//! out where the macro is called (`\input{chapters/one}`) reads it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::files;
use super::paths::{self, Scope};
use super::verbatim::{self, tex_letter};
use crate::converter::document::native::Warning;

/// How deep files may read others in: a file reading itself ends there.
const MAX_DEPTH: usize = 64;

/// How many files may be read in, the same one each time it is: far more
/// than a book reads in, as pandoc's route, now gone, allowed (#170).
const MAX_FILES: usize = 1000;

/// What the source may grow by for each of its own bytes, and at least: two
/// files each reading the next twice would double it at each level.
const GROWTH_PER_BYTE: usize = 16;
const MIN_GROWTH: usize = 16 << 20;

/// A document's source with the files it reads in it.
pub(super) struct Inlined {
    pub(super) source: String,
    /// The commands naming a file that wasn't found.
    missing: usize,
    /// Those left unread past the limits.
    cut_short: usize,
}

impl Inlined {
    /// What was left unread, as warnings.
    pub(super) fn warnings(&self) -> Vec<Warning> {
        let warning = |count, one, many| Warning {
            one,
            many,
            count,
            line: None,
            detail: None,
        };
        [
            warning(
                self.missing,
                "file the document reads wasn't found",
                "files the document reads weren't found",
            ),
            warning(
                self.cut_short,
                "file read in too often or too deep wasn't read",
                "files read in too often or too deep weren't read",
            ),
        ]
        .into_iter()
        .filter(|warning| warning.count > 0)
        .collect()
    }
}

/// `source`, a document in `dir`, with the files it reads in it.
pub(super) fn inlined(source: &str, dir: &Path) -> Inlined {
    let mut reading = Reading {
        dir: dir.to_path_buf(),
        canonical: dir.canonicalize().ok(),
        graphics: Vec::new(),
        macros: HashMap::new(),
        files: 0,
        growth_left: source.len().saturating_mul(GROWTH_PER_BYTE).max(MIN_GROWTH),
        inlined: Inlined {
            source: String::new(),
            missing: 0,
            cut_short: 0,
        },
    };
    let source = reading.read_in(source, &Scope::default(), 0);
    Inlined {
        source,
        ..reading.inlined
    }
}

/// Whether `text` ends in a control word, which letters would run on from.
fn ends_in_control_word(text: &str) -> bool {
    let before_letters = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let backslashes = before_letters.len() - before_letters.trim_end_matches('\\').len();
    before_letters.len() < text.len() && backslashes % 2 == 1
}

/// What a command in a document sets that later files' names are found
/// with (see [`settings`]).
#[derive(Debug, PartialEq)]
enum Setting {
    /// The folders a `\graphicspath` names.
    GraphicsPath(Vec<String>),
    /// A macro with no arguments, defined, and what it expands to; only
    /// where it isn't defined yet, for `\providecommand`.
    Macro {
        name: String,
        body: String,
        provided: bool,
    },
    /// A macro defined with arguments, which names aren't expanded with.
    Unmacro(String),
}

/// Each [`Setting`] in `source`, read as [`files::named`] reads the
/// commands naming files: not in a comment or verbatim text, nor in a
/// branch of a conditional that isn't read, and not in a listing either,
/// which the reader cuts out too. Where the command ends, and what it sets:
/// a `\graphicspath` with an argument of groups, and a `\newcommand`,
/// `\renewcommand`, `\providecommand` or `\def`.
fn settings(source: &str) -> Vec<(usize, Setting)> {
    let cut = verbatim::cut(source);
    let text = cut.source.as_str();
    let mut found = Vec::new();
    let mut at = 0;
    // As in `files::named`: whether each conditional open is an `\iftrue`.
    let mut open: Vec<bool> = Vec::new();
    let mut unclosed = Vec::new();
    while let Some((start, name)) = verbatim::next_command(&text[at..], tex_letter) {
        at += start + name.len();
        let rest = &text[at..];
        match name {
            "\\iffalse" => {
                let (length, at_else) = files::branch(rest, true);
                at += length;
                if at_else {
                    open.push(false);
                }
            }
            "\\else" if open.last() == Some(&true) => {
                at += files::branch(rest, false).0;
                open.pop();
            }
            "\\fi" => {
                open.pop();
            }
            _ if verbatim::CONDITIONALS.contains(&name) => open.push(name == "\\iftrue"),
            "\\verb" => at += verbatim::inline(rest).map_or(0, |(_, length)| length),
            "\\begin" => {
                at += listing_length(rest)
                    .or_else(|| verbatim::block(rest, &mut unclosed).map(|(_, length)| length))
                    .unwrap_or(0);
            }
            "\\graphicspath" => {
                if let Some((folders, length)) = graphics_argument(rest) {
                    at += length;
                    found.push((cut.in_source(at), Setting::GraphicsPath(folders)));
                }
            }
            "\\newcommand" | "\\renewcommand" | "\\providecommand" | "\\def" => {
                if let Some((setting, length)) = definition(name, rest) {
                    at += length;
                    found.push((cut.in_source(at), setting));
                }
            }
            _ => {}
        }
    }
    found
}

/// The macro the definition `text` starts with, after its `command`,
/// defines, and how long it is; `None` where it can't be read so.
fn definition(command: &str, text: &str) -> Option<(Setting, usize)> {
    let def = command == "\\def";
    let mut rest = if def {
        text
    } else {
        text.strip_prefix('*').unwrap_or(text)
    };
    rest = past_spaces(rest);
    // `\name`, or `{\name}` but for `\def`.
    let braced = !def && rest.starts_with('{');
    if braced {
        rest = past_spaces(&rest[1..]);
    }
    let after_slash = rest.strip_prefix('\\')?;
    let letters = after_slash.len() - after_slash.trim_start_matches(tex_letter).len();
    let name = format!(
        "\\{}",
        after_slash.get(..letters).filter(|n| !n.is_empty())?
    );
    rest = &after_slash[letters..];
    if braced {
        rest = past_spaces(rest).strip_prefix('}')?;
    }
    rest = past_spaces(rest);
    // Arguments: `[1]` after a `\newcommand`'s name, `#1` after a `\def`'s.
    if rest.starts_with(if def { '#' } else { '[' }) {
        return Some((Setting::Unmacro(name), text.len() - rest.len()));
    }
    let body = rest.strip_prefix('{')?;
    let end = group_end(body)?;
    let setting = Setting::Macro {
        name,
        body: body[..end].to_string(),
        provided: command == "\\providecommand",
    };
    Some((setting, text.len() - body.len() + end + 1))
}

/// Where the group `text` is the inside of ends: its `}`, past groups in it
/// and escaped braces.
fn group_end(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut chars = text.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '{' => depth += 1,
            '}' if depth == 0 => return Some(at),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// How many macros expanding a name expands, those they expand to too.
const MAX_EXPANSIONS: usize = 8;

/// `name`, a file's name a command gives, with the macros in it expanded
/// as `macros` define them; `None` where it has none, or one is left that
/// isn't defined so.
fn expanded(name: &str, macros: &HashMap<String, String>) -> Option<String> {
    if !name.contains('\\') {
        return None;
    }
    let mut name = name.to_string();
    for _ in 0..MAX_EXPANSIONS {
        let Some(start) = name.find('\\') else {
            return Some(name);
        };
        let after = &name[start + 1..];
        let letters = after.len() - after.trim_start_matches(tex_letter).len();
        let body = macros.get(&name[start..start + 1 + letters])?;
        // TeX passes spaces after a control word.
        let rest = after[letters..].trim_start_matches(' ');
        name = format!("{}{body}{rest}", &name[..start]);
    }
    None
}

/// How long the `lstlisting` environment `text` starts with is, after its
/// `\begin`, to its end; `None` where it's no listing.
fn listing_length(text: &str) -> Option<usize> {
    const END: &str = "\\end{lstlisting}";
    text.strip_prefix("{lstlisting}")?;
    Some(text.find(END).map_or(text.len(), |end| end + END.len()))
}

/// How long the comment `text` starts with is, to its line's end.
fn comment_length(text: &str) -> usize {
    text.find(['\n', '\r']).unwrap_or(text.len())
}

/// `text` past spaces, line ends and comments, as TeX passes them before
/// an argument.
fn past_spaces(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        if !text.starts_with('%') {
            return text;
        }
        text = &text[comment_length(text)..];
    }
}

/// The folders `\graphicspath`'s argument, at `text`'s start, names, as
/// written, as `figures/` in `{{figures/}}`, and how long it is; `None`
/// where it's no argument of groups, as one a macro makes.
fn graphics_argument(text: &str) -> Option<(Vec<String>, usize)> {
    let mut inside = past_spaces(text).strip_prefix('{')?;
    let mut folders = Vec::new();
    while let Some(group) = past_spaces(inside).strip_prefix('{') {
        // Not past a `{`, so that each unclosed one is looked past only to
        // the next brace.
        let end = group
            .find(['{', '}'])
            .filter(|&end| group[end..].starts_with('}'))?;
        folders.push(group[..end].to_string());
        inside = &group[end + 1..];
    }
    let after = past_spaces(inside).strip_prefix('}')?;
    Some((folders, text.len() - after.len()))
}

/// Files being read in (see [`inlined`]).
struct Reading {
    dir: PathBuf,
    /// The document's folder, links resolved, which images are renamed
    /// in (see [`Reading::is_inside`]); `None` where it can't be resolved,
    /// and none are.
    canonical: Option<PathBuf>,
    /// The folders the last `\graphicspath` read names, which an image not
    /// found as named is looked for in (see [`Reading::image_file`]). One in
    /// a group, an environment or a file `\import` reads isn't undone at its
    /// end, as LaTeX undoes it, and one a macro makes is passed by: only an
    /// image not found otherwise is looked for in them.
    graphics: Vec<String>,
    /// The macros with no arguments defined before, which a file's name
    /// that isn't found as written is expanded with (`\input{\chapter}`).
    /// One defined in a group isn't undone at its end, as `\graphicspath`
    /// isn't.
    macros: HashMap<String, String>,
    /// How many have been read.
    files: usize,
    /// What the source may still grow by.
    growth_left: usize,
    inlined: Inlined,
}

impl Reading {
    /// What `setting` sets, for the files named after it: each
    /// `\graphicspath` in place of those before.
    fn apply(&mut self, setting: Setting) {
        match setting {
            Setting::GraphicsPath(folders) => self.graphics = folders,
            Setting::Macro {
                name,
                body,
                provided,
            } => {
                if !(provided && self.macros.contains_key(&name)) {
                    self.macros.insert(name, body);
                }
            }
            Setting::Unmacro(name) => {
                self.macros.remove(&name);
            }
        }
    }

    /// `text`, a file read in `depth` files deep, looking in `scope`'s
    /// folders, with the files it reads in it.
    fn read_in(&mut self, text: &str, scope: &Scope, depth: usize) -> String {
        let dir = self.dir.clone();
        let exists = |path: &str| dir.join(path).is_file();
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        let mut settings = settings(text).into_iter().peekable();
        for named in files::named(text) {
            while let Some((_, setting)) =
                settings.next_if(|(set_at, _)| *set_at <= named.range.start)
            {
                self.apply(setting);
            }
            let args: Vec<&str> = named.args.iter().map(|arg| &text[arg.clone()]).collect();
            if named.command == "\\includegraphics" {
                // Named as found, if it's in the folder and can be written
                // as it is.
                let path = self
                    .image_file(&args, scope)
                    .filter(|path| path != args[0] && self.is_inside(path))
                    .filter(|path| !path.contains(['\\', '{', '}', '%', '#']));
                if let Some(path) = path {
                    out.push_str(&text[at..named.args[0].start]);
                    out.push_str(&path);
                    at = named.args[0].end;
                }
                continue;
            }
            // As written, or else with the macros before it expanded
            // (`\input{\chapterfile}`).
            let found = paths::find(named.command, &args, scope, exists).or_else(|| {
                let expanded = args
                    .iter()
                    .map(|arg| expanded(arg, &self.macros).unwrap_or_else(|| arg.to_string()))
                    .collect::<Vec<_>>();
                let expanded: Vec<&str> = expanded.iter().map(String::as_str).collect();
                (expanded != args)
                    .then(|| paths::find(named.command, &expanded, scope, exists))
                    .flatten()
            });
            let Some(found) = found else {
                // A name a macro makes that isn't defined so isn't found.
                self.inlined.missing += 1;
                continue;
            };
            // Wherever it is, as LaTeX reads it: outside the document's
            // folder too (`../shared`).
            let path = paths::tex_file(&found.path, exists);
            let Some(read) = self.read(&path, depth) else {
                continue;
            };
            let read = self.read_in(&read, &found.scope, depth + 1);
            out.push_str(&text[at..named.range.start]);
            // Its first letters don't run on from a control word before
            // the command: the space is one TeX doesn't see.
            if ends_in_control_word(&out) && read.starts_with(|c: char| c.is_ascii_alphabetic()) {
                out.push(' ');
            }
            out.push_str(&read);
            // A file's last line ends where the file does, and a comment
            // on it with it.
            if !read.ends_with('\n') {
                out.push('\n');
            }
            at = named.range.end;
        }
        // For what's read after it, as a file read in isn't a group.
        for (_, setting) in settings {
            self.apply(setting);
        }
        out.push_str(&text[at..]);
        out
    }

    /// The text of the file at `path`, from the document's folder, to be read
    /// `depth` files deep; `None` past the limits, or where it can't be
    /// read, which is counted as missing.
    fn read(&mut self, path: &str, depth: usize) -> Option<String> {
        if depth >= MAX_DEPTH || self.files >= MAX_FILES {
            self.inlined.cut_short += 1;
            return None;
        }
        let file = self.dir.join(path);
        // Its size first: one too big isn't read at all.
        let size = std::fs::metadata(&file).map(|metadata| metadata.len());
        if size.is_ok_and(|size| size > self.growth_left as u64) {
            self.inlined.cut_short += 1;
            return None;
        }
        let Ok(bytes) = std::fs::read(&file) else {
            self.inlined.missing += 1;
            return None;
        };
        // It may have grown since.
        let Some(growth_left) = self.growth_left.checked_sub(bytes.len()) else {
            self.inlined.cut_short += 1;
            return None;
        };
        self.growth_left = growth_left;
        self.files += 1;
        // As the document itself is read: a stray byte doesn't fail it.
        let text = String::from_utf8_lossy(&bytes);
        Some(text.trim_start_matches('\u{feff}').to_string())
    }

    /// The file an `\\includegraphics` with `args`, in a file looking in
    /// `scope`'s folders, shows, from the document's folder: the one that
    /// `paths::find`, trying names as graphicx does, takes, with the
    /// extension it tried.
    fn image_file(&self, args: &[&str], scope: &Scope) -> Option<String> {
        let taken = std::cell::RefCell::new(None);
        let exists = |path: &str| {
            let exists = self.dir.join(path).is_file();
            if exists {
                *taken.borrow_mut() = Some(path.to_string());
            }
            exists
        };
        // As main finds it first, so that an image found so is as it was;
        // in the folders `\graphicspath` names only where it isn't.
        if paths::find("\\includegraphics", args, scope, exists).is_none() {
            paths::find_in_graphics(args.first()?, &self.graphics, exists)?;
        }
        taken.into_inner()
    }

    /// Whether `path`, from the document's folder, is a file in it, where a
    /// link leads too.
    fn is_inside(&self, path: &str) -> bool {
        let Some(canonical) = &self.canonical else {
            return false;
        };
        self.dir
            .join(path)
            .canonicalize()
            .is_ok_and(|path| path.starts_with(canonical))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{read, read_in};
    use crate::converter::document::native::Warning;
    use crate::converter::document::native::ast::{Block, Inline};
    use std::path::Path;

    /// Files to write, each a path and its text.
    type Files<'a> = &'a [(&'a str, &'a [u8])];

    /// `main` read in a folder holding `files`.
    fn read_with(files: Files, main: &str) -> (Vec<Block>, Vec<Warning>) {
        let dir = tempfile::tempdir().unwrap();
        write_files(dir.path(), files);
        let (doc, warnings) = read_in(main, dir.path());
        (doc.blocks, warnings)
    }

    fn write_files(dir: &Path, files: Files) {
        for (path, text) in files {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    /// The counts of the warnings, by what they say of one.
    fn counts(warnings: &[Warning]) -> Vec<(&'static str, usize)> {
        warnings.iter().map(|w| (w.one, w.count)).collect()
    }

    /// Each row: the files, a document, and what it reads as, written out.
    #[test]
    fn files_are_read_in() {
        let cases: [(Files, &str, &str); 10] = [
            (&[("a.tex", b"A")], "x \\input{a} y", "x A\n y"),
            (&[("a.tex", b"A")], "\\input{a.tex}", "A"),
            (&[("a.tex", b"A")], "x\\include{a}y", "xA\ny"),
            // A file's own, from its folder.
            (
                &[("a.tex", b"\\input{b}"), ("b.tex", b"B")],
                "\\input{a}",
                "B",
            ),
            // Nor does it run on from a control word before it.
            (&[("a.tex", b"world")], "\\LaTeX\\input{a}", "\\LaTeX world"),
            // A comment on its last line ends with it.
            (&[("a.tex", b"A%c")], "\\input{a}B", "AB"),
            // The `import` package's folders.
            (
                &[("ch/one.tex", b"\\input{two}"), ("ch/two.tex", b"T")],
                "\\import{ch/}{one}",
                "T",
            ),
            (
                &[
                    ("ch/one.tex", b"\\subimport{sub/}{deep}"),
                    ("ch/sub/deep.tex", b"D"),
                ],
                "\\import{ch}{one}",
                "D",
            ),
            // Not where pandoc wouldn't see it.
            (
                &[("a.tex", b"A")],
                "%\\input{a}\nx \\verb|\\input{a}| \\iffalse\\input{a}\\fi",
                "%\nx \\verb|\\input{a}| ",
            ),
            // A stray byte doesn't fail it.
            (&[("a.tex", b"A\xff")], "\\input{a}", "A\u{fffd}"),
        ];
        for (files, main, written) in cases {
            let (blocks, warnings) = read_with(files, main);
            assert_eq!(blocks, read(written).0.blocks, "{main}");
            assert!(warnings.is_empty(), "{main}: {warnings:?}");
        }
    }

    /// An image is named as found, from the document's folder.
    #[test]
    fn images_are_named_from_the_document() {
        let files: Files = &[
            ("ch/one.tex", b"\\subimport{sub/}{deep}"),
            ("ch/sub/deep.tex", b"\\includegraphics{fig}"),
            ("ch/sub/fig.png", b"png"),
            ("ch/sub/fig.svg", b"svg"),
            ("top.png", b"png"),
        ];
        let (blocks, _) = read_with(files, "\\import{ch/}{one} \\includegraphics{top}");
        let urls: Vec<&str> = match &blocks[..] {
            [Block::Paragraph(inlines)] => inlines
                .iter()
                .filter_map(|inline| match inline {
                    Inline::Image { url, .. } => Some(url.as_str()),
                    _ => None,
                })
                .collect(),
            _ => panic!("{blocks:?}"),
        };
        assert_eq!(urls, ["ch/sub/fig.png", "top.png"]);
    }

    /// Each row: a document, with the files below beside it, and what its
    /// image is named: as main names it where main finds it, whatever
    /// `\graphicspath` names; else in the folders one before it names.
    #[test]
    fn images_not_found_are_looked_for_in_the_graphics_path() {
        let files: Files = &[
            ("plot.png", b"png"),
            ("other/plot.pdf", b"pdf"),
            ("other/only.png", b"png"),
            ("pics/only.jpg", b"jpg"),
            ("preamble.tex", b"\\graphicspath{{other/}}"),
            ("ch/body.tex", b"\\graphicspath{{other/}}"),
            ("ch/fig.tex", b"\\includegraphics{fig}"),
            ("ch/fig.png", b"png"),
            ("other/fig.png", b"png"),
        ];
        let set = "\\graphicspath{{other/}}";
        for (main, url) in [
            // Found as named: as main has it.
            (format!("{set}\\includegraphics{{plot}}"), "plot.png"),
            // A conditional of the document's own, a macro's folders and a
            // file `\import` reads change none of it.
            (
                format!("\\newif\\ifdraft\\draftfalse\\ifdraft{set}\\fi\\includegraphics{{plot}}"),
                "plot.png",
            ),
            (
                "\\def\\dirs{{other/}}\\graphicspath{\\dirs}\\includegraphics{plot}".to_string(),
                "plot.png",
            ),
            (
                format!("{set}\\graphicspath{{\\dirs}}\\includegraphics{{plot}}"),
                "plot.png",
            ),
            (
                "\\import{ch/}{body}\\includegraphics{plot}".to_string(),
                "plot.png",
            ),
            // Found in a folder `\import` looks in first, as main has it.
            (format!("{set}\\import{{ch/}}{{fig}}"), "ch/fig.png"),
            // Not found as named: in its folders.
            (format!("{set}\\includegraphics{{only}}"), "other/only.png"),
            (
                "\\input{preamble}\n\\includegraphics{only}".to_string(),
                "other/only.png",
            ),
            (
                "\\import{ch/}{body}\\includegraphics{only}".to_string(),
                "other/only.png",
            ),
            // The last one's, in place of those before.
            (
                format!("{set}\\graphicspath{{{{pics/}}}}\\includegraphics{{only}}"),
                "pics/only.jpg",
            ),
            (
                format!("{set}\\graphicspath{{}}\\includegraphics{{only}}"),
                "only",
            ),
            // An `\iffalse`'s `\else`, which is read.
            (
                format!("\\iffalse x\\else{set}\\fi\\includegraphics{{only}}"),
                "other/only.png",
            ),
            // Not one after it, nor one LaTeX doesn't read, nor a name from
            // the root, nor one in none.
            (format!("\\includegraphics{{only}}{set}"), "only"),
            (format!("% {set}\n\\includegraphics{{only}}"), "only"),
            (format!("\\verb|{set}|\\includegraphics{{only}}"), "only"),
            (
                format!("\\iffalse{set}\\fi\\includegraphics{{only}}"),
                "only",
            ),
            (
                format!("\\iftrue x\\else{set}\\fi\\includegraphics{{only}}"),
                "only",
            ),
            (
                format!("\\begin{{lstlisting}}{set}\\end{{lstlisting}}\\includegraphics{{only}}"),
                "only",
            ),
            (format!("{set}\\includegraphics{{/only}}"), "/only"),
            (format!("{set}\\includegraphics{{none}}"), "none"),
        ] {
            let (mut blocks, _) = read_with(files, &main);
            let mut urls = Vec::new();
            super::super::walk::for_each_inlines(&mut blocks, &mut |inlines| {
                for inline in inlines.iter() {
                    if let Inline::Image { url, .. } = inline {
                        urls.push(url.clone());
                    }
                }
            });
            assert_eq!(urls, [url], "{main}");
        }
    }

    /// A file that isn't there shows nothing, with a warning.
    #[test]
    fn a_missing_file_is_a_warning() {
        let (blocks, warnings) = read_with(&[], "\\input{nope}{x} \\import{a/}{b}{y}");
        assert_eq!(blocks, read("x y").0.blocks);
        assert_eq!(
            counts(&warnings),
            [("file the document reads wasn't found", 2)]
        );
        // Its name past a space isn't its text either.
        let (blocks, _) = read_with(&[], "a \\input {nope} b");
        assert_eq!(blocks, read_with(&[], "a \\input{nope} b").0);
        assert!(!format!("{blocks:?}").contains("nope"));
    }

    /// Files outside the document's folder are read, as LaTeX reads them,
    /// whether a relative name, an absolute one or a link leads there.
    #[test]
    fn files_outside_the_folder_are_read() {
        let root = tempfile::tempdir().unwrap();
        write_files(
            root.path(),
            &[
                ("shared.tex", b"SHARED"),
                // A name with a dot isn't taken for one with an extension
                // (#192).
                ("shared.v1.tex", b"VERSIONED"),
                ("absolute.tex", b"ABSOLUTE"),
                ("doc/in.tex", b"INSIDE"),
            ],
        );
        let doc = root.path().join("doc");
        let absolute = root.path().join("absolute.tex");
        let main = format!(
            "\\input{{in}} \\input{{../shared.tex}} \\input{{../shared.v1}} \\input{{{}}}",
            absolute.display()
        );
        #[cfg(unix)]
        let main = {
            std::os::unix::fs::symlink(root.path().join("shared.tex"), doc.join("link.tex"))
                .unwrap();
            main + " \\input{link}"
        };
        let (read, warnings) = read_in(&main, &doc);
        let text = format!("{:?}", read.blocks);
        for expected in ["INSIDE", "SHARED", "VERSIONED", "ABSOLUTE"] {
            assert!(text.contains(expected), "{expected}: {text}");
        }
        #[cfg(unix)]
        assert_eq!(text.matches("SHARED").count(), 2, "{text}");
        assert_eq!(warnings, []);
    }

    /// A name a macro with no arguments makes is found with it expanded,
    /// as defined before, in the document or a file read in before.
    #[test]
    fn names_macros_make_are_expanded() {
        let files: Files = &[
            ("chapter.tex", b"CHAPTER"),
            ("parts/one.tex", b"ONE"),
            ("defs.tex", b"\\def\\partsdir{parts}"),
            ("other.tex", b"OTHER"),
        ];
        let read = |main: &str| {
            let (blocks, warnings) = read_with(files, main);
            (format!("{blocks:?}"), counts(&warnings))
        };
        for main in [
            "\\newcommand{\\chapterfile}{chapter}\\input{\\chapterfile}",
            "\\newcommand*\\chapterfile{chapter}\\input{\\chapterfile}",
            "\\def\\chapterfile{chapter}\\include{\\chapterfile}",
            // Renewed, and one `\providecommand` doesn't change.
            "\\def\\c{other}\\renewcommand{\\c}{chapter}\\providecommand{\\c}{other}\\input{\\c}",
        ] {
            let (text, warnings) = read(main);
            assert!(
                text.contains("CHAPTER") && !text.contains("OTHER"),
                "{main}: {text}"
            );
            assert_eq!(warnings, [], "{main}");
        }
        // In a file read in before, with more after it.
        let (text, _) = read("\\input{defs}\\input{\\partsdir/one}");
        assert!(text.contains("ONE"), "{text}");
        let (text, _) = read("\\input{defs}\\input{\\partsdir /one}");
        assert!(text.contains("ONE"), "{text}");
        // Not one defined after, with arguments, in a comment, or itself.
        let missing = [("file the document reads wasn't found", 1)];
        for main in [
            "\\input{\\chapterfile}\\def\\chapterfile{chapter}",
            "\\newcommand{\\chapterfile}[1]{chapter}\\input{\\chapterfile}",
            "\\def\\chapterfile#1{chapter}\\input{\\chapterfile}",
            "\\def\\chapterfile{chapter}\\def\\chapterfile#1{x}\\input{\\chapterfile}",
            "% \\def\\chapterfile{chapter}\n\\input{\\chapterfile}",
            "\\def\\loop{\\loop}\\input{\\loop}",
        ] {
            let (text, warnings) = read(main);
            assert!(!text.contains("CHAPTER"), "{main}: {text}");
            assert_eq!(warnings, missing, "{main}");
        }
    }

    /// Files reading themselves, or each the next twice over, end, with a
    /// warning.
    #[test]
    fn files_reading_in_without_end_are_cut_short() {
        let (_, warnings) = read_with(&[("a.tex", b"a\\input{a}")], "\\input{a}");
        assert_eq!(
            counts(&warnings),
            [("file read in too often or too deep wasn't read", 1)]
        );
        let mut files = Vec::new();
        for level in 0..30 {
            let next = format!("\\input{{f{}}}", level + 1);
            files.push((
                format!("f{level}.tex"),
                format!("{next}{next}").into_bytes(),
            ));
        }
        files.push(("f30.tex".to_string(), b"x".to_vec()));
        let files: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_slice()))
            .collect();
        let start = std::time::Instant::now();
        let (_, warnings) = read_with(&files, "\\input{f0}");
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].one.contains("too often"));
    }
}
