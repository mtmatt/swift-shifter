//! Helpers shared by the CLI end-to-end tests.

use std::process::Command;

/// The `needed` tools that `swift-shifter doctor` doesn't report as found
/// (a line starting with "✓"). Tests skip themselves when this is non-empty.
pub fn missing_tools(needed: &[&str]) -> Vec<String> {
    let doctor = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .arg("doctor")
        .output()
        .expect("failed to run doctor");
    let report = String::from_utf8_lossy(&doctor.stdout);
    needed
        .iter()
        .filter(|tool| {
            !report
                .lines()
                .any(|l| l.starts_with('\u{2713}') && l.contains(**tool))
        })
        .map(|tool| tool.to_string())
        .collect()
}

/// Write a minimal valid 1x1 PNG.
pub fn write_tiny_png(path: &std::path::Path) {
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    std::fs::write(path, PNG).expect("write png");
}
