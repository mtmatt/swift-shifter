//! The one PDF route that still uses an external engine: `.tex` input, when
//! a LaTeX engine is installed. LaTeX keeps raw LaTeX that pandoc's typst
//! writer would drop. Everything else is typeset in-process with typst (see
//! `native::typst`), and so is `.tex` when no suitable engine exists.

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

/// An installed engine: pandoc's name for it and the binary to run.
#[derive(Debug, Clone, PartialEq)]
struct Engine {
    name: &'static str,
    program: PathBuf,
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
