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

use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

/// Which converter performs a single (from,to) hop. Mirrors the dispatch in
/// `converter::run_single_hop`. `route_hop` returns `Some` iff the pair is
/// handled — making it the shared coverage check for the property test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hop {
    ImageRaster,    // image -> pdf (document::convert_image_to_pdf)
    ImageToAvif,
    ImageToHeic,
    ImageToGif,     // media::convert_image_to_gif
    ImageGeneric,   // image::convert_image
    HeicInput,      // image::convert_heic (heic input, non-pdf target)
    Media,          // media::convert_media
    Data,           // data::convert_data
    EpubToMobi,
    MobiInput,      // document::convert_mobi
    PdfToMobi,
    PdfToHtml,
    PdfToMd,
    PdfToEpubOrMarker,
    DocumentPandoc, // document::convert_document (md/txt/tex/typst, epub non-mobi)
}

/// Classify a single hop. Inputs MUST already be normalized.
pub fn route_hop(from: &str, to: &str) -> Option<Hop> {
    use Hop::*;
    match from {
        "png" | "jpg" | "webp" | "bmp" | "tiff" | "gif" => match to {
            "pdf" => Some(ImageRaster),
            "avif" => Some(ImageToAvif),
            "heic" => Some(ImageToHeic),
            "gif" => Some(ImageToGif),
            "png" | "jpg" | "webp" | "bmp" | "tiff" => Some(ImageGeneric),
            _ => None,
        },
        "avif" => match to {
            "pdf" => Some(ImageRaster),
            "heic" => Some(ImageToHeic),
            "gif" => Some(ImageToGif),
            "png" | "jpg" | "webp" | "bmp" | "tiff" => Some(ImageGeneric),
            _ => None,
        },
        "heic" => match to {
            "pdf" => Some(ImageRaster),
            "jpg" | "png" | "tiff" | "gif" | "bmp" => Some(HeicInput),
            _ => None,
        },
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "mp3" | "aac" | "flac" | "ogg" | "wav"
        | "opus" | "m4a" => Some(Media),
        "json" | "yaml" | "toml" | "csv" => Some(Data),
        "mobi" => Some(MobiInput),
        "epub" => {
            if to == "mobi" {
                Some(EpubToMobi)
            } else {
                Some(DocumentPandoc)
            }
        }
        "pdf" => match to {
            "mobi" => Some(PdfToMobi),
            "html" => Some(PdfToHtml),
            "md" => Some(PdfToMd),
            "epub" => Some(PdfToEpubOrMarker),
            _ => None,
        },
        "md" | "txt" | "tex" | "typst" => Some(DocumentPandoc),
        _ => None,
    }
}

/// Maximum number of hops (edges) in a chain.
pub const MAX_HOPS: usize = 6;

/// Dijkstra from `from` over platform-satisfiable edges. Returns, for every
/// reachable node, the min-cost path (as a node sequence including endpoints)
/// whose length is within `MAX_HOPS`.
pub fn shortest_paths(from: &str) -> HashMap<String, Vec<String>> {
    let from = normalize_ext(from).to_string();

    // adjacency: node -> Vec<(neighbor, cost)>
    let mut adj: HashMap<&'static str, Vec<(&'static str, u32)>> = HashMap::new();
    for e in edges() {
        if e.cond.satisfied() {
            adj.entry(e.from).or_default().push((e.to, e.cost));
        }
    }

    let mut dist: HashMap<String, u32> = HashMap::new();
    let mut prev: HashMap<String, String> = HashMap::new();
    dist.insert(from.clone(), 0);

    // min-heap on (Reverse(cost), node)
    let mut heap: BinaryHeap<(std::cmp::Reverse<u32>, String)> = BinaryHeap::new();
    heap.push((std::cmp::Reverse(0), from.clone()));

    while let Some((std::cmp::Reverse(d), node)) = heap.pop() {
        if d > *dist.get(&node).unwrap_or(&u32::MAX) {
            continue;
        }
        if let Some(neighbors) = adj.get(node.as_str()) {
            for &(nbr, cost) in neighbors {
                let nd = d + cost;
                if nd < *dist.get(nbr).unwrap_or(&u32::MAX) {
                    dist.insert(nbr.to_string(), nd);
                    prev.insert(nbr.to_string(), node.clone());
                    heap.push((std::cmp::Reverse(nd), nbr.to_string()));
                }
            }
        }
    }

    // reconstruct paths, dropping any that exceed MAX_HOPS or are semantically blocked
    let mut paths: HashMap<String, Vec<String>> = HashMap::new();
    for target in dist.keys() {
        if *target == from {
            continue;
        }
        if is_semantically_blocked(&from, target) {
            continue;
        }
        let mut seq = vec![target.clone()];
        let mut cur = target.clone();
        while let Some(p) = prev.get(&cur) {
            seq.push(p.clone());
            cur = p.clone();
        }
        seq.reverse();
        if seq.len() - 1 <= MAX_HOPS {
            paths.insert(target.clone(), seq);
        }
    }
    paths
}

/// Semantic dead-ends: (from, to) pairs that the graph structurally cannot
/// offer even if a multi-hop path exists through intermediate nodes.
/// Reason: CSV is an array-of-records; TOML has no top-level array syntax,
/// so the conversion would always fail at runtime regardless of the route.
fn is_semantically_blocked(from: &str, to: &str) -> bool {
    matches!((from, to), ("csv", "toml"))
}

/// Min-cost path of normalized format nodes from `from_ext` to `target`,
/// including both endpoints. `None` if unreachable, beyond `MAX_HOPS`, or
/// semantically blocked (e.g. csv→toml has no valid TOML representation).
pub fn find_path(from_ext: &str, target: &str) -> Option<Vec<String>> {
    let target = normalize_ext(target).to_string();
    let from = normalize_ext(from_ext).to_string();
    if from == target {
        return None;
    }
    if is_semantically_blocked(&from, &target) {
        return None;
    }
    shortest_paths(&from).remove(&target)
}

#[derive(serde::Serialize, Debug)]
pub struct ChainedTarget {
    pub format: String,
    /// Full node sequence including source and target, e.g. ["png","pdf","epub","mobi"].
    pub route: Vec<String>,
    /// Number of hops (edges) = route.len() - 1.
    pub hops: usize,
}

#[derive(serde::Serialize, Debug)]
pub struct DetectResult {
    pub direct: Vec<String>,
    pub chained: Vec<ChainedTarget>,
}

/// Direct + multi-hop reachable targets for a file path. Errors if the input
/// extension has no outgoing edges at all (unsupported type).
pub fn detect_with_chains(path: &str) -> Result<DetectResult, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let from = normalize_ext(&ext);

    let direct = direct_targets(&ext);
    if direct.is_empty() {
        return Err(format!("Unsupported file type: .{ext}"));
    }

    let mut chained: Vec<ChainedTarget> = shortest_paths(from)
        .into_iter()
        .filter(|(fmt, seq)| seq.len() > 2 && !direct.contains(fmt))
        .map(|(fmt, seq)| ChainedTarget {
            hops: seq.len() - 1,
            route: seq,
            format: fmt,
        })
        .collect();
    // Stable, predictable order for the UI: fewest hops first, then name.
    chained.sort_by(|a, b| a.hops.cmp(&b.hops).then_with(|| a.format.cmp(&b.format)));

    Ok(DetectResult { direct, chained })
}
