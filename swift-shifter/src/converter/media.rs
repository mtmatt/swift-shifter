use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Emitter;

static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn output_path(input: &str, ext: &str) -> PathBuf {
    let p = Path::new(input);
    let stem = p.file_stem().unwrap_or_default();
    let dir = p.parent().unwrap_or(Path::new("."));
    dir.join(format!("{}.{}", stem.to_string_lossy(), ext))
}

/// Locations that Homebrew uses but macOS GUI apps don't inherit via PATH.
#[cfg(target_os = "macos")]
const EXTRA_SEARCH_PATHS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

fn find_ffmpeg_binary() -> Option<PathBuf> {
    // First try PATH (works in dev / terminal launches)
    if let Ok(p) = which::which("ffmpeg") {
        return Some(p);
    }
    // Then check known Homebrew locations that GUI bundles miss
    #[cfg(target_os = "macos")]
    for dir in EXTRA_SEARCH_PATHS {
        let candidate = PathBuf::from(dir).join("ffmpeg");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub async fn ensure_ffmpeg(app: &tauri::AppHandle) -> Result<(), String> {
    // Check if already cached
    if FFMPEG_PATH.get().is_some() {
        return Ok(());
    }

    // Try to find ffmpeg in PATH and well-known Homebrew locations
    if let Some(path) = find_ffmpeg_binary() {
        FFMPEG_PATH.set(Some(path)).ok();
        return Ok(());
    }

    // ffmpeg not found — notify the user and attempt to install via brew on macOS
    app.emit("ffmpeg:missing", ()).ok();

    #[cfg(target_os = "macos")]
    {
        // Try brew install
        let status = tokio::process::Command::new("brew")
            .args(["install", "ffmpeg"])
            .status()
            .await
            .map_err(|e| format!("brew not available: {e}"))?;

        if status.success() {
            if let Ok(path) = which::which("ffmpeg") {
                FFMPEG_PATH.set(Some(path)).ok();
                app.emit("ffmpeg:installed", ()).ok();
                return Ok(());
            }
        }
    }

    FFMPEG_PATH.set(None).ok();
    Ok(())
}

fn get_ffmpeg() -> Result<PathBuf, String> {
    match FFMPEG_PATH.get() {
        Some(Some(p)) => Ok(p.clone()),
        _ => find_ffmpeg_binary()
            .ok_or_else(|| "ffmpeg not found. Install it with: brew install ffmpeg".to_string()),
    }
}

#[derive(serde::Serialize, Clone)]
struct ProgressPayload {
    path: String,
    percent: f32,
}

pub async fn convert_media(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
) -> Result<String, String> {
    let ffmpeg = get_ffmpeg()?;
    let out = output_path(path, target_format);

    // Get duration for progress reporting
    let duration_secs = get_duration(&ffmpeg, path).await.unwrap_or(0.0);

    let mut cmd = tokio::process::Command::new(&ffmpeg);
    cmd.args(["-y", "-i", path]);

    // Format-specific flags
    match target_format {
        "mp3" => {
            cmd.args(["-codec:a", "libmp3lame", "-q:a", "2"]);
        }
        "aac" => {
            cmd.args(["-codec:a", "aac", "-b:a", "192k"]);
        }
        "flac" => {
            cmd.args(["-codec:a", "flac"]);
        }
        "ogg" => {
            cmd.args(["-codec:a", "libvorbis", "-q:a", "4"]);
        }
        "opus" => {
            cmd.args(["-codec:a", "libopus", "-b:a", "128k"]);
        }
        "wav" => {
            cmd.args(["-codec:a", "pcm_s16le"]);
        }
        "gif" => {
            cmd.args([
                "-vf",
                "fps=15,scale=480:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse",
            ]);
        }
        "webm" => {
            cmd.args(["-codec:v", "libvpx-vp9", "-codec:a", "libopus"]);
        }
        _ => {}
    }

    // Progress reporting via stderr pipe
    cmd.args([
        "-progress",
        "pipe:2",
        "-nostats",
        out.to_str().unwrap_or(""),
    ]);
    cmd.stderr(std::process::Stdio::piped());

    let path_string = path.to_string();
    let app_handle = app.clone();

    let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?;

    // Parse progress from stderr
    if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(val) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = val.trim().parse::<f64>() {
                    if duration_secs > 0.0 {
                        let percent = ((us / 1_000_000.0) / duration_secs * 100.0)
                            .min(100.0) as f32;
                        app_handle
                            .emit(
                                "convert:progress",
                                ProgressPayload {
                                    path: path_string.clone(),
                                    percent,
                                },
                            )
                            .ok();
                    }
                }
            }
        }
    }

    let status = child.wait().await.map_err(|e| format!("ffmpeg wait error: {e}"))?;
    if !status.success() {
        return Err(format!(
            "ffmpeg exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(out.to_string_lossy().to_string())
}

async fn get_duration(ffmpeg: &Path, path: &str) -> Option<f64> {
    let out = tokio::process::Command::new(ffmpeg)
        .args(["-i", path, "-hide_banner"])
        .output()
        .await
        .ok()?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        if let Some(pos) = line.find("Duration:") {
            let rest = &line[pos + 9..];
            let time_str = rest.split(',').next()?.trim();
            let parts: Vec<&str> = time_str.split(':').collect();
            if parts.len() == 3 {
                let h: f64 = parts[0].parse().ok()?;
                let m: f64 = parts[1].parse().ok()?;
                let s: f64 = parts[2].parse().ok()?;
                return Some(h * 3600.0 + m * 60.0 + s);
            }
        }
    }
    None
}
