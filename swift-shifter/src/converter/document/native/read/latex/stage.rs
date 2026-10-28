//! A document using the `import` package (#170), written out for pandoc,
//! which reads the files a document names itself but doesn't know the
//! package. Each LaTeX file the document reads is copied into a folder of
//! its own, where it is from the document's, as it is but for the paths its
//! commands name (see [`files`], [`paths`]): an import becomes the `\input`
//! or `\include` of the file it reads, and an `\input` or `\include` in an
//! imported file names its file's copy, both where the copy is, and an
//! `\includegraphics` its image from the document's folder. Pandoc then
//! reads the copies from the document's folder as it reads any document,
//! finding there what they don't rewrite, as a name a macro makes, as it
//! would for the document; no text is moved between files. A file read
//! from imports looking in other folders is copied once for each, as the
//! files it names are found in theirs (see [`another`]).
//!
//! Known limits:
//! - A name with a comment in it isn't found.
//! - A path's `..` is resolved as written, where LaTeX resolves it from
//!   where a symbolic link leads: from `link/../a.tex`, `link` a link to
//!   `real/sub`, LaTeX reads `real/a.tex` and this `a.tex`. At worst, where
//!   both are there, the other file is read, with no warning.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use super::paths::Scope;
use super::{files, paths};

/// How many files are copied at most: past it, as where each of a chain
/// of files imports the next twice, the document isn't staged.
const MAX_FILES: usize = 1000;

/// A document written out for pandoc (see [`stage`]).
#[derive(Debug, PartialEq)]
pub struct Staged {
    /// The document's copy.
    pub main: PathBuf,
    /// The files it names that weren't found, as named: their commands
    /// are left as they are.
    pub missing: Vec<String>,
}

/// `main`, written out under `into` (see the module), where it uses the
/// `import` package's commands; None where it doesn't, and is left as it
/// is for pandoc. Both are absolute: the copies name each other from
/// `into`, the rest from `main`'s folder. An error where `main` can't be
/// read; where it's staged, also where a file it reads can't be, a copy
/// written, or it reads more than [`MAX_FILES`].
pub fn stage(main: &Path, into: &Path) -> std::io::Result<Option<Staged>> {
    let folder = main.parent().unwrap_or(Path::new("."));
    let name = PathBuf::from(main.file_name().unwrap_or_default());
    let exists = |path: &str| folder.join(path).is_file();
    // Where each file, read looking in its folders, is copied to.
    let mut copied = HashMap::from([((name.clone(), Scope::default()), name.clone())]);
    let mut taken = HashSet::from([name.clone()]);
    let mut queue = VecDeque::from([(name.clone(), Scope::default(), name.clone())]);
    let mut copies: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut missing = Vec::new();
    let mut imports = false;
    // A file found but not read, whose copy would be missing: an error
    // where the document is staged.
    let mut unread = None;
    // Past `MAX_FILES`, what's queued is still read for an import, as a
    // document without one isn't staged and has no limit.
    let mut too_many = false;
    while let Some((path, scope, copy)) = queue.pop_front() {
        let bytes = match std::fs::read(folder.join(&path)) {
            Ok(bytes) => bytes,
            Err(e) if path == name => return Err(e),
            Err(e) => {
                let path = path.display();
                unread.get_or_insert(std::io::Error::new(e.kind(), format!("{path}: {e}")));
                continue;
            }
        };
        // Pandoc reads only UTF-8: anything else is copied as it is.
        let Ok(text) = String::from_utf8(bytes.clone()) else {
            copies.push((copy, bytes));
            continue;
        };
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        for named in files::named(&text) {
            let args: Vec<&str> = named.args.iter().map(|arg| &text[arg.clone()]).collect();
            let import = named.args.len() == 2;
            imports |= import;
            let Some(found) = paths::find(named.command, &args, &scope, exists) else {
                // A name a macro makes is pandoc's to find.
                if !args.iter().any(|arg| arg.contains('\\')) {
                    missing.push(args.last().copied().unwrap_or_default().to_string());
                }
                continue;
            };
            // Named with its `..` resolved: a folder `ch/sub/../` names may
            // not be there. A LaTeX file copied is named where its copy is,
            // `.tex` and all: pandoc reads `part.v1` as it is, not as
            // `part.v1.tex`.
            let mut named_as = within(&found.path)
                .map(|path| slashed(&path))
                .unwrap_or_else(|| slashed(&folder.join(&found.path)));
            if named.command != "\\includegraphics"
                && let Some(read) = within(&paths::tex_file(&found.path, exists))
            {
                let key = (read.clone(), unrepeated(&found.scope));
                let copy = match copied.get(&key) {
                    Some(copy) => copy.clone(),
                    None if copied.len() >= MAX_FILES => {
                        too_many = true;
                        continue;
                    }
                    None => {
                        let copy = if taken.contains(&read) {
                            another(&read, |path| {
                                taken.contains(path) || folder.join(path).exists()
                            })
                        } else {
                            read.clone()
                        };
                        taken.insert(copy.clone());
                        copied.insert(key, copy.clone());
                        queue.push_back((read.clone(), found.scope, copy.clone()));
                        copy
                    }
                };
                named_as = slashed(&into.join(&copy));
            }
            if import {
                let reads = if named.command.ends_with("includefrom") {
                    "\\include"
                } else {
                    "\\input"
                };
                edits.push((named.range, format!("{reads}{{{named_as}}}")));
            } else {
                edits.push((named.args[0].clone(), named_as));
            }
        }
        copies.push((copy, edited(&text, edits).into_bytes()));
    }
    if !imports {
        return Ok(None);
    }
    if let Some(e) = unread {
        return Err(e);
    }
    if too_many {
        return Err(std::io::Error::other(format!(
            "the document reads more than {MAX_FILES} files"
        )));
    }
    for (path, bytes) in copies {
        let copy = into.join(path);
        if let Some(parent) = copy.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(copy, bytes)?;
    }
    Ok(Some(Staged {
        main: into.join(name),
        missing,
    }))
}

/// `text` with each of `edits`' ranges replaced, in order.
fn edited(text: &str, mut edits: Vec<(Range<usize>, String)>) -> String {
    edits.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (range, with) in edits {
        if range.start < copied {
            continue;
        }
        out.push_str(&text[copied..range.start]);
        out.push_str(&with);
        copied = range.end;
    }
    out.push_str(&text[copied..]);
    out
}

/// `path`, from the document's folder, with its `..` resolved, where it
/// stays in that folder; None where it leaves it, or is from the root,
/// where no copy of it is written.
fn within(path: &str) -> Option<PathBuf> {
    let mut within = PathBuf::new();
    for part in Path::new(path).components() {
        match part {
            Component::Normal(part) => within.push(part),
            Component::CurDir => {}
            Component::ParentDir if within.pop() => {}
            _ => return None,
        }
    }
    Some(within)
}

/// `path` as LaTeX names it, with `/` between its parts.
fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `scope`, its folders' `.` and `..` resolved and each folder looked in
/// once: as it finds files, so that a file importing itself from `./` is
/// read with it once.
fn unrepeated(scope: &Scope) -> Scope {
    let resolved = |folder: &String| within(folder).map_or_else(|| folder.clone(), |f| slashed(&f));
    let mut search = Vec::new();
    for folder in scope.search.iter().map(resolved) {
        if !search.contains(&folder) {
            search.push(folder);
        }
    }
    Scope {
        folder: resolved(&scope.folder),
        search,
    }
}

/// Where `path` is copied once more, for an import looking in other
/// folders: `ch/a.tex` as `ch/a-2.tex`, or the first such name not
/// `taken`. It is named with `.tex`, which pandoc reads as it is.
fn another(path: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let path = slashed(path);
    let stem = path.strip_suffix(".tex").unwrap_or(&path);
    (2..)
        .map(|n| PathBuf::from(format!("{stem}-{n}.tex")))
        .find(|path| !taken(path))
        .expect("a name is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A book laid out as the APCS book is (#170), with the files outside
    /// its folder `../shared.tex` names, staged.
    fn staged(files: &[(&str, &str)]) -> (tempfile::TempDir, Option<Staged>) {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book");
        for (name, text) in files {
            let file = book.join(name);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        let into = dir.path().join("staged");
        let staged = stage(&book.join(files[0].0), &into).unwrap();
        (dir, staged)
    }

    /// The copy of `path`, the other copies it names named from the
    /// staged folder, for short.
    fn copy(dir: &tempfile::TempDir, path: &str) -> String {
        let into = dir.path().join("staged");
        std::fs::read_to_string(into.join(path))
            .unwrap()
            .replace(&format!("{}/", slashed(&into)), "")
    }

    #[test]
    fn imports_become_inputs_of_the_files_they_read() {
        let (dir, staged) = staged(&[
            (
                "main.tex",
                "\\import{Files/-Preamble/}{-Preamble}\n\
                 \\subincludefrom{Files/04DS}{DS}\n\\input{gone}\n",
            ),
            ("Files/-Preamble/-Preamble.tex", "\\usepackage{x}\n"),
            (
                "Files/04DS/DS.tex",
                "\\input{BD-BIT}\n\\includegraphics{../Images/BIT.png}\n\
                 \\subimport{sub/}{deep}\n",
            ),
            ("Files/04DS/BD-BIT.tex", "BIT\n"),
            ("Files/04DS/sub/deep.tex", "Deep \\input{common}\n"),
            ("common.tex", "Common\n"),
            ("Files/Images/BIT.png", ""),
        ]);
        let staged = staged.unwrap();
        assert_eq!(staged.main, dir.path().join("staged/main.tex"));
        assert_eq!(staged.missing, ["gone"]);
        // Copies name each other where they are, the rest as before.
        let main = std::fs::read_to_string(&staged.main).unwrap();
        let preamble = slashed(&dir.path().join("staged/Files/-Preamble/-Preamble.tex"));
        assert!(
            main.starts_with(&format!("\\input{{{preamble}}}")),
            "{main}"
        );
        assert_eq!(
            copy(&dir, "main.tex"),
            "\\input{Files/-Preamble/-Preamble.tex}\n\
             \\include{Files/04DS/DS.tex}\n\\input{gone}\n"
        );
        assert_eq!(
            copy(&dir, "Files/04DS/DS.tex"),
            "\\input{Files/04DS/BD-BIT.tex}\n\
             \\includegraphics{Files/Images/BIT.png}\n\
             \\input{Files/04DS/sub/deep.tex}\n"
        );
        assert_eq!(
            copy(&dir, "Files/04DS/sub/deep.tex"),
            "Deep \\input{common.tex}\n"
        );
        assert_eq!(copy(&dir, "Files/04DS/BD-BIT.tex"), "BIT\n");
        assert_eq!(
            copy(&dir, "Files/-Preamble/-Preamble.tex"),
            "\\usepackage{x}\n"
        );
        // Images are left where they are, for pandoc to find from the
        // document's folder.
        assert!(!dir.path().join("staged/Files/Images/BIT.png").exists());
    }

    #[test]
    fn a_document_without_imports_is_left_as_it_is() {
        let (dir, staged) = staged(&[("main.tex", "\\input{one}"), ("one.tex", "One")]);
        assert_eq!(staged, None);
        assert!(!dir.path().join("staged").exists());
    }

    #[test]
    fn files_outside_the_folder_are_named_where_they_are_and_not_copied() {
        let (dir, staged) = staged(&[
            ("main.tex", "\\import{ch/}{a}"),
            ("ch/a.tex", "\\input{../../shared}"),
            ("../shared.tex", "Shared"),
        ]);
        assert!(staged.unwrap().missing.is_empty());
        let shared = slashed(&dir.path().join("book/ch/../../shared"));
        assert_eq!(copy(&dir, "ch/a.tex"), format!("\\input{{{shared}}}"));
        // Only the document's own files are copied, all within the copy.
        let mut copied = walk(&dir.path().join("staged"));
        copied.sort();
        assert_eq!(copied, ["ch/a.tex", "main.tex"]);
    }

    /// The files under `folder`, from it, with `/` between their parts.
    fn walk(folder: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                found.extend(walk(&path).into_iter().map(|f| format!("{name}/{f}")));
            } else {
                found.push(name);
            }
        }
        found
    }

    #[test]
    fn files_reading_each_other_are_copied_once() {
        let (dir, staged) = staged(&[
            ("main.tex", "\\import{a/}{x}"),
            ("a/x.tex", "\\subimport{./}{x}"),
        ]);
        assert!(staged.is_some());
        assert_eq!(copy(&dir, "a/x.tex"), "\\input{a/x.tex}");
    }

    #[test]
    fn a_folder_left_by_dot_dot_is_named_without_it() {
        let (dir, _) = staged(&[
            ("main.tex", "\\import{ch/sub/../}{a}"),
            ("ch/a.tex", "A"),
            ("ch/sub/b.tex", ""),
        ]);
        assert_eq!(copy(&dir, "main.tex"), "\\input{ch/a.tex}");
        assert_eq!(copy(&dir, "ch/a.tex"), "A");
        // Only the files read are copied: `ch/sub` has none.
        assert!(!dir.path().join("staged/ch/sub").exists());
    }

    #[test]
    fn a_file_two_imports_read_is_copied_for_each() {
        let (dir, staged) = staged(&[
            ("main.tex", "\\import{a/}{x}\\import{b/}{x}"),
            ("a/x.tex", "\\input{common}"),
            ("b/x.tex", "\\input{common}"),
            ("common.tex", "\\input{leaf}"),
            ("a/leaf.tex", "Alpha"),
            ("b/leaf.tex", "Beta"),
        ]);
        assert!(staged.unwrap().missing.is_empty());
        assert_eq!(copy(&dir, "a/x.tex"), "\\input{common.tex}");
        assert_eq!(copy(&dir, "common.tex"), "\\input{a/leaf.tex}");
        assert_eq!(copy(&dir, "b/x.tex"), "\\input{common-2.tex}");
        assert_eq!(copy(&dir, "common-2.tex"), "\\input{b/leaf.tex}");
    }

    #[test]
    fn a_file_with_a_dot_in_its_name_is_named_with_tex() {
        let (dir, _) = staged(&[
            (
                "main.tex",
                "\\import{ch/}{part.v1}\\includefrom{ch/}{part.v1}",
            ),
            ("ch/part.v1.tex", "Part"),
        ]);
        assert_eq!(
            copy(&dir, "main.tex"),
            "\\input{ch/part.v1.tex}\\include{ch/part.v1.tex}"
        );
        assert_eq!(copy(&dir, "ch/part.v1.tex"), "Part");
    }

    /// A file found but not read, as one its user may not read, fails the
    /// staging, as its copy would be missing.
    #[cfg(unix)]
    #[test]
    fn a_file_that_cant_be_read_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), "\\import{ch/}{a}").unwrap();
        std::fs::create_dir(dir.path().join("ch")).unwrap();
        let unreadable = dir.path().join("ch/a.tex");
        std::fs::write(&unreadable, "A").unwrap();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&unreadable).is_ok() {
            return; // Run with permission to read anything, as root.
        }
        let into = dir.path().join("staged");
        let error = stage(&dir.path().join("main.tex"), &into).unwrap_err();
        assert!(error.to_string().contains("ch/a.tex"), "{error}");
        assert!(!into.exists());
    }

    #[test]
    fn a_file_named_with_tex_is_the_one_copied() {
        let (dir, _) = staged(&[
            ("main.tex", "\\import{ch/}{a.tex}"),
            ("ch/a.tex", "A"),
            ("ch/a.tex.tex", "Not A"),
        ]);
        assert_eq!(copy(&dir, "main.tex"), "\\input{ch/a.tex}");
        assert_eq!(copy(&dir, "ch/a.tex"), "A");
    }

    #[test]
    fn a_document_reading_too_many_files_is_not_staged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.tex"), "\\import{}{f1}").unwrap();
        for n in 1..=MAX_FILES {
            let next = format!("\\input{{f{}}}", n + 1);
            std::fs::write(dir.path().join(format!("f{n}.tex")), next).unwrap();
        }
        let into = dir.path().join("staged");
        assert!(stage(&dir.path().join("main.tex"), &into).is_err());
        assert!(!into.exists());
    }

    #[test]
    fn a_document_without_imports_has_no_limit() {
        let dir = tempfile::tempdir().unwrap();
        let inputs: String = (1..=MAX_FILES + 1)
            .map(|n| format!("\\input{{f{n}}}"))
            .collect();
        std::fs::write(dir.path().join("main.tex"), inputs).unwrap();
        for n in 1..=MAX_FILES + 1 {
            std::fs::write(dir.path().join(format!("f{n}.tex")), "").unwrap();
        }
        let into = dir.path().join("staged");
        assert_eq!(stage(&dir.path().join("main.tex"), &into).unwrap(), None);
    }
}
