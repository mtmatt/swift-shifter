//! What the benchmark's scorers run: the app's CLI, as a user converts with
//! it, and the tools scoring needs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The CLI, run in `dir`, as `swift-shifter convert <format> <input>`: the
/// file it made and its stderr, or why the conversion failed. An error
/// where the CLI couldn't be run.
pub fn convert(
    dir: &Path,
    format: &str,
    input: &Path,
) -> Result<Result<(PathBuf, String), String>, String> {
    let out = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .args(["convert", format])
        .arg(input)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("swift-shifter: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Ok(Err(format!("convert {format}: {}", stderr.trim())));
    }
    let made = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(Ok((PathBuf::from(made), stderr)))
}

/// Whether the CLI converts at all here, tried on a line of plain text:
/// where its app can't start (no display, no window server), every
/// document would fail for that, not as converted, so nothing is scored.
pub fn cli_converts(scratch: &Path) -> Result<(), String> {
    let probe = scratch.join("probe.txt");
    std::fs::write(&probe, "probe\n").map_err(|e| e.to_string())?;
    match convert(scratch, "md", &probe)? {
        Ok((made, _)) if made.is_file() => Ok(()),
        Ok((made, _)) => Err(format!("the CLI made no {}", made.display())),
        Err(why) => Err(why),
    }
}

/// `name`, a tool scoring needs, or `None` where it isn't installed (CI
/// installs it, so there a missing one fails rather than skips): without
/// it, every conversion would fail for want of it, not as converted.
pub fn required(name: &str) -> Option<PathBuf> {
    let found = which::which(name).ok();
    if found.is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "{name} isn't installed, and CI scores the benchmark with it"
        );
        eprintln!("skipping: {name} not found");
    }
    found
}

/// `pdf`'s text, as `pdftotext` gives it.
pub fn pdf_text(pdf: &Path) -> Result<String, String> {
    let out = Command::new("pdftotext")
        .args(["-enc", "UTF-8"])
        .arg(pdf)
        .arg("-")
        .output()
        .map_err(|e| format!("pdftotext: {e}"))?;
    if !out.status.success() {
        return Err(format!("pdftotext {}: failed", pdf.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
