//! Where the files a LaTeX document names are, as LaTeX with the `import`
//! package finds them (#170), so that their paths can be rewritten for
//! pandoc, which reads the files itself but doesn't know the package.
//!
//! The package's rules (`import.sty`, 2020 and later):
//! - `\import{folder}{file}` (and `\inputfrom`, `\includefrom`, which
//!   `\include`s) reads `folder` + `file`, `folder` relative to the
//!   document's; `\subimport` (and `\subinputfrom`, `\subincludefrom`)
//!   reads it with `folder` relative to the importing file's folder.
//! - While that file is read, `\input`, `\include` and `\includegraphics`
//!   look in its folder first, then in those of the files importing it,
//!   innermost first, then in the document's (`\input@path`,
//!   `\Ginput@path`).

/// The folders a file being read looks for files in, relative to the
/// document's, each ending in `/`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Scope {
    /// The package's current folder (`\import@path`), which a
    /// `\subimport`'s is relative to; empty in the document itself.
    pub folder: String,
    /// The folders looked in before the document's, innermost first.
    pub search: Vec<String>,
}

/// A file a command names, found.
#[derive(Debug, PartialEq)]
pub struct Found {
    /// Where it is, named from the document's folder as the command names
    /// it: the folder it was found in, then the name as written.
    pub path: String,
    /// The folders it looks for files in, where it's LaTeX read in.
    pub scope: Scope,
}

/// The extensions `\includegraphics` tries, in order, as graphicx does for
/// XeLaTeX (`\Gin@extensions` in `xetex.def`), which the app's PDF route
/// runs where it's installed.
const GRAPHICS: [&str; 22] = [
    ".pdf", ".PDF", ".ai", ".AI", ".png", ".PNG", ".jpg", ".JPG", ".jpeg", ".JPEG", ".jp2", ".JP2",
    ".jpf", ".JPF", ".bmp", ".BMP", ".ps", ".PS", ".eps", ".EPS", ".mps", ".MPS",
];

/// Where the file that `command`, with the arguments `args`, names is,
/// in a file read with `scope`: `exists` says whether a path from the
/// document's folder is a file. None where it isn't found, or `command`
/// names none (see `files::named`).
pub fn find(
    command: &str,
    args: &[&str],
    scope: &Scope,
    exists: impl Fn(&str) -> bool,
) -> Option<Found> {
    let found = |path: String, scope: Scope| Some(Found { path, scope });
    match (command, args) {
        ("\\input" | "\\include", [name]) => {
            let path = searched(scope, name, |path| is_tex(command, path, &exists))?;
            found(path, scope.clone())
        }
        // As graphicx looks: with an extension it knows, the name as it
        // is first, then each extension in turn in every folder; else
        // those, then the name as it is.
        ("\\includegraphics", [name]) => {
            let as_named = || searched(scope, name, &exists);
            let extended = || {
                GRAPHICS
                    .iter()
                    .find_map(|ext| searched(scope, name, |path| exists(&format!("{path}{ext}"))))
            };
            let path = if GRAPHICS.iter().any(|ext| name.ends_with(ext)) {
                as_named().or_else(extended)
            } else {
                extended().or_else(as_named)
            }?;
            found(path, scope.clone())
        }
        ("\\import" | "\\inputfrom" | "\\includefrom", [folder, name]) => {
            imported(command, folder, name, scope, exists)
        }
        ("\\subimport" | "\\subinputfrom" | "\\subincludefrom", [folder, name]) => {
            let folder = format!("{}{folder}", scope.folder);
            imported(command, &folder, name, scope, exists)
        }
        _ => None,
    }
}

/// The file an `\import` of `name` in `folder` reads, from a file read
/// with `scope`: `folder` + `name`, looked for as `\input` looks for it
/// once `folder` is looked in first, as the file is then read.
fn imported(
    command: &str,
    folder: &str,
    name: &str,
    scope: &Scope,
    exists: impl Fn(&str) -> bool,
) -> Option<Found> {
    // The package ends a folder with `/` where it has none.
    let folder = match folder {
        "" => String::new(),
        folder if folder.ends_with('/') => folder.to_string(),
        folder => format!("{folder}/"),
    };
    let reads = if command.ends_with("includefrom") {
        "\\include"
    } else {
        "\\input"
    };
    let search = std::iter::once(folder.clone())
        .chain(scope.search.iter().cloned())
        .collect();
    let scope = Scope { folder, search };
    let named = format!("{}{name}", scope.folder);
    let path = searched(&scope, &named, |path| is_tex(reads, path, &exists))?;
    Some(Found { path, scope })
}

/// `name` looked for in `scope`'s folders, innermost first, then in the
/// document's: the first path `is` takes, named from the document's
/// folder. A name from the root is looked for as it is.
fn searched(scope: &Scope, name: &str, is: impl Fn(&str) -> bool) -> Option<String> {
    if name.starts_with('/') {
        return is(name).then(|| name.to_string());
    }
    scope
        .search
        .iter()
        .map(String::as_str)
        .chain([""])
        .map(|folder| format!("{folder}{name}"))
        .find(|path| is(path))
}

/// Whether `path`, as `command` (`\input` or `\include`) names it, is a
/// LaTeX file: `\include` names a `.tex` file, with its extension or
/// without; `\input` that, or a file of another name.
fn is_tex(command: &str, path: &str, exists: impl Fn(&str) -> bool) -> bool {
    let read = tex_file(path, &exists);
    exists(&read) && (command == "\\input" || read.ends_with(".tex"))
}

/// The file `\input` or `\include` reads where it names `path`, found
/// (see [`find`]): `path` with `.tex`, where that is a file, else `path`.
pub fn tex_file(path: &str, exists: impl Fn(&str) -> bool) -> String {
    let tex = format!("{}.tex", path.strip_suffix(".tex").unwrap_or(path));
    if exists(&tex) { tex } else { path.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The files of a book laid out as the APCS book is (#170), from its
    /// folder.
    const BOOK: [&str; 9] = [
        "main.tex",
        "common.tex",
        "Files/-Preamble/-Preamble.tex",
        "Files/04DS/DS.tex",
        "Files/04DS/BD-BIT.tex",
        "Files/04DS/common.tex",
        "Files/04DS/sub/deep.tex",
        "Files/Images/BIT.png",
        "Files/Images/tree.pdf",
    ];

    fn find_in_book(command: &str, args: &[&str], scope: &Scope) -> Option<Found> {
        find(command, args, scope, |path| {
            BOOK.contains(&resolved(path).as_str())
        })
    }

    /// `path` with its `..` resolved, as the file system resolves it.
    fn resolved(path: &str) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                ".." => {
                    parts.pop();
                }
                part => parts.push(part),
            }
        }
        parts.join("/")
    }

    fn scope(folders: &[&str]) -> Scope {
        Scope {
            folder: folders.first().map_or(String::new(), |f| f.to_string()),
            search: folders.iter().map(|f| f.to_string()).collect(),
        }
    }

    #[test]
    fn imports_read_their_folder_from_the_document_or_the_importing_file() {
        let top = Scope::default();
        let cases = [
            (
                "\\import",
                ["Files/-Preamble/", "-Preamble"],
                &top,
                "Files/-Preamble/-Preamble",
                scope(&["Files/-Preamble/"]),
            ),
            // A folder without its `/`, as the book writes some.
            (
                "\\subincludefrom",
                ["Files/04DS", "DS"],
                &top,
                "Files/04DS/DS",
                scope(&["Files/04DS/"]),
            ),
            (
                "\\inputfrom",
                ["Files/04DS/", "DS"],
                &top,
                "Files/04DS/DS",
                scope(&["Files/04DS/"]),
            ),
        ];
        for (command, args, from, path, read_with) in cases {
            assert_eq!(
                find_in_book(command, &args, from),
                Some(Found {
                    path: path.into(),
                    scope: read_with
                }),
                "{command} {args:?}"
            );
        }
        // A `\subimport` is from the importing file's folder, an `\import`
        // from the document's, wherever it is.
        let ds = scope(&["Files/04DS/"]);
        let deep = find_in_book("\\subimport", &["sub/", "deep"], &ds).unwrap();
        assert_eq!(deep.path, "Files/04DS/sub/deep");
        assert_eq!(
            deep.scope,
            Scope {
                folder: "Files/04DS/sub/".into(),
                search: vec!["Files/04DS/sub/".into(), "Files/04DS/".into()],
            }
        );
        let again = find_in_book("\\import", &["Files/04DS/", "DS"], &deep.scope).unwrap();
        assert_eq!(again.path, "Files/04DS/DS");
        // Not found where it's named, it isn't looked for elsewhere.
        assert_eq!(
            find_in_book("\\subimport", &["Files/04DS/", "DS"], &ds),
            None
        );
    }

    #[test]
    fn files_in_an_imported_file_are_found_from_its_folder_first() {
        let ds = scope(&["Files/04DS/"]);
        let path = |command, name| find_in_book(command, &[name], &ds).map(|found| found.path);
        assert_eq!(
            path("\\input", "BD-BIT").as_deref(),
            Some("Files/04DS/BD-BIT")
        );
        assert_eq!(
            path("\\include", "BD-BIT").as_deref(),
            Some("Files/04DS/BD-BIT")
        );
        // In both its folder and the document's, its own; then the
        // document's.
        assert_eq!(
            path("\\input", "common").as_deref(),
            Some("Files/04DS/common")
        );
        assert_eq!(path("\\input", "main.tex").as_deref(), Some("main.tex"));
        // Images, with their extension or without.
        assert_eq!(
            path("\\includegraphics", "../Images/BIT.png").as_deref(),
            Some("Files/04DS/../Images/BIT.png")
        );
        assert_eq!(
            path("\\includegraphics", "../Images/tree").as_deref(),
            Some("Files/04DS/../Images/tree")
        );
        assert_eq!(path("\\input", "gone"), None);
        // `\include` takes only a `.tex` file, named with it or without.
        assert_eq!(path("\\include", "main.tex").as_deref(), Some("main.tex"));
        assert_eq!(path("\\include", "Images/BIT.png"), None);
    }

    /// As LaTeX looks: an image's extensions in turn in every folder, and
    /// an imported file in the folders the import adds too.
    #[test]
    fn files_are_found_in_the_order_latex_looks() {
        const FILES: [&str; 10] = [
            "chapter/scan.png.pdf",
            "chapter/figure.png",
            "figure.pdf",
            "chapter/other/nested.tex",
            "chapter/included.tex",
            "chapter/figure.v1.png",
            "diagram.ai",
            "chapter/diagram.png",
            "plot.v1",
            "chapter/plot.v1.png",
        ];
        let chapter = scope(&["chapter/"]);
        let find = |command, args: &[&str]| {
            find(command, args, &chapter, |path| FILES.contains(&path)).map(|found| found.path)
        };
        assert_eq!(
            find("\\includegraphics", &["figure"]).as_deref(),
            Some("figure")
        );
        assert_eq!(
            find("\\includegraphics", &["figure.png"]).as_deref(),
            Some("chapter/figure.png")
        );
        // An extension graphicx doesn't know takes one it does first, and
        // is the name as it is only then; and XeLaTeX's order of them.
        assert_eq!(
            find("\\includegraphics", &["figure.v1"]).as_deref(),
            Some("chapter/figure.v1")
        );
        assert_eq!(
            find("\\includegraphics", &["plot.v1"]).as_deref(),
            Some("chapter/plot.v1")
        );
        // A known extension not found as it's named takes another too.
        assert_eq!(
            find("\\includegraphics", &["scan.png"]).as_deref(),
            Some("chapter/scan.png")
        );
        assert_eq!(
            find("\\includegraphics", &["diagram"]).as_deref(),
            Some("diagram")
        );
        assert_eq!(
            find("\\import", &["other/", "nested"]).as_deref(),
            Some("chapter/other/nested")
        );
        assert_eq!(
            find("\\includefrom", &["chapter/", "included.tex"]).as_deref(),
            Some("chapter/included.tex")
        );
    }

    #[test]
    fn the_document_finds_files_in_its_own_folder() {
        let top = Scope::default();
        let found = find_in_book("\\input", &["common"], &top).unwrap();
        assert_eq!((found.path.as_str(), found.scope), ("common", top.clone()));
        assert_eq!(find_in_book("\\input", &["BD-BIT"], &top), None);
        assert_eq!(find_in_book("\\input", &["/etc/none"], &top), None);
        assert_eq!(find_in_book("\\section", &["x"], &top), None);
    }
}
