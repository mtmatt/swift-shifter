// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod converter;
mod hotkey;
mod tray;

use tauri::WindowEvent;

fn main() {
    tauri::Builder::default()
        .plugin(hotkey::build_plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            tray::setup_tray(app)?;
            hotkey::register_shortcut(app)?;
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
    path: String,
    target_format: String,
) -> Result<String, String> {
    converter::convert_file(&app_handle, &path, &target_format).await
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
