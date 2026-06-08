//! The format conversion graph: the single source of truth for which
//! conversions exist, what they cost, and which converter performs them.
//!
//! `detect_output_formats`, the chain finder, and the per-hop executor all
//! derive from `edges()` / `route_hop()` here — so the "must stay in sync"
//! coupling between detection and conversion lives in exactly one place.

/// Platform / capability gate on an edge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeCond {
    /// Always available.
    Always,
    /// Requires macOS (HEIC via `sips`).
    MacOnly,
}

impl EdgeCond {
    /// Whether this edge is usable on the current platform.
    pub fn satisfied(self) -> bool {
        match self {
            EdgeCond::Always => true,
            EdgeCond::MacOnly => cfg!(target_os = "macos"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub from: &'static str,
    pub to: &'static str,
    // cost is read by the Dijkstra path-finder in Task 3; suppress dead_code until then.
    #[allow(dead_code)]
    pub cost: u32,
    pub cond: EdgeCond,
}

/// Collapse extension aliases to a single canonical key used by the graph.
pub fn normalize_ext(ext: &str) -> &str {
    match ext {
        "jpeg" => "jpg",
        "yml" => "yaml",
        "tif" => "tiff",
        "markdown" => "md",
        "latex" => "tex",
        "heif" => "heic",
        other => other,
    }
}

const IMG_RASTER: &[&str] = &["png", "jpg", "webp", "bmp", "tiff", "gif"];
const VIDEO: &[&str] = &["mp4", "mov", "mkv", "webm", "avi"];
const AUDIO: &[&str] = &["mp3", "aac", "flac", "ogg", "wav", "opus", "m4a"];

/// Build the full edge list. Cheap; called per detection/conversion.
pub fn edges() -> Vec<Edge> {
    use EdgeCond::{Always, MacOnly};
    let mut e: Vec<Edge> = Vec::new();
    // Annotated to pin &'static str (Edge.from/to require it; inference is ambiguous otherwise).
    let mut push = |from: &'static str, to: &'static str, cost: u32, cond: EdgeCond| {
        e.push(Edge { from, to, cost, cond })
    };

    // ---- Raster image inputs (incl. avif) ----
    let img_inputs: &[&str] = &["png", "jpg", "webp", "bmp", "tiff", "gif", "avif"];
    for &from in img_inputs {
        for &to in IMG_RASTER {
            if to != from {
                push(from, to, if to == "jpg" { 4 } else { 2 }, Always);
            }
        }
        if from != "avif" {
            push(from, "avif", 4, Always);
        }
        push(from, "heic", 4, MacOnly);
        push(from, "pdf", 6, Always);
    }

    // ---- HEIC/HEIF input (macOS sips only) ----
    for &to in &["jpg", "png", "tiff", "gif", "bmp"] {
        push("heic", to, if to == "jpg" { 4 } else { 2 }, MacOnly);
    }
    push("heic", "pdf", 6, MacOnly);

    // ---- Video & audio (ffmpeg re-encode) ----
    for &from in VIDEO {
        for &to in &["mp4", "mov", "mkv", "webm", "avi", "gif"] {
            if to != from {
                push(from, to, 4, Always);
            }
        }
    }
    for &from in AUDIO {
        for &to in AUDIO {
            if to != from {
                push(from, to, 4, Always);
            }
        }
    }

    // ---- Data (serde, lossless-ish) ----
    push("json", "yaml", 1, Always);
    push("json", "toml", 1, Always);
    push("json", "csv", 1, Always);
    push("yaml", "json", 1, Always);
    push("yaml", "toml", 1, Always);
    push("yaml", "csv", 1, Always);
    push("toml", "json", 1, Always);
    push("toml", "yaml", 1, Always);
    push("toml", "csv", 1, Always);
    // csv -> toml deliberately absent: CSV is an array, TOML has no top-level array.
    push("csv", "json", 1, Always);
    push("csv", "yaml", 1, Always);

    // ---- Documents & ebooks ----
    // cost rule: render to pdf = 6, extract from pdf = 8, otherwise 4.
    let doc: &[(&str, &[&str])] = &[
        ("md", &["txt", "html", "pdf", "tex", "typst"]),
        ("txt", &["md", "html", "pdf", "tex", "typst"]),
        ("tex", &["md", "html", "pdf", "typst"]),
        ("typst", &["md", "html", "pdf", "tex"]),
        ("epub", &["pdf", "mobi", "md", "html"]),
        ("mobi", &["epub", "pdf", "html", "md"]),
        ("pdf", &["epub", "mobi", "html", "md"]),
    ];
    for &(from, tos) in doc {
        for &to in tos {
            let cost = if to == "pdf" {
                6
            } else if from == "pdf" {
                8
            } else {
                4
            };
            push(from, to, cost, Always);
        }
    }

    e
}

/// Direct (1-hop) target formats for a normalized input extension,
/// filtered to edges usable on this platform. This is the body of
/// `detect_output_formats`.
pub fn direct_targets(from_ext: &str) -> Vec<String> {
    let from = normalize_ext(from_ext);
    edges()
        .into_iter()
        .filter(|e| e.from == from && e.cond.satisfied())
        .map(|e| e.to.to_string())
        .collect()
}
