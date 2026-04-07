use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use regex::Regex;

pub static RE_NUM: OnceLock<Regex> = OnceLock::new();
pub static RE_MERGE: OnceLock<Regex> = OnceLock::new();
pub static RE_STYLE_BLOCK: OnceLock<Regex> = OnceLock::new();
pub static RE_CSS_POS: OnceLock<Regex> = OnceLock::new();
pub static RE_CSS_TOP: OnceLock<Regex> = OnceLock::new();
pub static RE_CSS_LEFT: OnceLock<Regex> = OnceLock::new();
pub static RE_CSS_HEIGHT: OnceLock<Regex> = OnceLock::new();
pub static RE_SVG_SRC: OnceLock<Regex> = OnceLock::new();

pub fn output_path(input: &str, ext: &str, output_dir: Option<&str>) -> Result<PathBuf, String> {
    let p = Path::new(input);
    let stem = p.file_stem().unwrap_or_default();
    let dir = match output_dir {
        Some(d) => {
            let dir = PathBuf::from(d);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create output directory: {e}"))?;
            dir
        }
        None => p.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };
    Ok(dir.join(format!("{}.{}", stem.to_string_lossy(), ext)))
}

/// Recursively copy everything under `src_dir` into `dst_dir`, skipping `skip`.
/// Subdirectories are recreated with the same name so relative paths in the
/// markdown (e.g. `images/_page_4_Figure_7.jpeg`) keep working.
pub fn copy_dir_contents_except(src_dir: &Path, dst_dir: &Path, skip: &Path) {
    let Ok(entries) = std::fs::read_dir(src_dir) else { return };
    for entry in entries.flatten() {
        let src = entry.path();
        if src == skip {
            continue;
        }
        let Some(name) = src.file_name() else { continue };
        let dst = dst_dir.join(name);
        if src.is_dir() {
            let _ = std::fs::create_dir_all(&dst);
            copy_dir_contents_except(&src, &dst, skip);
        } else if src.is_file() {
            let _ = std::fs::copy(&src, &dst);
        }
    }
}

/// Recursively find the first `.md` file under `dir`.
pub fn find_md_file(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_md_file(&path) {
                return Some(found);
            }
        }
    }
    None
}

/// Try to convert an SVG file to PNG using `rsvg-convert` or ImageMagick.
/// Returns the PNG path on success, None if no suitable tool is found.
pub async fn convert_svg_to_png(svg_path: &Path) -> Option<PathBuf> {
    let png_path = svg_path.with_extension("png");

    // Prefer rsvg-convert (brew install librsvg) — lossless, no Ghostscript needed
    if let Ok(rsvg) = which::which("rsvg-convert") {
        let ok = tokio::process::Command::new(&rsvg)
            .args(["-o", png_path.to_str()?, svg_path.to_str()?])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && png_path.exists() {
            return Some(png_path);
        }
    }

    // Fall back to ImageMagick 7 (`magick`) or 6 (`convert`)
    let magick = which::which("magick")
        .or_else(|_| which::which("convert"))
        .ok()?;
    let ok = tokio::process::Command::new(&magick)
        .args([svg_path.to_str()?, png_path.to_str()?])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    if ok && png_path.exists() {
        Some(png_path)
    } else {
        None
    }
}

/// Map a file extension to the pandoc format name used on the command line.
pub fn ext_to_pandoc_format(ext: &str) -> &str {
    match ext {
        "md" | "markdown" => "markdown",
        "txt" => "plain",
        "tex" | "latex" => "latex",
        "typst" => "typst",
        "epub" => "epub",
        "pdf" => "pdf",
        _ => ext,
    }
}

/// Output file extension for a given target format keyword.
pub fn target_to_ext(target: &str) -> &str {
    match target {
        "latex" => "tex",
        _ => target,
    }
}
