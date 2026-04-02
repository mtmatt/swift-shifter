use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Where to save converted files. None = same directory as input.
    #[serde(default)]
    pub output_dir: Option<String>,
    /// JPEG output quality (1–100).
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
    /// AVIF output quality (1–100).
    #[serde(default = "default_avif_quality")]
    pub avif_quality: u8,
    /// Maximum files to convert concurrently.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// Use marker-pdf (ML-based) for PDF → EPUB conversion when installed.
    /// Falls back to pdftohtml if marker is not found on PATH.
    #[serde(default)]
    pub use_marker_pdf: bool,
}

fn default_jpeg_quality() -> u8 { 75 }
fn default_avif_quality() -> u8 { 65 }
fn default_max_concurrent() -> usize { 4 }

impl Default for Config {
    fn default() -> Self {
        Self {
            output_dir: None,
            jpeg_quality:   default_jpeg_quality(),
            avif_quality:   default_avif_quality(),
            max_concurrent: default_max_concurrent(),
            use_marker_pdf: false,
        }
    }
}

pub struct AppState {
    pub config: std::sync::Mutex<Config>,
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("swift-shifter").join("config.toml"))
}

pub fn load() -> Config {
    let Some(path) = config_path() else {
        return Config::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    let mut cfg: Config = toml::from_str(&text).unwrap_or_default();
    // Clamp in case config was manually edited out of range
    cfg.jpeg_quality = cfg.jpeg_quality.clamp(1, 100);
    cfg.avif_quality = cfg.avif_quality.clamp(1, 100);
    cfg.max_concurrent = cfg.max_concurrent.clamp(1, 8);
    cfg
}

pub fn save(config: &Config) -> Result<(), String> {
    let path = config_path().ok_or("Cannot determine config directory")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}
