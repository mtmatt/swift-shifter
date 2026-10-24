//! Regression for #118: the CLI's `convert` runs in a windowless app, which
//! may not start (no window server, no display). Whether it does or not, the
//! CLI says how it went: the output's path when it worked, why on stderr,
//! with a failing status, when it didn't. It never exits 0 in silence.

use std::process::Command;

#[test]
fn convert_says_how_it_went_wherever_it_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let png = dir.path().join("tiny.png");
    image::RgbImage::new(1, 1).save(&png).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .args(["convert", "pdf", png.to_str().unwrap()])
        .output()
        .expect("failed to run convert");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    if out.status.success() {
        let produced = stdout.trim();
        assert!(
            produced.ends_with(".pdf"),
            "stdout={stdout} stderr={stderr}"
        );
        assert!(std::path::Path::new(produced).is_file(), "{produced}");
    } else {
        assert!(
            !stderr.trim().is_empty(),
            "failed in silence: {:?}",
            out.status
        );
    }
}
