//! The benchmark's test documents (`bench/`) match their list: every
//! directory has an entry in `documents.toml` with a clear licence, and
//! every entry a directory and a main file.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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
