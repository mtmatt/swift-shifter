use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Emitter;

static PANDOC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

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

#[cfg(target_os = "macos")]
const BREW_PATHS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

fn find_pandoc_binary() -> Option<PathBuf> {
    if let Ok(p) = which::which("pandoc") {
        return Some(p);
    }
    #[cfg(target_os = "macos")]
    for dir in BREW_PATHS {
        let candidate = PathBuf::from(dir).join("pandoc");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let candidate = PathBuf::from(local).join("Pandoc").join("pandoc.exe");
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let candidate = PathBuf::from(r"C:\Program Files\Pandoc\pandoc.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    #[cfg(target_os = "linux")]
    for path in &[
        "/usr/bin/pandoc",
        "/usr/local/bin/pandoc",
        "/snap/bin/pandoc",
    ] {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
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

pub async fn ensure_pandoc(app: &tauri::AppHandle) -> Result<(), String> {
    if PANDOC_PATH.get().is_some() {
        return Ok(());
    }

    if let Some(path) = find_pandoc_binary() {
        PANDOC_PATH.set(Some(path)).ok();
        return Ok(());
    }

    app.emit("pandoc:missing", ()).ok();

    #[cfg(target_os = "macos")]
    {
        if let Some(brew) = find_brew_binary() {
            app.emit("pandoc:installing", ()).ok();
            let ok = tokio::process::Command::new(&brew)
                .args(["install", "pandoc"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);

            if ok {
                if let Some(path) = find_pandoc_binary() {
                    PANDOC_PATH.set(Some(path)).ok();
                    app.emit("pandoc:installed", ()).ok();
                    return Ok(());
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        app.emit("pandoc:installing", ()).ok();
        // Try winget (built-in on Windows 10 1709+ / Windows 11)
        let ok = if which::which("winget").is_ok() {
            tokio::process::Command::new("winget")
                .args(["install", "--id", "JohnMacFarlane.Pandoc", "-e", "--silent"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };

        if ok {
            if let Some(path) = find_pandoc_binary() {
                PANDOC_PATH.set(Some(path)).ok();
                app.emit("pandoc:installed", ()).ok();
                return Ok(());
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        app.emit("pandoc:installing", ()).ok();
        let installed = if which::which("dnf").is_ok() {
            // Fedora / RHEL
            tokio::process::Command::new("pkexec")
                .args(["dnf", "install", "-y", "pandoc"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("pacman").is_ok() {
            // Arch
            tokio::process::Command::new("pkexec")
                .args(["pacman", "-S", "--noconfirm", "pandoc"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("apt-get").is_ok() {
            // Debian / Ubuntu
            tokio::process::Command::new("pkexec")
                .args(["apt-get", "install", "-y", "pandoc"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };

        if installed {
            if let Some(path) = find_pandoc_binary() {
                PANDOC_PATH.set(Some(path)).ok();
                app.emit("pandoc:installed", ()).ok();
                return Ok(());
            }
        }
    }

    PANDOC_PATH.set(None).ok();
    Ok(())
}

fn get_pandoc() -> Result<PathBuf, String> {
    match PANDOC_PATH.get() {
        Some(Some(p)) => Ok(p.clone()),
        _ => find_pandoc_binary().ok_or_else(|| {
            "pandoc not found — install it to enable document conversion".to_string()
        }),
    }
}

/// Map a file extension to the pandoc format name used on the command line.
fn ext_to_pandoc_format(ext: &str) -> &str {
    match ext {
        "md" | "markdown" => "markdown",
        "txt" => "plain",
        "tex" | "latex" => "latex",
        "typst" => "typst",
        "pdf" => "pdf",
        _ => ext,
    }
}

/// Output file extension for a given target format keyword.
fn target_to_ext(target: &str) -> &str {
    match target {
        "latex" => "tex",
        _ => target,
    }
}

/// Return the first PDF engine found on the system.
fn detect_pdf_engine() -> Option<&'static str> {
    const ENGINES: &[&str] = &["tectonic", "xelatex", "pdflatex", "lualatex", "wkhtmltopdf"];
    for engine in ENGINES {
        if which::which(engine).is_ok() {
            return Some(engine);
        }
        #[cfg(target_os = "macos")]
        for dir in BREW_PATHS {
            if PathBuf::from(dir).join(engine).exists() {
                return Some(engine);
            }
        }
    }
    None
}

#[derive(serde::Serialize, Clone)]
struct ProgressPayload {
    path: String,
    percent: f32,
}

pub async fn convert_document(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let pandoc = get_pandoc()?;

    let input_ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let out = output_path(path, target_to_ext(target_format), output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let from_fmt = ext_to_pandoc_format(&input_ext);
    let to_fmt = ext_to_pandoc_format(target_format);

    let mut cmd = tokio::process::Command::new(&pandoc);
    cmd.args([
        "-f",
        from_fmt,
        "-t",
        to_fmt,
        "-o",
        out.to_str().unwrap_or(""),
    ]);

    if target_format == "pdf" {
        if let Some(engine) = detect_pdf_engine() {
            cmd.args(["--pdf-engine", engine]);
        }
    }

    cmd.arg(path);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    let stderr_out = if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncReadExt, BufReader};
        let mut buf = String::new();
        BufReader::new(stderr).read_to_string(&mut buf).await.ok();
        buf
    } else {
        String::new()
    };

    let status = child
        .wait()
        .await
        .map_err(|e| format!("pandoc wait error: {e}"))?;

    if !status.success() {
        let msg = if stderr_out.trim().is_empty() {
            format!("pandoc exited with code {}", status.code().unwrap_or(-1))
        } else {
            stderr_out.trim().to_string()
        };
        return Err(msg);
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();

    Ok(out.to_string_lossy().to_string())
}
