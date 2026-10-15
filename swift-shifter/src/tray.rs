use tauri::{
    Manager,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconEvent},
};

use crate::quick;

/// Id Tauri gives the tray it creates from `app.trayIcon` in tauri.conf.json.
const CONFIG_TRAY_ID: &str = "main";

/// Show and focus the main window. Closing it only hides it, so this is how
/// it comes back.
pub fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        window.show().unwrap_or_default();
        window.set_focus().unwrap_or_default();
    }
}

/// Show the settings window, focusing it if it is already open.
pub fn open_or_focus_settings(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    let _ = tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("settings.html".into()),
    )
    .title("Preferences")
    .inner_size(420.0, 440.0)
    .resizable(false)
    .always_on_top(true)
    .center()
    .build();
}

pub fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "show", "Show Swift Shifter", true, None::<&str>)?;
    let prefs = MenuItem::with_id(app, "preferences", "Preferences…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &prefs, &quit])?;

    // Tauri already built the tray (icon, template mode, tooltip) from the
    // config. Attach the menu and handlers to it instead of building a second
    // tray: that one would have no icon, which fails to build on Linux.
    let tray = app
        .tray_by_id(CONFIG_TRAY_ID)
        .ok_or("tray icon from tauri.conf.json `app.trayIcon` is missing")?;
    tray.set_menu(Some(menu))?;
    tray.on_menu_event(|app, event| match event.id.as_ref() {
        "show" => show_main_window(app),
        "preferences" => open_or_focus_settings(app),
        "quit" => {
            app.exit(0);
        }
        _ => {}
    });
    tray.on_tray_icon_event(|tray, event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            quick::toggle(tray.app_handle());
        }
    });

    Ok(())
}
