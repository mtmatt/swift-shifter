use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{hotkey, recent};

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
    /// Falls back to pymupdf4llm if marker is not found on PATH.
    #[serde(default)]
    pub use_marker_pdf: bool,
    /// Use local LLM (Ollama) for post-processing PDF → EPUB/MD output.
    #[serde(default)]
    pub use_local_llm: bool,
    /// Ollama model name (e.g. gemma4:e2b).
    #[serde(default = "default_local_llm_model")]
    pub local_llm_model: String,
    /// Ollama HTTP base URL.
    #[serde(default = "default_local_llm_url")]
    pub local_llm_url: String,
    /// Where clipboard-converted results are sent: "clipboard" (default) or "download" (save to file).
    #[serde(default = "default_clipboard_output_mode")]
    pub clipboard_output_mode: String,
    /// Global shortcut that toggles the quick bar, as an accelerator such as
    /// "Control+Shift+Space" (see `hotkey::parse_shortcut`).
    #[serde(default = "default_quick_bar_shortcut")]
    pub quick_bar_shortcut: String,
    /// Folders the quick bar scans for recently changed files.
    #[serde(default = "default_recent_folders")]
    pub recent_folders: Vec<String>,
}

fn default_jpeg_quality() -> u8 {
    75
}
fn default_avif_quality() -> u8 {
    65
}
fn default_max_concurrent() -> usize {
    4
}
fn default_local_llm_model() -> String {
    "gemma4:e2b".to_string()
}
fn default_local_llm_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_clipboard_output_mode() -> String {
    "clipboard".to_string()
}
fn default_quick_bar_shortcut() -> String {
    hotkey::DEFAULT_SHORTCUT.to_string()
}
/// Desktop, Documents and Downloads, where they exist.
pub fn default_recent_folders() -> Vec<String> {
    recent::default_roots()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Trim folder paths and drop blanks and duplicates, keeping the user's order.
pub fn normalize_folders(folders: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    folders
        .into_iter()
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty() && seen.insert(f.clone()))
        .collect()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_dir: None,
            jpeg_quality: default_jpeg_quality(),
            avif_quality: default_avif_quality(),
            max_concurrent: default_max_concurrent(),
            use_marker_pdf: false,
            use_local_llm: false,
            local_llm_model: default_local_llm_model(),
            local_llm_url: default_local_llm_url(),
            clipboard_output_mode: default_clipboard_output_mode(),
            quick_bar_shortcut: default_quick_bar_shortcut(),
            recent_folders: default_recent_folders(),
        }
    }
}

pub struct AppState {
    pub config: std::sync::Mutex<Config>,
    pub ollama_process: std::sync::Mutex<Option<tokio::process::Child>>,
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
    sanitize(toml::from_str(&text).unwrap_or_default())
}

/// Bring a hand-edited or outdated config back into range.
fn sanitize(mut cfg: Config) -> Config {
    cfg.jpeg_quality = cfg.jpeg_quality.clamp(1, 100);
    cfg.avif_quality = cfg.avif_quality.clamp(1, 100);
    cfg.max_concurrent = cfg.max_concurrent.clamp(1, 8);
    if !["clipboard", "download"].contains(&cfg.clipboard_output_mode.as_str()) {
        cfg.clipboard_output_mode = "clipboard".to_string();
    }
    if hotkey::parse_shortcut(&cfg.quick_bar_shortcut).is_err() {
        cfg.quick_bar_shortcut = default_quick_bar_shortcut();
    }
    cfg.recent_folders = normalize_folders(cfg.recent_folders);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_clipboard_mode_is_clipboard() {
        let cfg = Config::default();
        assert_eq!(cfg.clipboard_output_mode, "clipboard");
    }

    #[test]
    fn config_round_trips_toml() {
        let cfg = Config::default();
        let s = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back.clipboard_output_mode, "clipboard");
    }

    #[test]
    fn config_without_quick_bar_fields_gets_defaults() {
        // A config.toml saved before these settings existed.
        let cfg: Config = toml::from_str("jpeg_quality = 80").unwrap();
        assert_eq!(cfg.quick_bar_shortcut, "Control+Shift+Space");
        assert_eq!(cfg.recent_folders, default_recent_folders());
    }

    #[test]
    fn an_emptied_folder_list_stays_empty() {
        // Removing every folder is a valid choice, not a request for defaults.
        let cfg: Config = toml::from_str("recent_folders = []").unwrap();
        assert!(cfg.recent_folders.is_empty());
    }

    #[test]
    fn normalize_folders_trims_and_dedupes_in_order() {
        let folders = vec![
            " /b ".to_string(),
            "/a".to_string(),
            "".to_string(),
            "/b".to_string(),
            "   ".to_string(),
        ];
        assert_eq!(normalize_folders(folders), ["/b", "/a"]);
    }

    #[test]
    fn sanitize_resets_an_unusable_shortcut_and_tidies_folders() {
        let cfg = Config {
            quick_bar_shortcut: "Shift+KeyA".to_string(),
            recent_folders: vec![" /a ".to_string(), "/a".to_string()],
            ..Config::default()
        };
        let cfg = sanitize(cfg);
        assert_eq!(cfg.quick_bar_shortcut, hotkey::DEFAULT_SHORTCUT);
        assert_eq!(cfg.recent_folders, ["/a"]);
    }

    #[test]
    fn sanitize_keeps_a_valid_custom_shortcut() {
        let cfg = Config {
            quick_bar_shortcut: "Alt+KeyK".to_string(),
            ..Config::default()
        };
        assert_eq!(sanitize(cfg).quick_bar_shortcut, "Alt+KeyK");
    }
}
