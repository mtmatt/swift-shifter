// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cli;
mod clipboard;
mod config;
mod converter;
mod downloader;
mod hotkey;
mod process;
mod quick;
mod recent;
#[cfg(test)]
mod test_tools;
mod tray;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_updater::UpdaterExt;

use config::AppState;

#[derive(Serialize, Clone)]
struct UpdateInfo {
    version: String,
    body: Option<String>,
}

async fn check_for_update_and_emit(app: &tauri::AppHandle) {
    let Ok(updater) = app.updater_builder().build() else {
        return;
    };
    let Ok(Some(update)) = updater.check().await else {
        return;
    };
    app.emit(
        "update:available",
        UpdateInfo {
            version: update.version.clone(),
            body: update.body.clone(),
        },
    )
    .ok();
}

#[derive(Deserialize)]
struct BatchItem {
    path: String,
    #[serde(rename = "targetFormat")]
    target_format: String,
}

#[derive(Deserialize)]
struct TrimItem {
    path: String,
    start: String,
    end: String,
}

#[derive(Serialize)]
struct BatchResult {
    path: String,
    output_path: Option<String>,
    error: Option<String>,
    /// What the conversion left out of its output.
    warnings: Vec<converter::document::native::Warning>,
}

impl BatchResult {
    /// What became of `path`: the file it made and what that left out, or
    /// why it failed.
    fn new(path: String, result: Result<converter::Converted, String>) -> Self {
        let (output_path, error, warnings) = match result {
            Ok(done) => (Some(done.output), None, done.warnings),
            Err(e) => (None, Some(e), Vec::new()),
        };
        BatchResult {
            path,
            output_path,
            error,
            warnings,
        }
    }
}

fn main() {
    let context = tauri::generate_context!();
    if cli::is_cli_invocation() {
        std::process::exit(cli::run(context));
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(hotkey::build_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Load persisted config and register managed state
            let cfg = config::load();
            let shortcut = cfg.quick_bar_shortcut.clone();
            app.manage(AppState {
                config: std::sync::Mutex::new(cfg),
            });

            quick::create(app)?;
            tray::setup_tray(app)?;
            if let Err(e) = hotkey::register(app.handle(), &shortcut) {
                eprintln!("Hotkey registration failed (non-fatal): {e}");
            }

            // Native macOS menu bar
            let about = PredefinedMenuItem::about(app, None::<&str>, None)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let prefs = MenuItem::with_id(
                app,
                "preferences",
                "Preferences…",
                true,
                Some("CmdOrCtrl+,"),
            )?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let quit = PredefinedMenuItem::quit(app, None::<&str>)?;
            let app_menu = Submenu::with_items(
                app,
                "Swift Shifter",
                true,
                &[&about, &sep1, &prefs, &sep2, &quit],
            )?;
            let menu = Menu::with_items(app, &[&app_menu])?;
            app.set_menu(menu)?;

            let menu_handle = app.handle().clone();
            app.on_menu_event(move |_app, event| {
                if event.id() == "preferences" {
                    tray::open_or_focus_settings(&menu_handle);
                }
            });

            // Check for ffmpeg at startup
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = converter::media::ensure_ffmpeg(&handle).await {
                    eprintln!("ffmpeg setup warning: {e}");
                    handle.emit("ffmpeg:failed", e).ok();
                }
            });

            // Check for pandoc at startup
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = converter::document::ensure_pandoc(&handle).await {
                    eprintln!("pandoc setup warning: {e}");
                    handle.emit("pandoc:failed", e).ok();
                }
            });

            // PDFs are typeset in-process; scan fonts now so the first
            // conversion doesn't wait for it.
            converter::document::native::typst::warm_up();

            // Check for pymupdf4llm at startup — needed for PDF → EPUB/HTML/MD
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = converter::document::ensure_pymupdf4llm(&handle).await {
                    eprintln!("pymupdf4llm setup warning: {e}");
                    handle.emit("pymupdf:failed", e).ok();
                }
            });

            // Check for ebook-convert (Calibre) at startup — needed for MOBI conversion
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = converter::document::ensure_ebook_convert(&handle).await {
                    eprintln!("ebook-convert setup warning: {e}");
                    handle.emit("ebook-convert:failed", e).ok();
                }
            });

            // Check for app updates in the background
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                check_for_update_and_emit(&handle).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Hide the main drop-zone window instead of closing it so the
            // app stays resident in the tray. Other windows (settings)
            // close normally and are recreated on next open.
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                window.hide().unwrap_or_default();
                api.prevent_close();
            }
            WindowEvent::Focused(false) if window.label() == quick::LABEL => {
                quick::hide_on_blur(window);
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            clipboard::paste_from_clipboard,
            clipboard::paste_diagnostics,
            clipboard::copy_file_to_clipboard,
            clipboard::remove_temp_file,
            detect_format,
            recent_files,
            default_recent_folders,
            validate_shortcut,
            quick::hide_quick_bar,
            convert,
            convert_batch,
            trim_media,
            trim_batch,
            get_media_duration,
            merge_pdfs,
            get_config,
            set_config,
            check_marker,
            install_marker,
            check_ebook_convert,
            open_output_folder,
            check_update,
            install_update,
            quit,
        ])
        .build(context)
        .expect("error while building tauri application")
        // Used on macOS alone.
        .run(|_app_handle, event| match event {
            // Clicking the Dock icon. Without this, a hidden main window stays
            // hidden and the click appears to do nothing.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => tray::show_main_window(_app_handle),
            _ => {}
        });
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn check_update(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo {
        version: u.version.clone(),
        body: u.body.clone(),
    }))
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let handle = app.clone();
    update
        .download_and_install(
            move |downloaded, total| {
                let percent = total
                    .map(|t| downloaded as f32 / t as f32 * 100.0)
                    .unwrap_or(0.0);
                handle.emit("update:progress", percent).ok();
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

#[tauri::command]
async fn detect_format(path: String) -> Result<converter::graph::DetectResult, String> {
    converter::graph::detect_with_chains(&path)
}

/// Convertible files from the configured folders, newest first, for the
/// quick bar.
#[tauri::command]
async fn recent_files(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<recent::RecentFile>, String> {
    let roots: Vec<std::path::PathBuf> = state
        .config
        .lock()
        .unwrap()
        .recent_folders
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    tokio::task::spawn_blocking(move || recent::scan(&roots, recent::MAX_RESULTS))
        .await
        .map_err(|e| format!("task panicked: {e}"))
}

/// Check a shortcut recorded in Settings before it is saved, so an unusable
/// key is reported right away instead of on Save.
#[tauri::command]
fn validate_shortcut(accelerator: String) -> Result<(), String> {
    hotkey::parse_shortcut(&accelerator).map(|_| ())
}

/// The folders the quick bar scans out of the box, for Settings' "restore
/// defaults".
#[tauri::command]
fn default_recent_folders() -> Vec<String> {
    config::default_recent_folders()
}

#[tauri::command]
async fn convert(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
    target_format: String,
) -> Result<converter::Converted, String> {
    // Clone releases the lock before the await — never hold std::sync::Mutex across await
    let cfg = state.config.lock().unwrap().clone();
    converter::convert_file(&app_handle, &path, &target_format, &cfg).await
}

/// Convert multiple files concurrently, capped by `config.max_concurrent`.
/// Progress events are emitted per-file via the existing "convert:progress" channel.
#[tauri::command]
async fn convert_batch(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    items: Vec<BatchItem>,
) -> Result<Vec<BatchResult>, String> {
    use tokio::sync::Semaphore;

    let cfg = Arc::new(state.config.lock().unwrap().clone());
    let sem = Arc::new(Semaphore::new(cfg.max_concurrent));

    let handles: Vec<_> = items
        .into_iter()
        .map(|item| {
            let sem = Arc::clone(&sem);
            let cfg = Arc::clone(&cfg);
            let handle = app_handle.clone();
            tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result =
                    converter::convert_file(&handle, &item.path, &item.target_format, &cfg).await;
                BatchResult::new(item.path, result)
            })
        })
        .collect();

    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(e) => results.push(BatchResult::new(
                String::new(),
                Err(format!("task panicked: {e}")),
            )),
        }
    }
    Ok(results)
}

#[tauri::command]
async fn get_media_duration(path: String) -> Result<f64, String> {
    converter::media::media_duration_secs(&path).await
}

#[tauri::command]
async fn trim_media(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
    start: String,
    end: String,
) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    converter::media::trim_media(&app_handle, &path, &start, &end, cfg.output_dir.as_deref()).await
}

#[tauri::command]
async fn trim_batch(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    items: Vec<TrimItem>,
) -> Result<Vec<BatchResult>, String> {
    use tokio::sync::Semaphore;

    let cfg = Arc::new(state.config.lock().unwrap().clone());
    let sem = Arc::new(Semaphore::new(cfg.max_concurrent));

    let handles: Vec<_> = items
        .into_iter()
        .map(|item| {
            let sem = Arc::clone(&sem);
            let cfg = Arc::clone(&cfg);
            let handle = app_handle.clone();
            tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = converter::media::trim_media(
                    &handle,
                    &item.path,
                    &item.start,
                    &item.end,
                    cfg.output_dir.as_deref(),
                )
                .await;
                BatchResult::new(item.path, result.map(converter::Converted::whole))
            })
        })
        .collect();

    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(e) => results.push(BatchResult::new(
                String::new(),
                Err(format!("task panicked: {e}")),
            )),
        }
    }
    Ok(results)
}

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> config::Config {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
fn set_config(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    new_config: config::Config,
) -> Result<(), String> {
    let validated = config::Config {
        output_dir: new_config.output_dir,
        jpeg_quality: new_config.jpeg_quality.clamp(1, 100),
        avif_quality: new_config.avif_quality.clamp(1, 100),
        max_concurrent: new_config.max_concurrent.clamp(1, 8),
        use_marker_pdf: new_config.use_marker_pdf,
        clipboard_output_mode: match new_config.clipboard_output_mode.as_str() {
            "download" => "download".to_string(),
            _ => "clipboard".to_string(),
        },
        quick_bar_shortcut: new_config.quick_bar_shortcut.trim().to_string(),
        recent_folders: config::normalize_folders(new_config.recent_folders),
    };

    // Swap the live shortcut first: if the OS refuses it, nothing is saved.
    let old_shortcut = state.config.lock().unwrap().quick_bar_shortcut.clone();
    let shortcut_changed = validated.quick_bar_shortcut != old_shortcut;
    if shortcut_changed {
        hotkey::replace(&app, &old_shortcut, &validated.quick_bar_shortcut)?;
    }
    if let Err(e) = config::save(&validated) {
        if shortcut_changed
            && let Err(rollback_err) =
                hotkey::replace(&app, &validated.quick_bar_shortcut, &old_shortcut)
        {
            eprintln!("Couldn't restore shortcut {old_shortcut}: {rollback_err}");
        }
        return Err(e);
    }
    *state.config.lock().unwrap() = validated;
    app.emit("config:updated", ()).ok();
    Ok(())
}

#[tauri::command]
fn check_marker() -> bool {
    converter::document::marker_available()
}

#[tauri::command]
async fn install_marker(app: tauri::AppHandle) -> Result<(), String> {
    converter::document::install_marker(&app).await
}

#[tauri::command]
fn check_ebook_convert() -> bool {
    converter::document::ebook_convert_available()
}

#[tauri::command]
async fn open_output_folder(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    let dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        p.parent().ok_or("No parent directory")?.to_path_buf()
    };
    #[cfg(target_os = "macos")]
    crate::process::sync_command("open")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    crate::process::sync_command("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    crate::process::sync_command("xdg-open")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn merge_pdfs(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    tokio::task::spawn_blocking(move || converter::merge_pdfs(&paths, cfg.output_dir.as_deref()))
        .await
        .map_err(|e| format!("task panicked: {e}"))?
}

#[cfg(test)]
mod window_config_tests {
    use serde_json::Value;

    fn main_window(config: &str) -> Value {
        let config: Value = serde_json::from_str(config).expect("valid JSON");
        config["app"]["windows"]
            .as_array()
            .expect("app.windows is an array")
            .iter()
            .find(|w| w["label"] == "main")
            .expect("a window labelled main")
            .clone()
    }

    #[test]
    fn main_window_uses_native_decorations_without_transparency() {
        // Frameless + transparent rendered blank on Wayland (#26) and looked
        // broken on Windows; every platform now gets a native title bar.
        let main = main_window(include_str!("../tauri.conf.json"));
        assert_eq!(main["decorations"], true);
        assert_eq!(main["transparent"], false);
    }

    #[test]
    fn main_window_is_a_regular_window() {
        // The quick bar is the floating surface now; the main window opens
        // centred like any app window and doesn't stay on top.
        let main = main_window(include_str!("../tauri.conf.json"));
        assert_eq!(main["alwaysOnTop"], false);
        assert_eq!(main["center"], true);
    }

    #[test]
    fn macos_override_keeps_traffic_lights_over_content() {
        let main = main_window(include_str!("../tauri.macos.conf.json"));
        assert_eq!(main["decorations"], true);
        assert_eq!(main["transparent"], false);
        assert_eq!(main["titleBarStyle"], "Overlay");
        assert_eq!(main["hiddenTitle"], true);
    }

    #[test]
    fn macos_override_matches_base_window_apart_from_title_bar() {
        // Tauri replaces the whole `windows` array when merging a platform
        // file, so every other field has to be repeated there verbatim.
        let mut base = main_window(include_str!("../tauri.conf.json"));
        let mut mac = main_window(include_str!("../tauri.macos.conf.json"));
        for key in ["titleBarStyle", "hiddenTitle"] {
            base.as_object_mut().unwrap().remove(key);
            mac.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(base, mac);
    }

    #[test]
    fn macos_tray_uses_a_template_glyph_on_transparency() {
        // macOS draws template images from alpha alone, so the opaque app
        // icon shows as a blank square there; it gets a glyph instead.
        let mac: Value = serde_json::from_str(include_str!("../tauri.macos.conf.json")).unwrap();
        assert_eq!(
            mac["app"]["trayIcon"]["iconPath"],
            "icons/tray-template.png"
        );
        assert_eq!(mac["app"]["trayIcon"]["iconAsTemplate"], true);

        let icon = image::load_from_memory(include_bytes!("../icons/tray-template.png"))
            .expect("tray-template.png decodes")
            .to_rgba8();
        assert_eq!(icon.width(), icon.height());
        let alphas: Vec<u8> = icon.pixels().map(|px| px[3]).collect();
        assert!(alphas.contains(&0), "background must be transparent");
        assert!(alphas.contains(&255), "glyph must be opaque");
    }

    #[test]
    fn other_platforms_keep_the_colour_tray_icon() {
        let base: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(base["app"]["trayIcon"]["iconPath"], "icons/icon.png");
        assert_eq!(base["app"]["trayIcon"]["iconAsTemplate"], false);
    }
}

#[cfg(test)]
mod batch_tests {
    use super::BatchResult;
    use crate::converter::Converted;
    use crate::converter::document::native::Warning;
    use serde_json::json;

    /// The UI gets each file's output and what it left out, or its error.
    #[test]
    fn batch_results_say_what_became_of_each_file() {
        let lost = Warning {
            one: "thing was lost",
            many: "things were lost",
            count: 1,
            line: None,
        };
        let converted = Converted {
            output: "/out/a.html".into(),
            warnings: vec![lost],
        };
        let done = BatchResult::new("/in/a.md".into(), Ok(converted));
        assert_eq!(
            serde_json::to_value(done).unwrap(),
            json!({
                "path": "/in/a.md",
                "output_path": "/out/a.html",
                "error": null,
                "warnings": [{ "message": "1 thing was lost", "count": 1, "line": null }],
            })
        );
        let failed = BatchResult::new("/in/b.md".into(), Err("no".into()));
        assert_eq!(
            serde_json::to_value(failed).unwrap(),
            json!({ "path": "/in/b.md", "output_path": null, "error": "no", "warnings": [] })
        );
    }
}
