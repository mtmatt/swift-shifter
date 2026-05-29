use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Emitter;

#[derive(serde::Serialize, Clone)]
struct InstallLogPayload {
    line: String,
    phase: String,
}

/// Run a command, streaming every stdout/stderr line as an `install:log` event.
/// Returns whether the process exited successfully.
async fn run_streamed(
    app: &tauri::AppHandle,
    mut cmd: tokio::process::Command,
    phase: &str,
) -> Result<bool, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    let app2 = app.clone();
    let phase2 = phase.to_string();
    let stderr_task = tokio::spawn(async move {
        while let Ok(Some(line)) = stderr_lines.next_line().await {
            app2.emit(
                "install:log",
                InstallLogPayload {
                    line,
                    phase: phase2.clone(),
                },
            )
            .ok();
        }
    });

    let phase_str = phase.to_string();
    while let Ok(Some(line)) = stdout_lines.next_line().await {
        app.emit(
            "install:log",
            InstallLogPayload {
                line,
                phase: phase_str.clone(),
            },
        )
        .ok();
    }

    stderr_task.await.ok();

    let status = child.wait().await.map_err(|e| format!("wait error: {e}"))?;
    Ok(status.success())
}

static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

fn output_path(input: &str, ext: &str, output_dir: Option<&str>) -> Result<PathBuf, String> {
    let p = Path::new(input);
    let stem = p.file_stem().unwrap_or_default();
    let dir = match output_dir {
        Some(d) => {
            let dir = PathBuf::from(d);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create output directory: {e}"))?;
            dir
        }
        None => p.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };
    Ok(dir.join(format!("{}.{}", stem.to_string_lossy(), ext)))
}

pub fn trim_output_path(input: &str, output_dir: Option<&str>) -> Result<PathBuf, String> {
    let p = Path::new(input);
    let stem = p.file_stem().unwrap_or_default();
    let ext = p.extension().unwrap_or_default().to_str().unwrap_or("");
    let dir = match output_dir {
        Some(d) => {
            let dir = PathBuf::from(d);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create output directory: {e}"))?;
            dir
        }
        None => p.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };
    let file_name = if ext.is_empty() {
        format!("{}-trim", stem.to_string_lossy())
    } else {
        format!("{}-trim.{}", stem.to_string_lossy(), ext)
    };
    Ok(dir.join(file_name))
}

/// Audio containers we can losslessly stream-copy when trimming. Video files
/// are re-encoded instead so the cut lands on the exact requested frame.
fn is_audio_only(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "mp3" | "aac" | "flac" | "ogg" | "wav" | "opus" | "m4a"
    )
}

/// Locations that Homebrew uses but macOS GUI apps don't inherit via PATH.
#[cfg(target_os = "macos")]
const BREW_PATHS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

fn find_ffmpeg_binary() -> Option<PathBuf> {
    // First try PATH (works in dev / terminal launches)
    if let Ok(p) = which::which("ffmpeg") {
        return Some(p);
    }
    // Check known Homebrew locations that GUI bundles miss
    #[cfg(target_os = "macos")]
    for dir in BREW_PATHS {
        let candidate = PathBuf::from(dir).join("ffmpeg");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    // Last resort: ask the user's login shell — picks up nix, MacPorts, custom PATH
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("/bin/zsh")
            .args(["-l", "-c", "command -v ffmpeg"])
            .output()
        {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !path.is_empty() {
                let p = PathBuf::from(&path);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn find_brew_binary() -> Option<PathBuf> {
    if let Ok(p) = which::which("brew") {
        return Some(p);
    }
    for dir in BREW_PATHS {
        let candidate = PathBuf::from(dir).join("brew");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Install Homebrew non-interactively via the official install script.
#[cfg(target_os = "macos")]
async fn install_brew(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app.emit("brew:installing", ()).ok();

    let mut cmd = tokio::process::Command::new("/bin/bash");
    cmd.arg("-c")
        .arg("curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh | /bin/bash")
        .env("NONINTERACTIVE", "1");

    let ok = run_streamed(app, cmd, "brew").await?;

    if ok {
        if let Some(p) = find_brew_binary() {
            app.emit("brew:installed", ()).ok();
            return Ok(p);
        }
    }

    Err("Homebrew installation failed or brew binary not found after install".to_string())
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
        // Ensure brew is available, installing it if needed
        let brew = match find_brew_binary() {
            Some(p) => p,
            None => install_brew(app).await?,
        };

        app.emit("ffmpeg:installing", ()).ok();

        let mut cmd = tokio::process::Command::new(&brew);
        cmd.args(["install", "ffmpeg"]);
        let ok = run_streamed(app, cmd, "ffmpeg").await?;

        if ok {
            // Re-search after install (brew may have put it in a non-PATH location)
            if let Some(path) = find_ffmpeg_binary() {
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
    output_dir: Option<&str>,
) -> Result<String, String> {
    let ffmpeg = get_ffmpeg()?;
    let out = output_path(path, target_format, output_dir)?;

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
        "m4a" => {
            cmd.args(["-vn", "-codec:a", "aac", "-b:a", "192k"]);
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

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?;

    // Parse progress from stderr
    if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(val) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = val.trim().parse::<f64>() {
                    if duration_secs > 0.0 {
                        let percent =
                            ((us / 1_000_000.0) / duration_secs * 100.0).min(100.0) as f32;
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

    let status = child
        .wait()
        .await
        .map_err(|e| format!("ffmpeg wait error: {e}"))?;
    if !status.success() {
        return Err(format!(
            "ffmpeg exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(out.to_string_lossy().to_string())
}

/// Convert a still image to a single-frame GIF via ffmpeg.
///
/// The `image` crate's GIF encoder is single-threaded LZW and slow enough
/// (10s+ on multi-MP images) that the UI looks frozen. ffmpeg's
/// palettegen+paletteuse path is both much faster *and* gives us real-time
/// progress events to feed the frontend bar.
pub async fn convert_image_to_gif(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let ffmpeg = get_ffmpeg()?;
    let out = output_path(path, "gif", output_dir)?;

    let mut cmd = tokio::process::Command::new(&ffmpeg);
    cmd.args([
        "-y",
        "-i",
        path,
        "-vf",
        "split[s0][s1];[s0]palettegen=max_colors=256[p];[s1][p]paletteuse=dither=sierra2_4a",
        "-progress",
        "pipe:2",
        "-nostats",
        out.to_str().unwrap_or(""),
    ]);
    cmd.stderr(std::process::Stdio::piped());

    let path_string = path.to_string();
    let app_handle = app.clone();

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?;

    // ffmpeg's `-progress pipe:2` prints `progress=continue` / `progress=end`
    // alongside `frame=N` lines. A still image yields one frame, so the only
    // meaningful events are start (frame=1) and end. We map those to ~50% / 100%
    // so the bar visibly moves even on a fast run.
    if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let percent = if line.starts_with("progress=end") {
                Some(100.0_f32)
            } else if line.starts_with("frame=") {
                Some(50.0_f32)
            } else {
                None
            };
            if let Some(p) = percent {
                app_handle
                    .emit(
                        "convert:progress",
                        ProgressPayload { path: path_string.clone(), percent: p },
                    )
                    .ok();
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("ffmpeg wait error: {e}"))?;
    if !status.success() {
        return Err(format!(
            "ffmpeg exited with status {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(out.to_string_lossy().to_string())
}

pub async fn media_duration_secs(path: &str) -> Result<f64, String> {
    let ffmpeg = get_ffmpeg()?;
    Ok(get_duration(&ffmpeg, path).await.unwrap_or(0.0))
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

fn parse_time_to_secs(t: &str) -> Option<f64> {
    let t = t.trim();
    let parts: Vec<&str> = t.split(':').collect();
    match parts.len() {
        1 => parts[0].parse::<f64>().ok(),
        2 => {
            let m: f64 = parts[0].parse().ok()?;
            let s: f64 = parts[1].parse().ok()?;
            Some(m * 60.0 + s)
        }
        3 => {
            let h: f64 = parts[0].parse().ok()?;
            let m: f64 = parts[1].parse().ok()?;
            let s: f64 = parts[2].parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + s)
        }
        _ => None,
    }
}

pub async fn trim_media(
    app: &tauri::AppHandle,
    path: &str,
    start: &str,
    end: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let ffmpeg = get_ffmpeg()?;
    let out = trim_output_path(path, output_dir)?;
    let source_duration_secs = get_duration(&ffmpeg, path).await.unwrap_or(0.0);

    let start_secs = (!start.trim().is_empty())
        .then(|| parse_time_to_secs(start))
        .flatten();
    let end_secs = (!end.trim().is_empty())
        .then(|| parse_time_to_secs(end))
        .flatten();

    if let (Some(s), Some(e)) = (start_secs, end_secs) {
        if e <= s {
            return Err("End time must be after start time".to_string());
        }
    }

    // ffmpeg resets output timestamps to 0 after an input-side -ss, so the
    // progress denominator is the *trimmed* span, not the full source.
    let effective_duration_secs = match (start_secs, end_secs) {
        (Some(s), Some(e)) => e - s,
        (Some(s), None) => (source_duration_secs - s).max(0.0),
        (None, Some(e)) => e,
        (None, None) => source_duration_secs,
    };

    let mut cmd = tokio::process::Command::new(&ffmpeg);
    cmd.arg("-y");
    if !start.trim().is_empty() {
        cmd.args(["-ss", start.trim()]);
    }
    if !end.trim().is_empty() {
        cmd.args(["-to", end.trim()]);
    }
    cmd.args(["-i", path]);
    // Audio: lossless stream copy (audio frames are tiny, so cuts stay accurate).
    // Video: re-encode with container defaults — modern ffmpeg makes the
    // input-side -ss frame-accurate when transcoding.
    if is_audio_only(path) {
        cmd.args(["-c", "copy"]);
    }
    cmd.args(["-progress", "pipe:2", "-nostats"]);
    cmd.arg(&out);
    cmd.stderr(std::process::Stdio::piped());

    let path_string = path.to_string();
    let app_handle = app.clone();

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn ffmpeg: {e}"))?;

    // Keep the tail of ffmpeg's non-progress stderr so a failure can report
    // the actual diagnostic instead of just an exit code.
    let mut stderr_tail: Vec<String> = Vec::new();

    if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            if let Some(val) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = val.trim().parse::<f64>() {
                    if effective_duration_secs > 0.0 {
                        let percent =
                            ((us / 1_000_000.0) / effective_duration_secs * 100.0).min(100.0) as f32;
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
                continue;
            }
            // Skip `-progress` key=value lines (lowercase_with_underscores=...);
            // keep everything else as a potential error message.
            let is_progress_kv = line.split_once('=').is_some_and(|(k, _)| {
                !k.is_empty()
                    && k.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            });
            if !is_progress_kv && !line.trim().is_empty() {
                stderr_tail.push(line);
                if stderr_tail.len() > 15 {
                    stderr_tail.remove(0);
                }
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("ffmpeg wait error: {e}"))?;
    if !status.success() {
        let detail = stderr_tail.join("\n");
        let detail = detail.trim();
        if detail.is_empty() {
            return Err(format!(
                "ffmpeg exited with status {}",
                status.code().unwrap_or(-1)
            ));
        }
        return Err(format!("ffmpeg failed: {detail}"));
    }

    Ok(out.to_string_lossy().to_string())
}
