//! What the benchmark's tests share: its list of test documents
//! (`bench/documents.toml`), and compiling one with XeLaTeX into the PDF
//! the benchmark scores against.

pub mod text;

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(serde::Deserialize)]
pub struct Manifest {
    pub document: Vec<Document>,
}

#[derive(serde::Deserialize)]
pub struct Document {
    pub dir: String,
    pub main: String,
    pub source: String,
    pub license: String,
    pub fetched: String,
    pub covers: Vec<String>,
}

pub fn bench() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench")
}

pub fn manifest() -> Manifest {
    let text = std::fs::read_to_string(bench().join("documents.toml")).expect("documents.toml");
    toml::from_str(&text).expect("documents.toml parses")
}

/// Compile `doc` in a copy under `scratch`: XeLaTeX twice, so references
/// resolve, with BibTeX between where the document asks for one (a
/// `\bibdata` in its `.aux`) and the paper came without a `.bbl`.
/// The PDF, or why there's none: the run that failed and what its log
/// says of it (see [`failure`]).
pub fn compile(doc: &Document, xelatex: &Path, scratch: &Path) -> Result<PathBuf, String> {
    let dir = scratch.join(&doc.dir);
    copy_dir(&bench().join("documents").join(&doc.dir), &dir);
    let main = dir.join(&doc.main);
    let work = main.parent().unwrap();
    let file = main.file_name().unwrap().to_string_lossy().into_owned();
    let stem = main.file_stem().unwrap().to_string_lossy().into_owned();
    let uploaded_bbl = work.join(format!("{stem}.bbl")).exists();
    // `log` is the file the program explains a failure in.
    let run = |program: &Path, args: &[&str], log: &str| -> Result<(), String> {
        let out = Command::new(program)
            .args(args)
            .current_dir(work)
            .output()
            .map_err(|e| format!("{}: {e}", program.display()))?;
        if out.status.success() {
            return Ok(());
        }
        let log =
            std::fs::read_to_string(work.join(format!("{stem}.{log}"))).unwrap_or_else(|_| {
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        Err(format!("{} failed:\n{}", program.display(), failure(&log)))
    };
    let tex = [
        "-interaction=nonstopmode",
        "-halt-on-error",
        "-no-shell-escape",
        &file,
    ];
    run(xelatex, &tex, "log")?;
    let aux = std::fs::read_to_string(work.join(format!("{stem}.aux"))).unwrap_or_default();
    if !uploaded_bbl && aux.contains("\\bibdata{") {
        let bibtex = which::which("bibtex").map_err(|e| format!("bibtex: {e}"))?;
        run(&bibtex, &[&stem], "blg")?;
        run(xelatex, &tex, "log")?;
    }
    run(xelatex, &tex, "log")?;
    Ok(work.join(format!("{stem}.pdf")))
}

/// The lines of `log` that say why a run failed: from TeX's first error,
/// a line starting with `!`, 15 of them; with none (BibTeX's, say), its
/// last 15.
pub fn failure(log: &str) -> String {
    const LINES: usize = 15;
    let lines: Vec<&str> = log.lines().collect();
    let from = lines
        .iter()
        .position(|line| line.starts_with('!'))
        .unwrap_or(lines.len().saturating_sub(LINES));
    lines[from..]
        .iter()
        .take(LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// XeLaTeX, or `None` where it isn't installed (CI installs it, so there a
/// missing one fails rather than skips).
pub fn xelatex() -> Option<PathBuf> {
    let found = which::which("xelatex").ok();
    if found.is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "xelatex isn't installed, and CI compiles the benchmark with it"
        );
        eprintln!("skipping: xelatex not found");
    }
    found
}
