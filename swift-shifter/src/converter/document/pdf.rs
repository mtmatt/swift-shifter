//! The one PDF route that still uses an external engine: `.tex` input, when
//! a LaTeX engine is installed, typeset by it as written
//! ([`typeset_latex`]). Everything else is typeset in-process with typst
//! (see `native::typst`), and so is `.tex` when no suitable engine exists
//! or the engine fails. [`latex_setup`] is the pandoc route's, which no
//! conversion takes now (#73's 22b removes it).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use crate::converter::document::binaries::BREW_PATHS;
use crate::converter::document::native::ast::contains_cjk;

/// LaTeX engines pandoc can drive, most preferred first. pdflatex can't
/// typeset CJK text, so it's never chosen for it.
const ENGINES: &[&str] = &["tectonic", "xelatex", "lualatex", "pdflatex"];

/// CJK font for the XeLaTeX-family engines, which can't report what's
/// installed. These ship with the OS (or the usual Noto CJK package on Linux).
#[cfg(target_os = "macos")]
const LATEX_CJK_FONT: &str = "Songti SC";
#[cfg(target_os = "windows")]
const LATEX_CJK_FONT: &str = "SimSun";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const LATEX_CJK_FONT: &str = "Noto Serif CJK SC";

/// An installed engine: its name, which says how pandoc and
/// [`latex_command`] run it, and the binary to run.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Engine {
    pub(crate) name: &'static str,
    pub(crate) program: PathBuf,
}

/// The most times a TeX Live engine runs on a document, again while its
/// log says references changed. Tectonic reruns itself.
const MAX_RUNS: usize = 3;

/// The most folders below the document's that [`typeset_latex`] makes again
/// where the engine writes, and how deep it goes.
const MAX_FOLDERS: usize = 500;
const MAX_FOLDER_DEPTH: usize = 3;

/// How many lines of an engine's error a failure says.
const ERROR_LINES: usize = 3;

/// The installed engine that can typeset `input`, a `.tex` file, as it's
/// written, or `None`.
pub(crate) fn latex_engine(input: &Path) -> Option<Engine> {
    choose(&installed_engines(), has_cjk(input)).cloned()
}

/// Whether `input`, a file, has CJK text.
fn has_cjk(input: &Path) -> bool {
    std::fs::read(input)
        .map(|bytes| contains_cjk(&String::from_utf8_lossy(&bytes)))
        .unwrap_or(false)
}

/// `input`, a `.tex` file, typeset by `engine` as LaTeX would, run in its
/// folder, where it finds what it reads in, writing into a temp folder.
/// Shell escape is off, as it is by default: a document can't run
/// commands. It also fails when a document with CJK text has characters
/// the engine's fonts lack, which the engine only notes in its log:
/// without a CJK font set, that is all of the CJK text. The error, where
/// it fails, is the engine's name and the first lines of what it said.
pub(crate) fn typeset_latex(input: &Path, engine: &Engine) -> Result<Vec<u8>, String> {
    let input = std::path::absolute(input)
        .map_err(|e| format!("Failed to resolve {}: {e}", input.display()))?;
    let folder = input.parent().unwrap_or(Path::new("/"));
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let out = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {e}"))?;
    // What the engine makes is named after the whole stem: `paper.v1.pdf`.
    let made = |extension: &str| out.path().join(format!("{stem}.{extension}"));
    // TeX Live writes an `\include`d file's `.aux` in the folder it names,
    // under the output folder, and stops where there's none.
    make_folders_again(folder, out.path());
    let runs = if engine.name == "tectonic" {
        1
    } else {
        MAX_RUNS
    };
    for _ in 0..runs {
        let output = latex_command(engine, &input, out.path())
            .output()
            .map_err(|e| format!("{}: couldn't be run ({e})", engine.name))?;
        let log_text = std::fs::read_to_string(made("log")).unwrap_or_default();
        let printed = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() {
            let error = latex_error(&log_text, &printed);
            return Err(format!("{}: {error}", engine.name));
        }
        if let Some(missing) =
            (log_text.lines().chain(printed.lines())).find(|line| line.contains(MISSING_CHARACTER))
            && has_cjk(&input)
        {
            return Err(format!("{}: {}", engine.name, missing.trim()));
        }
        let changed = ["Rerun to get", "Label(s) may have changed"];
        if !changed.iter().any(|said| log_text.contains(said)) {
            break;
        }
    }
    std::fs::read(made("pdf")).map_err(|e| format!("{}: no PDF was made ({e})", engine.name))
}

/// What TeX Live and tectonic note of a character the font lacks.
const MISSING_CHARACTER: &str = "Missing character: There is no";

/// `engine` on `input`, in its folder, writing into `out`.
fn latex_command(engine: &Engine, input: &Path, out: &Path) -> std::process::Command {
    let mut cmd = crate::process::sync_command(&engine.program);
    cmd.current_dir(input.parent().unwrap_or(Path::new("/")));
    if engine.name == "tectonic" {
        // `--untrusted` keeps shell escape off whatever else asks for it.
        cmd.arg("--untrusted").arg("--outdir").arg(out);
        cmd.args(["--chatter", "minimal"]);
    } else {
        // `-disable-write18`: TeX Live's synonym for `-no-shell-escape`,
        // and the spelling MiKTeX documents.
        cmd.args([
            "-disable-write18",
            "-interaction=nonstopmode",
            "-halt-on-error",
        ]);
        cmd.arg(format!("-output-directory={}", out.display()));
    }
    cmd.arg(input.file_name().unwrap_or_default());
    if let Some(path) = search_path(std::env::var_os("PATH"), engine) {
        cmd.env("PATH", path);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd
}

/// The folders below `from`, but hidden ones, made again below `to`, as
/// many as [`MAX_FOLDERS`], as deep as [`MAX_FOLDER_DEPTH`].
fn make_folders_again(from: &Path, to: &Path) {
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf(), 0)];
    let mut made = 0;
    while let Some((from, to, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&from) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let hidden = name.to_string_lossy().starts_with('.');
            if hidden || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if made == MAX_FOLDERS {
                return;
            }
            let again = to.join(&name);
            if std::fs::create_dir(&again).is_ok() {
                made += 1;
                if depth + 1 < MAX_FOLDER_DEPTH {
                    pending.push((entry.path(), again, depth + 1));
                }
            }
        }
    }
}

/// The first lines of a LaTeX error: in `log`, from its first line
/// beginning `!`, as TeX Live writes one; or else in `printed`, what the
/// engine printed, from its first line beginning `error:`, as tectonic
/// prints one after its warnings, or from its start.
fn latex_error(log: &str, printed: &str) -> String {
    // `text`'s lines from the first beginning `prefix`, where one does.
    fn from<'a>(text: &'a str, prefix: &str) -> Option<Vec<&'a str>> {
        let start = text.lines().position(|line| line.starts_with(prefix))?;
        Some(text.lines().skip(start).collect())
    }
    let lines = from(log, "!")
        .or_else(|| from(printed, "error:"))
        .unwrap_or_else(|| printed.lines().collect());
    let said: Vec<&str> = lines
        .into_iter()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(ERROR_LINES)
        .collect();
    if said.is_empty() {
        "it stopped without saying why".to_string()
    } else {
        said.join(" ")
    }
}

/// How to run pandoc with a LaTeX engine.
#[derive(Debug, PartialEq)]
pub struct LatexSetup {
    /// `--pdf-engine NAME` and any font variables.
    pub args: Vec<String>,
    /// A `PATH` that also holds the engine's directory, when it isn't on
    /// `PATH` already (e.g. Homebrew's bin for a GUI app).
    pub path: Option<OsString>,
}

/// The LaTeX setup for `input` (a `.tex` file), or `None` when no engine
/// that can typeset it is installed: then the caller typesets via typst.
pub fn latex_setup(input: &Path) -> Option<LatexSetup> {
    let engines = installed_engines();
    let has_cjk = std::fs::read(input)
        .map(|bytes| contains_cjk(&String::from_utf8_lossy(&bytes)))
        .unwrap_or(false);
    let engine = choose(&engines, has_cjk)?;
    Some(LatexSetup {
        args: engine_args(engine, has_cjk),
        path: search_path(std::env::var_os("PATH"), engine),
    })
}

fn installed_engines() -> Vec<Engine> {
    ENGINES
        .iter()
        .filter_map(|&name| find_engine(name).map(|program| Engine { name, program }))
        .collect()
}

fn find_engine(name: &str) -> Option<PathBuf> {
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    #[cfg(target_os = "macos")]
    for dir in BREW_PATHS {
        let candidate = Path::new(dir).join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn choose(engines: &[Engine], has_cjk: bool) -> Option<&Engine> {
    engines.iter().find(|e| !(has_cjk && e.name == "pdflatex"))
}

/// Pandoc gets the engine's name, not its path: `search_path` makes sure it
/// can find it. xeCJK / luatexja (pulled in by pandoc's template when
/// `CJKmainfont` is set) handle CJK once they have a font.
fn engine_args(engine: &Engine, has_cjk: bool) -> Vec<String> {
    let mut args = vec!["--pdf-engine".to_string(), engine.name.to_string()];
    if has_cjk {
        args.push("-V".to_string());
        args.push(format!("CJKmainfont={LATEX_CJK_FONT}"));
    }
    args
}

/// `current` with the engine's directory appended, or `None` when it's
/// already there.
fn search_path(current: Option<OsString>, engine: &Engine) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = current
        .as_deref()
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let dir = engine.program.parent()?;
    if dirs.iter().any(|d| d == dir) {
        return None;
    }
    dirs.push(dir.to_path_buf());
    std::env::join_paths(dirs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(name: &'static str, dir: &str) -> Engine {
        Engine {
            name,
            program: Path::new(dir).join(name),
        }
    }

    #[test]
    fn plain_documents_use_the_first_engine() {
        let engines = [
            engine("pdflatex", "/usr/bin"),
            engine("xelatex", "/usr/bin"),
        ];
        assert_eq!(choose(&engines, false).unwrap().name, "pdflatex");
    }

    #[test]
    fn cjk_skips_pdflatex() {
        let engines = [
            engine("pdflatex", "/usr/bin"),
            engine("lualatex", "/usr/bin"),
        ];
        assert_eq!(choose(&engines, true).unwrap().name, "lualatex");
        assert!(
            choose(&[engine("pdflatex", "/usr/bin")], true).is_none(),
            "typst takes over"
        );
        assert!(choose(&[], false).is_none());
    }

    #[test]
    fn cjk_sets_a_font_for_xelatex_family() {
        let args = engine_args(&engine("xelatex", "/usr/bin"), true);
        assert_eq!(args[..2], ["--pdf-engine", "xelatex"]);
        assert_eq!(args[3], format!("CJKmainfont={LATEX_CJK_FONT}"));
        assert_eq!(engine_args(&engine("xelatex", "/usr/bin"), false).len(), 2);
    }

    fn args(cmd: &std::process::Command) -> Vec<String> {
        cmd.get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn engines_run_without_shell_escape_in_the_documents_folder() {
        let input = Path::new("/docs/paper/main.tex");
        let out = Path::new("/tmp/out");
        for name in ["xelatex", "lualatex", "pdflatex"] {
            let cmd = latex_command(&engine(name, "/usr/bin"), input, out);
            assert_eq!(
                args(&cmd),
                [
                    "-disable-write18",
                    "-interaction=nonstopmode",
                    "-halt-on-error",
                    "-output-directory=/tmp/out",
                    "main.tex",
                ],
                "{name}"
            );
            assert_eq!(cmd.get_current_dir(), Some(Path::new("/docs/paper")));
        }
        let cmd = latex_command(&engine("tectonic", "/usr/bin"), input, out);
        assert_eq!(
            args(&cmd),
            [
                "--untrusted",
                "--outdir",
                "/tmp/out",
                "--chatter",
                "minimal",
                "main.tex"
            ]
        );
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/docs/paper")));
    }

    #[test]
    fn an_error_is_its_first_lines_in_the_log_or_else_as_printed() {
        let log = "This is XeTeX\n(./main.tex\n! Undefined control sequence.\n\nl.5 \\foo\n\nmore\nand more\n";
        assert_eq!(
            latex_error(log, "printed"),
            "! Undefined control sequence. l.5 \\foo more"
        );
        // Tectonic's warnings come first.
        let printed = "warning: main.tex:3: Overfull \\hbox\n\
                       error: main.tex:5: Undefined control sequence\n\
                       error: halted on potentially-recoverable error as specified\n";
        assert_eq!(
            latex_error("no error here\n", printed),
            "error: main.tex:5: Undefined control sequence \
             error: halted on potentially-recoverable error as specified"
        );
        assert_eq!(latex_error("", "\nsegfault\n"), "segfault");
        assert_eq!(latex_error("", " \n"), "it stopped without saying why");
    }

    #[test]
    fn the_documents_folders_are_made_again_where_the_engine_writes() {
        let from = tempfile::tempdir().unwrap();
        let to = tempfile::tempdir().unwrap();
        for folder in ["chapters/part", "a/b/c/d", ".git/objects"] {
            std::fs::create_dir_all(from.path().join(folder)).unwrap();
        }
        std::fs::write(from.path().join("chapters/intro.tex"), "").unwrap();
        make_folders_again(from.path(), to.path());
        assert!(to.path().join("chapters/part").is_dir());
        assert!(to.path().join("a/b/c").is_dir());
        // Deeper than `MAX_FOLDER_DEPTH`, hidden, or a file: not made.
        assert!(!to.path().join("a/b/c/d").exists());
        assert!(!to.path().join(".git").exists());
        assert!(!to.path().join("chapters/intro.tex").exists());
    }

    #[test]
    fn search_path_appends_a_missing_engine_dir() {
        let current = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let path = search_path(
            Some(current.clone()),
            &engine("xelatex", "/opt/homebrew/bin"),
        )
        .unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(dirs.last().unwrap(), Path::new("/opt/homebrew/bin"));
        assert!(search_path(Some(current), &engine("xelatex", "/usr/bin")).is_none());
    }
}
