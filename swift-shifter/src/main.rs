// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod converter;
mod hotkey;
mod tray;
mod util;

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

#[derive(Serialize)]
struct BatchResult {
    path: String,
    output_path: Option<String>,
    error: Option<String>,
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(hotkey::build_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Load persisted config and register managed state
            let cfg = config::load();
            app.manage(AppState {
                config: std::sync::Mutex::new(cfg),
                ollama_process: std::sync::Mutex::new(None),
            });

            tray::setup_tray(app)?;
            if let Err(e) = hotkey::register_shortcut(app) {
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

            // Check for Ollama reachability at startup
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tauri::Manager;
                let cfg = handle.state::<AppState>().config.lock().unwrap().clone();
                let url = cfg.local_llm_url.clone();
                let model = cfg.local_llm_model.clone();
                
                if cfg.use_local_llm {
                    // Try to start Ollama if it's not reachable
                    if !converter::document::ollama_reachable(&url).await {
                        if let Ok(Some(child)) = converter::document::install_ollama_and_model(&handle, &url, &model).await {
                            *handle.state::<AppState>().ollama_process.lock().unwrap() = Some(child);
                        }
                    }
                }

                let ok = converter::document::ollama_reachable(&url).await;
                handle.emit("ollama:status", ok).ok();
            });

            // Check for app updates in the background
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                check_for_update_and_emit(&handle).await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Only intercept close on the main drop-zone window so it
                // stays resident in the tray.  The settings window is allowed
                // to close normally; it will be recreated on next open.
                if window.label() == "main" {
                    window.hide().unwrap_or_default();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            detect_format,
            convert,
            convert_batch,
            merge_pdfs,
            get_config,
            set_config,
            check_marker,
            install_marker,
            check_ebook_convert,
            check_ollama,
            check_ollama_url,
            list_ollama_models,
            install_ollama_and_model,
            open_output_folder,
            check_update,
            install_update,
            quit,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| match event {
            tauri::RunEvent::ExitRequested { .. } => {
                if let Some(mut child) = _app_handle.state::<AppState>().ollama_process.lock().unwrap().take() {
                    let _ = child.start_kill();
                }
            }
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
fn set_config(state: tauri::State<'_, AppState>, new_config: config::Config) -> Result<(), String> {
    // Validate local_llm_url is a well-formed HTTP/HTTPS URL
    let llm_url = new_config.local_llm_url.trim().to_string();
    if !llm_url.is_empty()
        && !llm_url.starts_with("http://")
        && !llm_url.starts_with("https://")
    {
        return Err(format!(
            "Invalid local LLM URL '{}': must start with http:// or https://",
            llm_url
        ));
    }

    let validated = config::Config {
        output_dir: new_config.output_dir,
        jpeg_quality: new_config.jpeg_quality.clamp(1, 100),
        avif_quality: new_config.avif_quality.clamp(1, 100),
        max_concurrent: new_config.max_concurrent.clamp(1, 8),
        use_marker_pdf: new_config.use_marker_pdf,
        use_local_llm:  new_config.use_local_llm,
        local_llm_model: new_config.local_llm_model,
        local_llm_url: llm_url,
    };
    config::save(&validated)?;
    *state.config.lock().unwrap() = validated;
    Ok(())
}

#[tauri::command]
async fn check_ollama(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    let url = state.config.lock().unwrap().local_llm_url.clone();
    Ok(converter::document::ollama_reachable(&url).await)
}

#[tauri::command]
async fn check_ollama_url(url: String) -> Result<bool, String> {
    Ok(converter::document::ollama_reachable(&url).await)
}

#[tauri::command]
async fn list_ollama_models(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    let url = state.config.lock().unwrap().local_llm_url.clone();
    Ok(converter::document::ollama_list_models(&url).await)
}

#[tauri::command]
async fn install_ollama_and_model(app: tauri::AppHandle, state: tauri::State<'_, AppState>, base_url: String, model: String) -> Result<(), String> {
    match converter::document::install_ollama_and_model(&app, &base_url, &model).await {
        Ok(Some(child)) => {
            *state.ollama_process.lock().unwrap() = Some(child);
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    }
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

#[tauri::command]
async fn merge_pdfs(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
) -> Result<String, String> {
    let cfg = state.config.lock().unwrap().clone();
    tokio::task::spawn_blocking(move || {
        converter::merge_pdfs(&paths, cfg.output_dir.as_deref())
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))?
}
