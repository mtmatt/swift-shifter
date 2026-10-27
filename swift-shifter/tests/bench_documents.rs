//! The benchmark's test documents (`bench/`) match their list: every
//! directory has an entry in `documents.toml` with a clear licence, and
//! every entry a directory and a main file. Each compiles with XeLaTeX,
//! as its PDF is the answer key the benchmark scores against (an ignored
//! test, run on its own).

// Shared with the other benchmark tests; not all of it is used here.
#[allow(dead_code)]
mod bench;

use std::collections::BTreeSet;

use bench::run::each_document;
use bench::{bench, compile, failure, manifest, xelatex};

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

/// Slow, and needs TeX Live packages the Tests job doesn't install
/// (REVTeX, physics), so it runs on its own:
/// `cargo test --test bench_documents -- --ignored`.
#[test]
#[ignore = "compiles every benchmark document; run with --ignored"]
fn every_document_compiles_with_xelatex() {
    let Some(xelatex) = xelatex() else { return };
    let manifest = manifest();
    let scratch = tempfile::tempdir().unwrap();
    let (_, failures) = each_document(&manifest.document, |doc| {
        match compile(doc, &xelatex, scratch.path())? {
            pdf if pdf.is_file() => Ok(()),
            pdf => Err(format!("no {}", pdf.display())),
        }
    });
    assert!(
        failures.is_empty(),
        "{} of {} documents don't compile:\n\n{}",
        failures.len(),
        manifest.document.len(),
        failures.join("\n\n")
    );
}
