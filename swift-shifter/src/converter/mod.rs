pub mod data;
pub mod image;
pub mod media;

use std::path::Path;

/// Return the list of valid output format strings for a given input file path.
pub fn detect_output_formats(path: &str) -> Result<Vec<String>, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let formats: &[&str] = match ext.as_str() {
        // Images
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tiff" | "tif" | "gif" | "avif" => {
            &["png", "jpg", "webp", "avif", "gif", "bmp", "tiff", "heic"]
        }
        // HEIC — macOS sips only; no WebP/AVIF output via sips
        "heic" | "heif" => &["jpg", "png", "tiff", "gif", "bmp"],
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
        _ => return Err(format!("Unsupported file type: .{ext}")),
    };

    Ok(formats
        .iter()
        .filter(|&&f| f != ext.as_str() && !(ext == "jpg" && f == "jpg"))
        .map(|s| s.to_string())
        .collect())
}

pub async fn convert_file(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
) -> Result<String, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tiff" | "tif" | "gif" => {
            if target_format == "avif" {
                image::convert_to_avif(path)
            } else if target_format == "heic" {
                image::convert_to_heic(path)
            } else {
                image::convert_image(path, target_format)
            }
        }
        "avif" => {
            if target_format == "heic" {
                image::convert_to_heic(path)
            } else {
                image::convert_image(path, target_format)
            }
        }
        "heic" | "heif" => image::convert_heic(path, target_format),
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "mp3" | "aac" | "flac" | "ogg" | "wav"
        | "opus" => media::convert_media(app, path, target_format).await,
        "json" | "yaml" | "yml" | "toml" | "csv" => data::convert_data(path, target_format),
        _ => Err(format!("Unsupported input format: .{ext}")),
    }
}
