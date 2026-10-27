//! The benchmark's test documents (`bench/`) match their list: every
//! directory has an entry in `documents.toml` with a clear licence, and
//! every entry a directory and a main file. Each compiles with XeLaTeX,
//! as its PDF is the answer key the benchmark scores against (an ignored
//! test, run on its own).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(serde::Deserialize)]
struct Manifest {
    document: Vec<Document>,
}

#[derive(serde::Deserialize)]
struct Document {
    dir: String,
    main: String,
    source: String,
    license: String,
    fetched: String,
    covers: Vec<String>,
}

/// The only licences a document may have: CC BY or CC0 for arXiv papers,
/// the repository's own for ones written for the benchmark.
const LICENSES: &[&str] = &[
    "http://creativecommons.org/licenses/by/4.0/",
    "http://creativecommons.org/publicdomain/zero/1.0/",
    "GPL-3.0-or-later",
];

const COVERS: &[&str] = &[
    "math",
    "macros",
    "tables",
    "two-column",
    "multi-file",
    "bibliography",
    "cjk",
];

fn bench() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench")
}

fn manifest() -> Manifest {
    let text = std::fs::read_to_string(bench().join("documents.toml")).expect("documents.toml");
    toml::from_str(&text).expect("documents.toml parses")
}

#[test]
fn every_document_is_listed_with_a_clear_license() {
    let manifest = manifest();
    let listed: BTreeSet<_> = manifest.document.iter().map(|d| d.dir.clone()).collect();
    assert_eq!(
        listed.len(),
        manifest.document.len(),
        "a directory is listed twice"
    );

    let on_disk: BTreeSet<_> = std::fs::read_dir(bench().join("documents"))
        .expect("bench/documents")
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(on_disk, listed, "bench/documents and documents.toml differ");

    for doc in &manifest.document {
        let main = bench().join("documents").join(&doc.dir).join(&doc.main);
        assert!(main.is_file(), "{}: no main file {}", doc.dir, doc.main);
        assert!(
            LICENSES.contains(&doc.license.as_str()),
            "{}: licence {} isn't CC BY, CC0 or the repository's",
            doc.dir,
            doc.license
        );
        let arxiv = doc.source.starts_with("https://arxiv.org/abs/");
        assert!(
            arxiv || doc.source == "written for this benchmark",
            "{}: unknown source {}",
            doc.dir,
            doc.source
        );
        // Only the repository's own documents are under its licence.
        assert_eq!(arxiv, doc.license != "GPL-3.0-or-later", "{}", doc.dir);
        assert!(!doc.fetched.is_empty(), "{}: no fetch date", doc.dir);
        for cover in &doc.covers {
            assert!(
                COVERS.contains(&cover.as_str()),
                "{}: covers {cover}?",
                doc.dir
            );
        }
    }
}

#[test]
fn the_documents_cover_what_the_benchmark_measures() {
    let manifest = manifest();
    for cover in COVERS {
        let count = manifest
            .document
            .iter()
            .filter(|d| d.covers.iter().any(|c| c == cover))
            .count();
        assert!(count >= 2, "only {count} document(s) cover {cover}");
    }
}

/// Compile `doc` in a copy under `scratch`: XeLaTeX twice, so references
/// resolve, with BibTeX between where the document asks for one (a
/// `\bibdata` in its `.aux`) and the paper came without a `.bbl`.
/// The PDF, or why there's none: the run that failed and what its log
/// says of it (see [`failure`]).
fn compile(doc: &Document, xelatex: &Path, scratch: &Path) -> Result<PathBuf, String> {
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
fn failure(log: &str) -> String {
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

#[test]
fn a_failure_is_told_from_its_first_error() {
    let mut log: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    log[9] = "! Undefined control sequence.".into();
    log[29] = "! Emergency stop.".into();
    let told = failure(&log.join("\n"));
    assert_eq!(told, log[9..24].join("\n"));
    // With no error, the end.
    let log: Vec<String> = (1..=40).map(|n| format!("line {n}")).collect();
    assert_eq!(failure(&log.join("\n")), log[25..].join("\n"));
    assert_eq!(failure("short"), "short");
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
fn xelatex() -> Option<PathBuf> {
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

/// Slow, and needs TeX Live packages the Tests job doesn't install
/// (REVTeX, physics), so it runs on its own:
/// `cargo test --test bench_documents -- --ignored`.
#[test]
#[ignore = "compiles every benchmark document; run with --ignored"]
fn every_document_compiles_with_xelatex() {
    let Some(xelatex) = xelatex() else { return };
    let manifest = manifest();
    let scratch = tempfile::tempdir().unwrap();
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(doc) = manifest
                    .document
                    .get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    match compile(doc, &xelatex, scratch.path()) {
                        Ok(pdf) if pdf.is_file() => {}
                        Ok(pdf) => failures.lock().unwrap().push(format!(
                            "{}: no {}",
                            doc.dir,
                            pdf.display()
                        )),
                        Err(why) => failures.lock().unwrap().push(format!("{}: {why}", doc.dir)),
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} documents don't compile:\n\n{}",
        failures.len(),
        manifest.document.len(),
        failures.join("\n\n")
    );
}
