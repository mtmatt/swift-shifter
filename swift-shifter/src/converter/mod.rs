pub mod data;
pub mod document;
pub mod graph;
pub mod image;
pub mod media;

pub use document::merge_pdfs;

use std::path::Path;

use crate::config::Config;

/// Return the list of valid DIRECT output format strings for a given input path.
/// Derived from the edge table in `graph.rs` (single source of truth).
pub fn detect_output_formats(path: &str) -> Result<Vec<String>, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let targets = graph::direct_targets(&ext);
    if targets.is_empty() {
        return Err(format!("Unsupported file type: .{ext}"));
    }
    Ok(targets)
}

/// Execute exactly one conversion hop. `from`/`to` MUST be normalized.
/// `out_dir` is where THIS hop writes (a temp dir for intermediate hops).
async fn run_single_hop(
    app: &tauri::AppHandle,
    path: &str,
    from: &str,
    to: &str,
    out_dir: Option<&str>,
    config: &Config,
) -> Result<String, String> {
    use graph::Hop;
    let hop = graph::route_hop(from, to)
        .ok_or_else(|| format!("No converter handles .{from} → {to}"))?;

    match hop {
        Hop::ImageRaster => document::convert_image_to_pdf(app, path, out_dir).await,
        Hop::ImageToAvif => image::convert_to_avif(path, config.avif_quality, out_dir),
        Hop::ImageToHeic => image::convert_to_heic(path, out_dir),
        Hop::ImageToGif => media::convert_image_to_gif(app, path, out_dir).await,
        Hop::ImageGeneric => image::convert_image(path, to, config.jpeg_quality, out_dir),
        Hop::HeicInput => image::convert_heic(path, to, out_dir),
        Hop::Media => media::convert_media(app, path, to, out_dir).await,
        Hop::Data => data::convert_data(path, to, out_dir),
        Hop::EpubToMobi => document::convert_epub_to_mobi(app, path, out_dir).await,
        Hop::MobiInput => document::convert_mobi(app, path, to, out_dir).await,
        Hop::DocumentPandoc => document::convert_document(app, path, to, out_dir).await,
        Hop::PdfToMobi => {
            let llm = pdf_llm_cfg(config);
            document::convert_pdf_to_mobi(app, path, out_dir, config.use_marker_pdf, llm).await
        }
        Hop::PdfToHtml => document::convert_pdf_to_html(app, path, out_dir).await,
        Hop::PdfToMd => {
            let llm = pdf_llm_cfg(config);
            document::convert_pdf_to_md(app, path, out_dir, config.use_marker_pdf, llm).await
        }
        Hop::PdfToEpubOrMarker => {
            let llm = pdf_llm_cfg(config);
            if config.use_marker_pdf && document::marker_available() {
                document::convert_pdf_with_marker(app, path, out_dir, llm).await
            } else {
                document::convert_pdf_to_epub(app, path, out_dir, llm).await
            }
        }
    }
}

fn pdf_llm_cfg(config: &Config) -> document::LlmCfg {
    document::LlmCfg {
        enabled: config.use_local_llm,
        model: config.local_llm_model.clone(),
        url: config.local_llm_url.clone(),
    }
}

/// Convert `path` to `target_format`, chaining hops through a temp dir when no
/// direct conversion exists. Only the final artifact lands in `config.output_dir`.
pub async fn convert_file(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
    config: &Config,
) -> Result<String, String> {
    use tauri::Emitter;

    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let seq = graph::find_path(&ext, target_format)
        .ok_or_else(|| format!("No conversion path from .{ext} to {target_format}"))?;

    // Direct conversion: behave exactly as before.
    if seq.len() == 2 {
        return run_single_hop(app, path, &seq[0], &seq[1], config.output_dir.as_deref(), config)
            .await;
    }

    // Multi-hop: run through a temp dir; only the last hop writes to output_dir.
    let tmp = tempfile::Builder::new()
        .prefix("swift-shifter-chain-")
        .tempdir()
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_str = tmp.path().to_str().ok_or("Temp path is not valid UTF-8")?;

    let total = seq.len() - 1;
    let mut current = path.to_string();
    for (i, win) in seq.windows(2).enumerate() {
        // Overall progress keyed to the ORIGINAL path (UI ignores temp-path emits).
        let pct = (i as f64 / total as f64) * 100.0;
        app.emit(
            "convert:progress",
            serde_json::json!({ "path": path, "percent": pct }),
        )
        .ok();

        let is_last = i == total - 1;
        let out_dir = if is_last {
            config.output_dir.as_deref()
        } else {
            Some(tmp_str)
        };
        current = run_single_hop(app, &current, &win[0], &win[1], out_dir, config).await?;
    }

    app.emit(
        "convert:progress",
        serde_json::json!({ "path": path, "percent": 100.0 }),
    )
    .ok();

    // `tmp` drops here, removing all intermediates; the final file is in output_dir.
    Ok(current)
}
mod tests;
