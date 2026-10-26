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
    /// Light or dark windows, or the system's (see [`Appearance`]).
    #[serde(default, deserialize_with = "appearance_or_system")]
    pub appearance: Appearance,
}

/// How the windows look: as the system does (the default), light or dark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    /// The theme windows are given: none for the system's.
    pub fn theme(self) -> Option<tauri::Theme> {
        match self {
            Appearance::System => None,
            Appearance::Light => Some(tauri::Theme::Light),
            Appearance::Dark => Some(tauri::Theme::Dark),
        }
    }
}

/// The appearance a config holds: what it names, or the system's where it
/// names none this version knows (a newer version's, say) or isn't a name,
/// so the rest of the config is kept.
fn appearance_or_system<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Appearance, D::Error> {
    let value = toml::Value::deserialize(deserializer)?;
    Ok(match value.as_str() {
        Some("light") => Appearance::Light,
        Some("dark") => Appearance::Dark,
        _ => Appearance::System,
    })
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
            clipboard_output_mode: default_clipboard_output_mode(),
            quick_bar_shortcut: default_quick_bar_shortcut(),
            recent_folders: default_recent_folders(),
            appearance: Appearance::System,
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
    parse(&text)
}

/// The config `text` holds. Settings it doesn't know, an older version's
/// (`use_local_llm`, `local_llm_*`), are skipped and the rest kept; one it
/// can't read at all gives the defaults.
fn parse(text: &str) -> Config {
    sanitize(toml::from_str(text).unwrap_or_default())
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

    /// Regression for #131: a config.toml saved with the local LLM settings
    /// still loads, those settings skipped and every other one kept.
    #[test]
    fn a_config_with_the_removed_llm_settings_keeps_the_rest() {
        let cfg = parse(
            "output_dir = \"/out\"\njpeg_quality = 80\nuse_marker_pdf = true\n\
             use_local_llm = true\nlocal_llm_model = \"gemma4:e2b\"\n\
             local_llm_url = \"http://localhost:11434\"\n\
             clipboard_output_mode = \"download\"\nquick_bar_shortcut = \"Alt+KeyK\"\n\
             recent_folders = [\"/a\"]\n",
        );
        assert_eq!(cfg.output_dir.as_deref(), Some("/out"));
        assert_eq!(cfg.jpeg_quality, 80);
        assert!(cfg.use_marker_pdf);
        assert_eq!(cfg.clipboard_output_mode, "download");
        assert_eq!(cfg.quick_bar_shortcut, "Alt+KeyK");
        assert_eq!(cfg.recent_folders, ["/a"]);
        // Saved again, they're gone.
        let saved = toml::to_string_pretty(&cfg).unwrap();
        assert!(!saved.contains("llm"), "{saved}");
    }

    /// #152: a config without an appearance, or with one this version
    /// doesn't know or that isn't a name, follows the system and keeps the
    /// rest; one it knows is saved and read back.
    #[test]
    fn appearance_falls_back_to_the_system_and_round_trips() {
        for text in [
            "jpeg_quality = 80\n",
            "jpeg_quality = 80\nappearance = \"sepia\"\n",
            "jpeg_quality = 80\nappearance = 3\n",
            "jpeg_quality = 80\nappearance = \"system\"\n",
        ] {
            let cfg = parse(text);
            assert_eq!(cfg.appearance, Appearance::System, "{text}");
            assert_eq!(cfg.jpeg_quality, 80, "{text}");
        }
        for (name, appearance) in [("light", Appearance::Light), ("dark", Appearance::Dark)] {
            let cfg = parse(&format!("appearance = \"{name}\"\n"));
            assert_eq!(cfg.appearance, appearance);
            let saved = toml::to_string_pretty(&cfg).unwrap();
            assert!(
                saved.contains(&format!("appearance = \"{name}\"")),
                "{saved}"
            );
            assert_eq!(parse(&saved).appearance, appearance);
        }
        // As Settings sends it.
        let sent = serde_json::to_value(Config {
            appearance: Appearance::Dark,
            ..Config::default()
        })
        .unwrap();
        let back: Config = serde_json::from_value(sent).unwrap();
        assert_eq!(back.appearance, Appearance::Dark);
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
