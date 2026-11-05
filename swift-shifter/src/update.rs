//! New versions of the app: asking GitHub for one, and installing it where
//! that's safe. Elsewhere the UI links to the release page instead.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Serialize;
use tauri::Emitter;
use tauri::utils::config::BundleType;
use tauri::utils::platform::{bundle_type, current_exe};
use tauri_plugin_updater::UpdaterExt;

const RELEASES: &str = "https://github.com/mtmatt/swift-shifter/releases";

/// What's running, as installing an update ends the app and with it any
/// conversion: one excludes the other.
struct Running {
    conversions: usize,
    update: bool,
}

static RUNNING: Mutex<Running> = Mutex::new(Running {
    conversions: 0,
    update: false,
});

fn running() -> MutexGuard<'static, Running> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Held while a conversion runs.
pub struct Working;

impl Working {
    /// Refused while an update installs.
    pub fn start() -> Result<Self, String> {
        let mut running = running();
        if running.update {
            return Err("An update is installing".to_string());
        }
        running.conversions += 1;
        Ok(Working)
    }
}

impl Drop for Working {
    fn drop(&mut self) {
        running().conversions -= 1;
    }
}

/// Held while an update downloads and installs.
struct Updating;

impl Updating {
    /// Refused while a conversion runs, or another update installs.
    fn start() -> Result<Self, String> {
        let mut running = running();
        if running.conversions > 0 {
            return Err("Wait for the conversions to finish".to_string());
        }
        if running.update {
            return Err("An update is installing".to_string());
        }
        running.update = true;
        Ok(Updating)
    }
}

impl Drop for Updating {
    fn drop(&mut self) {
        running().update = false;
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct UpdateInfo {
    version: String,
    /// Whether `install_update` installs it; otherwise it's a download from
    /// `release_url`.
    can_install: bool,
    release_url: String,
}

impl UpdateInfo {
    fn new(version: String, can_install: bool) -> Self {
        let release_url = format!("{RELEASES}/tag/v{version}");
        UpdateInfo {
            version,
            can_install,
            release_url,
        }
    }
}

/// Whether an app running as `exe` from `bundle` updates itself safely.
/// Linux doesn't: nothing yet sees that `latest.json` gives an install its
/// own package, and the updater writes any package over an AppImage
/// unchecked. A plain binary (`cargo binstall`) has no bundle to replace;
/// on macOS `bundle_type` says `App` for one too.
fn installs_in_place(bundle: Option<BundleType>, exe: &Path) -> bool {
    match bundle {
        Some(BundleType::Msi | BundleType::Nsis) => true,
        Some(BundleType::App) => exe
            .ancestors()
            .any(|dir| dir.extension().is_some_and(|ext| ext == "app")),
        _ => false,
    }
}

fn can_install() -> bool {
    current_exe().is_ok_and(|exe| installs_in_place(bundle_type(), &exe))
}

/// How far `done` bytes are through `total`, as a percentage.
fn percent(done: usize, total: Option<u64>) -> f32 {
    match total {
        Some(total) if total > 0 => done as f32 / total as f32 * 100.0,
        _ => 0.0,
    }
}

#[tauri::command]
pub async fn check_update(app: tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|update| UpdateInfo::new(update.version, can_install())))
}

/// Downloads and installs the new version, then restarts, reporting how far
/// the download is with `update:progress`.
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    if !can_install() {
        return Err("This copy can't update itself: download the new version".to_string());
    }
    let _updating = Updating::start()?;
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("No new version found")?;
    let handle = app.clone();
    let mut done = 0;
    update
        .download_and_install(
            move |chunk, total| {
                done += chunk;
                handle.emit("update:progress", percent(done, total)).ok();
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_macos_apps_and_windows_installers_update_in_place() {
        let cargo_bin = Path::new("/home/a/.cargo/bin/swift-shifter");
        let app = Path::new("/Applications/Swift Shifter.app/Contents/MacOS/swift-shifter");
        assert!(installs_in_place(Some(BundleType::Msi), cargo_bin));
        assert!(installs_in_place(Some(BundleType::Nsis), cargo_bin));
        assert!(installs_in_place(Some(BundleType::App), app));
        assert!(!installs_in_place(Some(BundleType::App), cargo_bin));
        for bundle in [
            Some(BundleType::Deb),
            Some(BundleType::Rpm),
            Some(BundleType::AppImage),
            None,
        ] {
            let name = format!("{bundle:?}");
            assert!(!installs_in_place(bundle, cargo_bin), "{name}");
        }
    }

    #[test]
    fn the_release_page_is_the_versions_own() {
        assert_eq!(
            UpdateInfo::new("2.0.0".to_string(), false).release_url,
            "https://github.com/mtmatt/swift-shifter/releases/tag/v2.0.0"
        );
    }

    #[test]
    fn progress_is_of_the_whole_download() {
        assert_eq!(percent(50, Some(200)), 25.0);
        assert_eq!(percent(50, Some(0)), 0.0);
        assert_eq!(percent(50, None), 0.0);
    }

    #[test]
    fn conversions_and_an_update_exclude_each_other() {
        // Conversions keep an update out until the last ends.
        let (first, second) = (Working::start().unwrap(), Working::start().unwrap());
        assert!(Updating::start().is_err());
        drop(first);
        assert!(Updating::start().is_err());
        drop(second);
        // An update keeps conversions and other updates out, until it fails.
        let updating = Updating::start().unwrap();
        assert!(Working::start().is_err());
        assert!(Updating::start().is_err());
        drop(updating);
        drop(Working::start().unwrap());
    }
}
