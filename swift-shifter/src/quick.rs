//! The quick bar: a wide, frameless search bar summoned by the global hotkey
//! or a tray click, as opposed to the main window opened from the Dock/Start
//! menu. It is built hidden at startup so summoning it is instant.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{
    LogicalSize, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
};

pub const LABEL: &str = "quick";

/// Logical size of the bar.
const WIDTH: f64 = 640.0;
const HEIGHT: f64 = 56.0;

/// How far down the monitor the bar's top edge sits, as a fraction of the
/// monitor's height (roughly where Spotlight puts its field).
const TOP_FRACTION: f64 = 0.2;

/// Clicking the tray icon while the bar is open first takes focus from the
/// bar, which hides it, and only then delivers the click that toggles it. A
/// toggle this soon after a blur-hide is that same click and must not re-show
/// the bar.
const SAME_CLICK_WINDOW: Duration = Duration::from_millis(300);

/// When the bar last hid itself because it lost focus.
static LAST_HIDDEN_ON_BLUR: Mutex<Option<Instant>> = Mutex::new(None);

pub fn create(app: &tauri::App) -> tauri::Result<()> {
    let builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("quick.html".into()))
        .title("Swift Shifter")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        // Show on the current Space/workspace rather than the one the window
        // was created on.
        .visible_on_all_workspaces(true)
        .visible(false);
    // Rounded corners come from CSS over a transparent window. Only macOS
    // gets that: transparent windows rendered blank on Wayland (#26).
    #[cfg(target_os = "macos")]
    let builder = builder.transparent(true);
    builder.build()?;
    Ok(())
}

/// Hide the bar if it is showing, otherwise show it on the monitor under the
/// cursor and focus it.
pub fn toggle(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        window.hide().unwrap_or_default();
        return;
    }
    let hidden_on_blur_at = LAST_HIDDEN_ON_BLUR.lock().unwrap().take();
    if is_same_click(hidden_on_blur_at, Instant::now()) {
        return;
    }
    if let Some(monitor) = monitor_under_cursor(app) {
        let bar_size = LogicalSize::new(WIDTH, HEIGHT).to_physical(monitor.scale_factor());
        let origin = bar_origin(*monitor.position(), *monitor.size(), bar_size);
        window.set_position(origin).unwrap_or_default();
    }
    window.show().unwrap_or_default();
    window.set_focus().unwrap_or_default();
}

/// Like Spotlight, the bar goes away when the user clicks elsewhere.
pub fn hide_on_blur(window: &tauri::Window) {
    *LAST_HIDDEN_ON_BLUR.lock().unwrap() = Some(Instant::now());
    window.hide().unwrap_or_default();
}

/// Hide the bar on request (Esc). On macOS, also hand focus back to the
/// previous app unless another of our windows is showing; hiding the window
/// alone would leave Swift Shifter frontmost with nothing to type into.
#[tauri::command]
pub fn hide_quick_bar(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.hide().unwrap_or_default();
    }
    #[cfg(target_os = "macos")]
    if !app
        .webview_windows()
        .values()
        .any(|w| w.is_visible().unwrap_or(false))
    {
        app.hide().unwrap_or_default();
    }
}

fn is_same_click(hidden_on_blur_at: Option<Instant>, now: Instant) -> bool {
    hidden_on_blur_at.is_some_and(|at| now.duration_since(at) < SAME_CLICK_WINDOW)
}

fn monitor_under_cursor(app: &tauri::AppHandle) -> Option<tauri::Monitor> {
    let cursor = app.cursor_position().ok()?;
    match app.monitor_from_point(cursor.x, cursor.y) {
        Ok(Some(monitor)) => Some(monitor),
        _ => app.primary_monitor().ok().flatten(),
    }
}

/// Top-left corner that centres the bar horizontally on a monitor and puts
/// its top edge `TOP_FRACTION` of the way down. All values are physical pixels.
fn bar_origin(
    monitor_origin: PhysicalPosition<i32>,
    monitor_size: PhysicalSize<u32>,
    bar_size: PhysicalSize<u32>,
) -> PhysicalPosition<i32> {
    let spare_width = monitor_size.width.saturating_sub(bar_size.width);
    let top_offset = (f64::from(monitor_size.height) * TOP_FRACTION) as i32;
    PhysicalPosition::new(
        monitor_origin.x + (spare_width / 2) as i32,
        monitor_origin.y + top_offset,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_right_after_a_blur_hide_is_the_same_click() {
        let hidden = Instant::now();
        assert!(is_same_click(
            Some(hidden),
            hidden + Duration::from_millis(50)
        ));
        assert!(!is_same_click(
            Some(hidden),
            hidden + Duration::from_secs(1)
        ));
        assert!(!is_same_click(None, hidden));
    }

    #[test]
    fn bar_is_centred_a_fifth_of_the_way_down() {
        let origin = bar_origin(
            PhysicalPosition::new(0, 0),
            PhysicalSize::new(2000, 1000),
            PhysicalSize::new(1280, 112),
        );
        assert_eq!(origin, PhysicalPosition::new(360, 200));
    }

    #[test]
    fn bar_origin_is_relative_to_a_secondary_monitor() {
        // A monitor to the left of and above the primary has negative coordinates.
        let origin = bar_origin(
            PhysicalPosition::new(-1920, -200),
            PhysicalSize::new(1920, 1080),
            PhysicalSize::new(640, 56),
        );
        assert_eq!(origin, PhysicalPosition::new(-1280, 16));
    }

    #[test]
    fn bar_wider_than_monitor_pins_to_its_left_edge() {
        let origin = bar_origin(
            PhysicalPosition::new(100, 0),
            PhysicalSize::new(500, 800),
            PhysicalSize::new(640, 56),
        );
        assert_eq!(origin, PhysicalPosition::new(100, 160));
    }
}
