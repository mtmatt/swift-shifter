use tauri::Manager;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Returns the platform-appropriate toggle shortcut.
/// macOS: ⌘⇧Space — Windows/Linux: Ctrl+Shift+Space
/// Win+Shift+Space is OS-reserved on Windows 11 (IME switcher) and cannot be registered.
fn make_shortcut() -> Shortcut {
    #[cfg(target_os = "macos")]
    return Shortcut::new(Some(Modifiers::META | Modifiers::SHIFT), Code::Space);
    #[cfg(not(target_os = "macos"))]
    return Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
}

pub fn build_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let shortcut = make_shortcut();
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(move |app, s, event| {
            if s == &shortcut && event.state() == ShortcutState::Pressed {
                if let Some(window) = app.get_webview_window("main") {
                    if window.is_visible().unwrap_or(false) {
                        window.hide().unwrap_or_default();
                    } else {
                        window.show().unwrap_or_default();
                        window.set_focus().unwrap_or_default();
                    }
                }
            }
        })
        .build()
}

pub fn register_shortcut(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    app.global_shortcut()
        .register(make_shortcut())
        .map_err(|e| format!("Failed to register hotkey: {e}"))?;
    Ok(())
}
