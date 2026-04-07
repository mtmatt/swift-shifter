pub mod data;
pub mod document;
pub mod image;
pub mod media;

use std::path::Path;

use crate::config::Config;

/// Return the list of valid output format strings for a given input file path.
pub fn detect_output_formats(path: &str) -> Result<Vec<String>, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let formats: &[&str] = match ext.as_str() {
        // Images
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tiff" | "tif" | "gif" | "avif" => &[
            "png", "jpg", "webp", "avif", "gif", "bmp", "tiff", "heic", "pdf",
        ],
        // HEIC — macOS sips only; no WebP/AVIF output via sips
        "heic" | "heif" => &["jpg", "png", "tiff", "gif", "bmp", "pdf"],
        // Video
        "mp4" | "mov" | "mkv" | "webm" | "avi" => &["mp4", "mov", "mkv", "webm", "avi", "gif"],
        // Audio
        "mp3" | "aac" | "flac" | "ogg" | "wav" | "opus" => {
            &["mp3", "aac", "flac", "ogg", "wav", "opus"]
        }
        // Data
        "json" => &["yaml", "toml", "csv"],
        "yaml" | "yml" => &["json", "toml", "csv"],
        "toml" => &["json", "yaml", "csv"],
        "csv" => &["json", "yaml", "toml"],
        // Documents (pandoc)
        "md" | "markdown" => &["txt", "html", "pdf", "tex", "typst"],
        "txt" => &["md", "html", "pdf", "tex", "typst"],
        "tex" | "latex" => &["md", "html", "pdf", "typst"],
        "typst" => &["md", "html", "pdf", "tex"],
        "epub" => &["pdf", "mobi", "md", "html"],
        "mobi" => &["epub", "pdf", "html", "md"],
        "pdf" => &["epub", "mobi", "html", "md"],
        _ => return Err(format!("Unsupported file type: .{ext}")),
    };

    let is_jpeg_input = ext == "jpg" || ext == "jpeg";
    Ok(formats
        .iter()
        .filter(|&&f| {
            f != ext.as_str() && !(is_jpeg_input && (f == "jpg" || f == "jpeg"))
        })
        .map(|s| s.to_string())
        .collect())
}

pub async fn convert_file(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
    config: &Config,
) -> Result<String, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let out_dir = config.output_dir.as_deref();

    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tiff" | "tif" | "gif" => {
            if target_format == "pdf" {
                document::convert_image_to_pdf(app, path, out_dir).await
            } else if target_format == "avif" {
                image::convert_to_avif(path, config.avif_quality, out_dir)
            } else if target_format == "heic" {
                image::convert_to_heic(path, out_dir)
            } else {
                image::convert_image(path, target_format, config.jpeg_quality, out_dir)
            }
        }
        "avif" => {
            if target_format == "pdf" {
                document::convert_image_to_pdf(app, path, out_dir).await
            } else if target_format == "heic" {
                image::convert_to_heic(path, out_dir)
            } else {
                image::convert_image(path, target_format, config.jpeg_quality, out_dir)
            }
        }
        "heic" | "heif" => {
            if target_format == "pdf" {
                document::convert_image_to_pdf(app, path, out_dir).await
            } else {
                image::convert_heic(path, target_format, out_dir)
            }
        }
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "mp3" | "aac" | "flac" | "ogg" | "wav"
        | "opus" => media::convert_media(app, path, target_format, out_dir).await,
        "json" | "yaml" | "yml" | "toml" | "csv" => {
            data::convert_data(path, target_format, out_dir)
        }
        "mobi" => document::convert_mobi(app, path, target_format, out_dir).await,
        "epub" => match target_format {
            "mobi" => document::convert_epub_to_mobi(app, path, out_dir).await,
            _ => document::convert_document(app, path, target_format, out_dir).await,
        },
        "pdf" => {
            let llm = document::LlmCfg {
                enabled: config.use_local_llm,
                model:   config.local_llm_model.clone(),
                url:     config.local_llm_url.clone(),
            };
            match target_format {
                "mobi" => document::convert_pdf_to_mobi(app, path, out_dir, config.use_marker_pdf, llm).await,
                "html" => document::convert_pdf_to_html(app, path, out_dir).await,
                "md" => document::convert_pdf_to_md(app, path, out_dir, config.use_marker_pdf, llm).await,
                _ => {
                    if config.use_marker_pdf && document::marker_available() {
                        document::convert_pdf_with_marker(app, path, out_dir, llm).await
                    } else {
                        document::convert_pdf_to_epub(app, path, out_dir, llm).await
                    }
                }
            }
        },
        "md" | "markdown" | "txt" | "tex" | "latex" | "typst" => {
            document::convert_document(app, path, target_format, out_dir).await
        }
        _ => Err(format!("Unsupported input format: .{ext}")),
    }
}
mod tests;
