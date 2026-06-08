//! End-to-end test of multi-hop chaining through the real CLI.
//! Skipped when the tools the chain needs are unavailable (e.g. CI without
//! pandoc / typst / Calibre).

use std::process::Command;

/// True if `doctor` reports the named tool with a leading "✓".
fn tool_present(doctor_stdout: &str, name: &str) -> bool {
    doctor_stdout
        .lines()
        .any(|l| l.starts_with('\u{2713}') && l.contains(name))
}

#[test]
fn png_to_mobi_chain_produces_file() {
    // 1. Ask the CLI which tools exist.
    let doctor = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .arg("doctor")
        .output()
        .expect("failed to run doctor");
    let report = String::from_utf8_lossy(&doctor.stdout);

    // png -> pdf needs pandoc + typst; pdf -> mobi needs Calibre (ebook-convert).
    let needed = ["pandoc", "typst", "ebook-convert"];
    let missing: Vec<&str> = needed
        .iter()
        .copied()
        .filter(|t| !tool_present(&report, t))
        .collect();
    if !missing.is_empty() {
        eprintln!(
            "SKIP: png->mobi chain e2e; missing tools: {missing:?}"
        );
        return;
    }

    // 2. Make a tiny real PNG in a temp dir.
    let dir = tempfile::tempdir().expect("tempdir");
    let png = dir.path().join("tiny.png");
    write_tiny_png(&png);

    // 3. Run the chain via the CLI: `convert mobi tiny.png`, output into the temp dir.
    let out = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .args([
            "--output-dir",
            dir.path().to_str().unwrap(),
            "convert",
            "mobi",
            png.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run convert");

    assert!(
        out.status.success(),
        "convert failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // 4. The CLI prints the output path on success; it must exist and be non-empty.
    let printed = String::from_utf8_lossy(&out.stdout);
    let produced = printed.trim().lines().last().unwrap_or("").trim();
    assert!(!produced.is_empty(), "no output path printed");
    let meta = std::fs::metadata(produced).expect("output file missing");
    assert!(meta.len() > 0, "output file is empty");
}

/// Write a minimal valid 1x1 PNG.
fn write_tiny_png(path: &std::path::Path) {
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    std::fs::write(path, PNG).expect("write png");
}
