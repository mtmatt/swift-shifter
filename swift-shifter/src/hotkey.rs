use std::str::FromStr;

use tauri_plugin_global_shortcut::{GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::quick;

/// Default quick-bar shortcut: Ctrl+Shift+Space on every platform (Control,
/// not Cmd, on macOS too). Win+Shift+Space would be unusable anyway: Windows
/// 11 reserves it for the IME switcher.
pub const DEFAULT_SHORTCUT: &str = "Control+Shift+Space";

/// Parse an accelerator such as "Control+Shift+Space" or "Alt+KeyK". It needs
/// Control, Alt or Super: with no modifier, or Shift alone, the shortcut would
/// swallow ordinary typing (a key, a capital letter, Shift+Tab) system-wide.
pub fn parse_shortcut(accelerator: &str) -> Result<Shortcut, String> {
    // The parser's own errors are terse and point at an upstream issue
    // tracker, so they aren't shown to the user.
    let shortcut = Shortcut::from_str(accelerator)
        .map_err(|_| format!("'{accelerator}' can't be used as a global shortcut"))?;
    if !shortcut
        .mods
        .intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER)
    {
        return Err(format!(
            "'{accelerator}' needs Ctrl, Alt or Cmd/Win, not just Shift"
        ));
    }
    Ok(shortcut)
}

/// The quick-bar shortcut is the only one the app registers, so any press
/// toggles the bar.
pub fn build_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                quick::toggle(app);
            }
        })
        .build()
}

pub fn register(app: &tauri::AppHandle, accelerator: &str) -> Result<(), String> {
    app.global_shortcut()
        .register(parse_shortcut(accelerator)?)
        .map_err(|e| format!("Couldn't register {accelerator}: {e}"))
}

/// Swap the registered shortcut from `old` to `new`. If `new` can't be
/// registered (e.g. another app or the OS already owns it), `old` is put back
/// and the error returned, so the bar always stays reachable.
pub fn replace(app: &tauri::AppHandle, old: &str, new: &str) -> Result<(), String> {
    let new_shortcut = parse_shortcut(new)?;
    let shortcuts = app.global_shortcut();
    let old_shortcut = parse_shortcut(old).ok();
    if let Some(old_shortcut) = old_shortcut {
        // Already unregistered if it failed to register at startup.
        let _ = shortcuts.unregister(old_shortcut);
    }
    if let Err(e) = shortcuts.register(new_shortcut) {
        if let Some(old_shortcut) = old_shortcut
            && let Err(restore_err) = shortcuts.register(old_shortcut)
        {
            eprintln!("Couldn't restore shortcut {old}: {restore_err}");
        }
        return Err(format!("Couldn't register {new}: {e}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_global_shortcut::{Code, Modifiers};

    #[test]
    fn default_shortcut_is_ctrl_shift_space() {
        let shortcut = parse_shortcut(DEFAULT_SHORTCUT).unwrap();
        assert_eq!(shortcut.mods, Modifiers::CONTROL | Modifiers::SHIFT);
        assert_eq!(shortcut.key, Code::Space);
    }

    #[test]
    fn accepts_keyboard_event_code_names() {
        // The settings page records shortcuts as modifiers + KeyboardEvent.code.
        let shortcut = parse_shortcut("Alt+Super+KeyK").unwrap();
        assert_eq!(shortcut.mods, Modifiers::ALT | Modifiers::SUPER);
        assert_eq!(shortcut.key, Code::KeyK);
    }

    #[test]
    fn rejects_a_bare_key() {
        assert!(parse_shortcut("Space").is_err());
    }

    #[test]
    fn rejects_shift_as_the_only_modifier() {
        assert!(
            parse_shortcut("Shift+KeyA")
                .unwrap_err()
                .contains("not just Shift")
        );
        assert!(parse_shortcut("Shift+Tab").is_err());
    }

    #[test]
    fn accepts_shift_alongside_another_modifier() {
        assert!(parse_shortcut("Super+Shift+KeyA").is_ok());
    }

    #[test]
    fn rejects_keys_the_parser_does_not_know() {
        // e.g. the ISO key left of Z, or an empty code from an unmapped key.
        let err = parse_shortcut("Control+IntlBackslash").unwrap_err();
        assert!(err.contains("can't be used"), "{err}");
        assert!(parse_shortcut("Control+").is_err());
        assert!(parse_shortcut("").is_err());
    }
}
