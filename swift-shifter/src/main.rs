// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod converter;
mod hotkey;
mod tray;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Manager, WindowEvent};

use config::AppState;

#[derive(Deserialize)]
struct BatchItem {
    path: String,
    #[serde(rename = "targetFormat")]
    target_format: String,
}

#[derive(Serialize)]
struct BatchResult {
    path: String,
    output_path: Option<String>,
    error: Option<String>,
}

fn main() {
    tauri::Builder::default()
        .plugin(hotkey::build_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Load persisted config and register managed state
            let cfg = config::load();
            app.manage(AppState {
                config: std::sync::Mutex::new(cfg),
            });

            tray::setup_tray(app)?;
            hotkey::register_shortcut(app)?;

            // Native macOS menu bar
            let about = PredefinedMenuItem::about(app, None::<&str>, None)?;
            let sep1 = PredefinedMenuItem::separator(app)?;
            let prefs = MenuItem::with_id(app, "preferences", "Preferences…", true, Some("CmdOrCtrl+,"))?;
            let sep2 = PredefinedMenuItem::separator(app)?;
            let quit = PredefinedMenuItem::quit(app, None::<&str>)?;
            let app_menu = Submenu::with_items(app, "Swift Shifter", true, &[&about, &sep1, &prefs, &sep2, &quit])?;
            let menu = Menu::with_items(app, &[&app_menu])?;
            app.set_menu(menu)?;

            let menu_handle = app.handle().clone();
            app.on_menu_event(move |_app, event| {
                if event.id() == "preferences" {
                    // Focus existing settings window if already open
                    if let Some(win) = menu_handle.get_webview_window("settings") {
                        let _ = win.set_focus();
                        return;
                    }
                    let _ = tauri::WebviewWindowBuilder::new(
                        &menu_handle,
                        "settings",
                        tauri::WebviewUrl::App("settings.html".into()),
                    )
                    .title("Preferences")
                    .inner_size(360.0, 300.0)
                    .resizable(false)
                    .center()
                    .build();
                }
            });

            // Check for ffmpeg at startup
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = converter::media::ensure_ffmpeg(&handle).await {
                    eprintln!("ffmpeg setup warning: {e}");
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Hide instead of close so the app stays in tray
                window.hide().unwrap_or_default();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            detect_format,
            convert,
            convert_batch,
            get_config,
            set_config,
            open_output_folder,
            quit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn detect_format(path: String) -> Result<Vec<String>, String> {
    converter::detect_output_formats(&path)
}

#[tauri::command]
async fn convert(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    path: String,
    target_format: String,
) -> Result<String, String> {
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
                match converter::convert_file(&handle, &item.path, &item.target_format, &cfg).await
                {
                    Ok(out) => BatchResult {
                        path: item.path,
                        output_path: Some(out),
                        error: None,
                    },
                    Err(e) => BatchResult {
                        path: item.path,
                        output_path: None,
                        error: Some(e),
                    },
                }
            })
        })
        .collect();

    let mut results = Vec::with_capacity(handles.len());
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(e) => results.push(BatchResult {
                path: String::new(),
                output_path: None,
                error: Some(format!("task panicked: {e}")),
            }),
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
    state: tauri::State<'_, AppState>,
    new_config: config::Config,
) -> Result<(), String> {
    let validated = config::Config {
        output_dir: new_config.output_dir,
        jpeg_quality: new_config.jpeg_quality.clamp(1, 100),
        avif_quality: new_config.avif_quality.clamp(1, 100),
        max_concurrent: new_config.max_concurrent.clamp(1, 8),
    };
    config::save(&validated)?;
    *state.config.lock().unwrap() = validated;
    Ok(())
}

#[tauri::command]
async fn open_output_folder(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    let dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        p.parent()
            .ok_or("No parent directory")?
            .to_path_buf()
    };
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(&dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
