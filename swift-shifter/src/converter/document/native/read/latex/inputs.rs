//! The files a document reads in, read into its source before it's parsed:
//! what `\input`, `\include` and the `import` package's commands name, found
//! as LaTeX finds them (see [`paths`]) where pandoc would see the commands
//! (see [`files`]), each file's own read in with the folders it looks in. An
//! `\includegraphics` names its image as found, from the document's folder,
//! as the image is read from there.
//!
//! Only files in the document's folder are read, where a link leads too: a
//! converted document isn't to carry the rest of the disk with it, as
//! `\input{/etc/passwd}` would. Past [`MAX_DEPTH`], [`MAX_FILES`] or what
//! the source may grow to, a command is left unread. Each such command, and
//! one naming a file that isn't there, shows nothing, and is counted for a
//! warning.

use std::path::{Path, PathBuf};

use super::files;
use super::paths::{self, Scope};
use crate::converter::document::native::Warning;

/// How deep files may read others in: a file reading itself ends there.
const MAX_DEPTH: usize = 64;

/// How many files may be read in, the same one each time it is: as many as
/// a document is staged with for pandoc (see `stage::MAX_FILES`).
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
    /// Those naming one outside the document's folder.
    outside: usize,
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
        };
        [
            warning(
                self.missing,
                "file the document reads wasn't found",
                "files the document reads weren't found",
            ),
            warning(
                self.outside,
                "file outside the document's folder wasn't read",
                "files outside the document's folder weren't read",
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
        files: 0,
        growth_left: source.len().saturating_mul(GROWTH_PER_BYTE).max(MIN_GROWTH),
        inlined: Inlined {
            source: String::new(),
            missing: 0,
            outside: 0,
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

/// Files being read in (see [`inlined`]).
struct Reading {
    dir: PathBuf,
    /// The document's folder, links resolved, which files read are in;
    /// `None` where it can't be resolved, and none are read.
    canonical: Option<PathBuf>,
    /// How many have been read.
    files: usize,
    /// What the source may still grow by.
    growth_left: usize,
    inlined: Inlined,
}

impl Reading {
    /// `text`, a file read in `depth` files deep, looking in `scope`'s
    /// folders, with the files it reads in it.
    fn read_in(&mut self, text: &str, scope: &Scope, depth: usize) -> String {
        let dir = self.dir.clone();
        let exists = |path: &str| dir.join(path).is_file();
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for named in files::named(text) {
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
            let Some(found) = paths::find(named.command, &args, scope, exists) else {
                // A name a macro makes isn't found either.
                self.inlined.missing += 1;
                continue;
            };
            let path = paths::tex_file(&found.path, exists);
            if !self.is_inside(&path) {
                self.inlined.outside += 1;
                continue;
            }
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
        paths::find("\\includegraphics", args, scope, exists)?;
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

    /// Files outside the document's folder aren't read, wherever a name or a
    /// link leads.
    #[test]
    fn files_outside_the_folder_are_not_read() {
        let root = tempfile::tempdir().unwrap();
        write_files(
            root.path(),
            &[("secret.tex", b"SECRET"), ("doc/in.tex", b"in")],
        );
        let doc = root.path().join("doc");
        let secret = root.path().join("secret.tex");
        let main = format!(
            "\\input{{in}} \\input{{../secret}} \\input{{{}}}",
            secret.display()
        );
        #[cfg(unix)]
        let main = {
            std::os::unix::fs::symlink(&secret, doc.join("link.tex")).unwrap();
            main + " \\input{link}"
        };
        let (read, warnings) = read_in(&main, &doc);
        assert!(
            !format!("{:?}", read.blocks).contains("SECRET"),
            "{:?}",
            read.blocks
        );
        assert!(format!("{:?}", read.blocks).contains("in"));
        let outside = if cfg!(unix) { 3 } else { 2 };
        assert_eq!(
            counts(&warnings),
            [("file outside the document's folder wasn't read", outside)]
        );
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
