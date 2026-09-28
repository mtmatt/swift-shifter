//! End-to-end test of image -> PDF through the real CLI (pandoc + typst).
//! Skipped when pandoc or typst is unavailable.

mod common;

use common::{missing_tools, write_tiny_png};
use std::process::Command;

/// A relative input path with spaces and parentheses in the file name, no
/// `--output-dir`: the PDF must land next to the image. pandoc runs from a
/// staging dir, so this covers both the relative-path and escaping cases.
#[test]
fn image_to_pdf_handles_relative_path_and_awkward_name() {
    let missing = missing_tools(&["pandoc", "typst"]);
    if !missing.is_empty() {
        eprintln!("SKIP: image->pdf e2e; missing tools: {missing:?}");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    write_tiny_png(&dir.path().join("my pic (1).png"));

    let out = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .current_dir(dir.path())
        .args(["convert", "pdf", "my pic (1).png"])
        .output()
        .expect("failed to run convert");
    assert!(
        out.status.success(),
        "convert failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let pdf = dir.path().join("my pic (1).pdf");
    let bytes = std::fs::read(&pdf).expect("PDF missing next to the image");
    assert!(bytes.starts_with(b"%PDF"), "not a PDF: {pdf:?}");
}
