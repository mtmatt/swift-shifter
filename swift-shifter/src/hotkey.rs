use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::quick;

/// Toggles the quick bar: Ctrl+Shift+Space on every platform (Control, not
/// Cmd, on macOS too). Win+Shift+Space would be unusable anyway: Windows 11
/// reserves it for the IME switcher.
fn make_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space)
}

pub fn build_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    let shortcut = make_shortcut();
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(move |app, s, event| {
            if s == &shortcut && event.state() == ShortcutState::Pressed {
                quick::toggle(app);
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
