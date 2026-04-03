use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::Emitter;

static PANDOC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static PDFTOHTML_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static EBOOK_CONVERT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static RE_NUM: OnceLock<regex::Regex> = OnceLock::new();
static RE_MERGE: OnceLock<regex::Regex> = OnceLock::new();
static RE_STYLE_BLOCK: OnceLock<regex::Regex> = OnceLock::new();
static RE_CSS_POS: OnceLock<regex::Regex> = OnceLock::new();
static RE_CSS_TOP: OnceLock<regex::Regex> = OnceLock::new();
static RE_CSS_LEFT: OnceLock<regex::Regex> = OnceLock::new();
static RE_CSS_HEIGHT: OnceLock<regex::Regex> = OnceLock::new();
static RE_SVG_SRC: OnceLock<regex::Regex> = OnceLock::new();

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

/// Run `brew install <args>`, automatically clearing stale `.incomplete` lock
/// files left by a previous interrupted download before retrying.
///
/// Returns `true` on success, `false` on failure.
#[cfg(target_os = "macos")]
async fn brew_install(brew: &PathBuf, args: &[&str]) -> bool {
    let out = tokio::process::Command::new(brew)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;

    let out = match out {
        Ok(o) => o,
        Err(_) => return false,
    };

    if out.status.success() {
        return true;
    }

    let stderr = String::from_utf8_lossy(&out.stderr);

    // Brew leaves a .incomplete lock file when a download is interrupted (e.g.
    // the app was quit mid-install). Detect this, remove the stale file, and
    // retry once so the user doesn't have to intervene manually.
    if stderr.contains("has already locked") {
        if let Some(lock_path) = extract_brew_incomplete_path(&stderr) {
            let _ = std::fs::remove_file(&lock_path);
        }
        // Retry after clearing the lock
        return tokio::process::Command::new(brew)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
    }

    false
}

/// Extract the `.incomplete` lock file path from a brew "has already locked"
/// error message.
///
/// Example input: "A `brew install --cask calibre` process has already locked
/// /path/to/file.dmg.incomplete.\nPlease wait…"
#[cfg(target_os = "macos")]
fn extract_brew_incomplete_path(stderr: &str) -> Option<String> {
    let marker = "has already locked ";
    let start = stderr.find(marker)? + marker.len();
    let rest = &stderr[start..];
    let end = rest.find('\n').unwrap_or(rest.len());
    // The sentence ends with a period; strip it
    let path = rest[..end].trim().trim_end_matches('.');
    if path.ends_with(".incomplete") {
        Some(path.to_string())
    } else {
        None
    }
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
            let ok = brew_install(&brew, &["install", "pandoc"]).await;

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

fn find_ebook_convert_binary() -> Option<PathBuf> {
    if let Ok(p) = which::which("ebook-convert") {
        return Some(p);
    }
    #[cfg(target_os = "macos")]
    {
        // Calibre.app bundle (primary install location)
        let app_bin = PathBuf::from("/Applications/calibre.app/Contents/MacOS/ebook-convert");
        if app_bin.exists() {
            return Some(app_bin);
        }
        for dir in BREW_PATHS {
            let candidate = PathBuf::from(dir).join("ebook-convert");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    #[cfg(target_os = "windows")]
    for dir in &[
        r"C:\Program Files\Calibre2",
        r"C:\Program Files (x86)\Calibre2",
    ] {
        let candidate = PathBuf::from(dir).join("ebook-convert.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    #[cfg(target_os = "linux")]
    for path in &["/usr/bin/ebook-convert", "/usr/local/bin/ebook-convert"] {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub async fn ensure_ebook_convert(app: &tauri::AppHandle) -> Result<(), String> {
    if EBOOK_CONVERT_PATH.get().is_some() {
        return Ok(());
    }
    if let Some(path) = find_ebook_convert_binary() {
        EBOOK_CONVERT_PATH.set(Some(path)).ok();
        return Ok(());
    }

    app.emit("ebook-convert:missing", ()).ok();

    #[cfg(target_os = "macos")]
    {
        if let Some(brew) = find_brew_binary() {
            app.emit("ebook-convert:installing", ()).ok();
            let ok = brew_install(&brew, &["install", "--cask", "calibre"]).await;
            if ok {
                if let Some(path) = find_ebook_convert_binary() {
                    EBOOK_CONVERT_PATH.set(Some(path)).ok();
                    app.emit("ebook-convert:installed", ()).ok();
                    return Ok(());
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        app.emit("ebook-convert:installing", ()).ok();
        let ok = if which::which("winget").is_ok() {
            tokio::process::Command::new("winget")
                .args(["install", "--id", "calibre.calibre", "-e", "--silent"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };
        if ok {
            if let Some(path) = find_ebook_convert_binary() {
                EBOOK_CONVERT_PATH.set(Some(path)).ok();
                app.emit("ebook-convert:installed", ()).ok();
                return Ok(());
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        app.emit("ebook-convert:installing", ()).ok();
        let installed = if which::which("dnf").is_ok() {
            tokio::process::Command::new("pkexec")
                .args(["dnf", "install", "-y", "calibre"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("pacman").is_ok() {
            tokio::process::Command::new("pkexec")
                .args(["pacman", "-S", "--noconfirm", "calibre"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("apt-get").is_ok() {
            tokio::process::Command::new("pkexec")
                .args(["apt-get", "install", "-y", "calibre"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };
        if installed {
            if let Some(path) = find_ebook_convert_binary() {
                EBOOK_CONVERT_PATH.set(Some(path)).ok();
                app.emit("ebook-convert:installed", ()).ok();
                return Ok(());
            }
        }
    }

    EBOOK_CONVERT_PATH.set(None).ok();
    Ok(())
}

fn get_ebook_convert() -> Result<PathBuf, String> {
    match EBOOK_CONVERT_PATH.get() {
        Some(Some(p)) => Ok(p.clone()),
        _ => find_ebook_convert_binary().ok_or_else(|| {
            "ebook-convert not found — install Calibre to enable MOBI conversion".to_string()
        }),
    }
}

pub fn ebook_convert_available() -> bool {
    find_ebook_convert_binary().is_some()
}

fn find_pdftohtml_binary() -> Option<PathBuf> {
    if let Ok(p) = which::which("pdftohtml") {
        return Some(p);
    }
    #[cfg(target_os = "macos")]
    for dir in BREW_PATHS {
        let candidate = PathBuf::from(dir).join("pdftohtml");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let candidate = PathBuf::from(r"C:\ProgramData\chocolatey\bin\pdftohtml.exe");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

pub async fn ensure_pdftohtml(app: &tauri::AppHandle) -> Result<(), String> {
    if PDFTOHTML_PATH.get().is_some() {
        return Ok(());
    }

    if let Some(path) = find_pdftohtml_binary() {
        PDFTOHTML_PATH.set(Some(path)).ok();
        return Ok(());
    }

    app.emit("pdftohtml:missing", ()).ok();

    #[cfg(target_os = "macos")]
    {
        if let Some(brew) = find_brew_binary() {
            app.emit("pdftohtml:installing", ()).ok();
            let ok = brew_install(&brew, &["install", "poppler"]).await;

            if ok {
                if let Some(path) = find_pdftohtml_binary() {
                    PDFTOHTML_PATH.set(Some(path)).ok();
                    app.emit("pdftohtml:installed", ()).ok();
                    return Ok(());
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        app.emit("pdftohtml:installing", ()).ok();
        let ok = if which::which("winget").is_ok() {
            tokio::process::Command::new("winget")
                .args(["install", "--id", "poppler.poppler", "-e", "--silent"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };

        if ok {
            if let Some(path) = find_pdftohtml_binary() {
                PDFTOHTML_PATH.set(Some(path)).ok();
                app.emit("pdftohtml:installed", ()).ok();
                return Ok(());
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        app.emit("pdftohtml:installing", ()).ok();
        let installed = if which::which("dnf").is_ok() {
            tokio::process::Command::new("pkexec")
                .args(["dnf", "install", "-y", "poppler-utils"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("pacman").is_ok() {
            tokio::process::Command::new("pkexec")
                .args(["pacman", "-S", "--noconfirm", "poppler"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("apt-get").is_ok() {
            tokio::process::Command::new("pkexec")
                .args(["apt-get", "install", "-y", "poppler-utils"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else {
            false
        };

        if installed {
            if let Some(path) = find_pdftohtml_binary() {
                PDFTOHTML_PATH.set(Some(path)).ok();
                app.emit("pdftohtml:installed", ()).ok();
                return Ok(());
            }
        }
    }

    PDFTOHTML_PATH.set(None).ok();
    Ok(())
}

fn get_pdftohtml() -> Result<PathBuf, String> {
    match PDFTOHTML_PATH.get() {
        Some(Some(p)) => Ok(p.clone()),
        _ => find_pdftohtml_binary().ok_or_else(|| {
            "pdftohtml not found — install poppler to enable PDF → EPUB conversion".to_string()
        }),
    }
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
        "epub" => "epub",
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

// ── marker-pdf integration ────────────────────────────────────────────────────

/// Find a binary by trying multiple names, including common brew/user paths.
fn find_any_binary(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if let Ok(p) = which::which(name) {
            return Some(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let extra: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", &format!("{home}/.local/bin")];
    for dir in extra {
        for name in names {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn find_marker_binary() -> Option<PathBuf> {
    // Check PATH + common install locations from both pip --user and pipx
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates: &[&str] = &["marker", "marker_single"];
    if let Some(p) = find_any_binary(candidates) {
        return Some(p);
    }
    // pip --user on macOS writes to ~/Library/Python/X.Y/bin
    #[cfg(target_os = "macos")]
    for ver in ["3.13", "3.12", "3.11", "3.10"] {
        for name in candidates {
            let p = PathBuf::from(format!("{home}/Library/Python/{ver}/bin/{name}"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    // pipx default bin dir
    for name in candidates {
        let p = PathBuf::from(format!("{home}/.local/bin/{name}"));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Returns true if the `marker` binary is available on this system.
pub fn marker_available() -> bool {
    find_marker_binary().is_some()
}

/// Emit a user-visible step message during marker installation.
fn marker_step(app: &tauri::AppHandle, msg: &str) {
    app.emit("marker:step", msg).ok();
}

/// Run a command silently; return Ok(()) on success or a trimmed stderr string on failure.
async fn run_silent(program: &PathBuf, args: &[&str]) -> Result<(), String> {
    let out = tokio::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| e.to_string())?;

    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    Err([stderr.trim(), stdout.trim()]
        .iter()
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("exited with code {}", out.status.code().unwrap_or(-1))))
}

/// Install `marker-pdf` fully automatically — the user never needs to know
/// what Python, pip, or pipx is.
///
/// Steps (each emits a `marker:step` event so the UI can show progress):
///   1. Ensure pipx is available  (auto-install via brew if needed)
///   2. Ensure Python is available (auto-install via brew if needed)
///   3. `pipx install marker-pdf`
///   4. Fall back to `pip install --user marker-pdf` if pipx failed
pub async fn install_marker(app: &tauri::AppHandle) -> Result<(), String> {
    // ── Step 1: get pipx ───────────────────────────────────────────────────
    let pipx = if let Some(p) = find_any_binary(&["pipx"]) {
        Some(p)
    } else {
        marker_step(app, "Setting up package installer…");
        #[cfg(target_os = "macos")]
        if let Some(brew) = find_brew_binary() {
            run_silent(&brew, &["install", "pipx"]).await.ok();
        }
        #[cfg(target_os = "windows")]
        if which::which("winget").is_ok() {
            run_silent(
                &PathBuf::from("winget"),
                &["install", "--id", "pypa.pipx", "-e", "--silent"],
            ).await.ok();
        }
        #[cfg(target_os = "linux")]
        if which::which("dnf").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["dnf", "install", "-y", "pipx"]).await.ok();
        } else if which::which("pacman").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["pacman", "-S", "--noconfirm", "python-pipx"]).await.ok();
        } else if which::which("apt-get").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["apt-get", "install", "-y", "pipx"]).await.ok();
        }
        find_any_binary(&["pipx"])
    };

    // ── Step 2: install marker-pdf via pipx ────────────────────────────────
    if let Some(ref pipx) = pipx {
        marker_step(app, "Downloading marker-pdf — this may take a few minutes…");
        match run_silent(pipx, &["install", "marker-pdf"]).await {
            Ok(()) => {
                run_silent(pipx, &["inject", "marker-pdf", "psutil"]).await.ok();
                return Ok(());
            }
            Err(_) => {}
        }
    }

    // ── Step 3: ensure pip / Python ────────────────────────────────────────
    let pip = if let Some(p) = find_any_binary(&["pip3", "pip"]) {
        Some(p)
    } else {
        marker_step(app, "Setting up Python…");
        #[cfg(target_os = "macos")]
        if let Some(brew) = find_brew_binary() {
            run_silent(&brew, &["install", "python3"]).await.ok();
        }
        #[cfg(target_os = "windows")]
        if which::which("winget").is_ok() {
            run_silent(
                &PathBuf::from("winget"),
                &["install", "--id", "Python.Python.3", "-e", "--silent"],
            ).await.ok();
        }
        #[cfg(target_os = "linux")]
        if which::which("dnf").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["dnf", "install", "-y", "python3-pip"]).await.ok();
        } else if which::which("pacman").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["pacman", "-S", "--noconfirm", "python-pip"]).await.ok();
        } else if which::which("apt-get").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["apt-get", "install", "-y", "python3-pip"]).await.ok();
        }
        find_any_binary(&["pip3", "pip"])
    };

    // ── Step 4: pip install --user ─────────────────────────────────────────
    if let Some(ref pip) = pip {
        marker_step(app, "Downloading marker-pdf — this may take a few minutes…");
        return run_silent(pip, &["install", "--user", "marker-pdf", "psutil"]).await
            .map_err(|e| format!("Installation failed: {e}"));
    }

    Err("Could not install marker-pdf automatically. Please install pipx or pip and try again.".to_string())
}

/// Recursively copy everything under `src_dir` into `dst_dir`, skipping `skip`.
/// Subdirectories are recreated with the same name so relative paths in the
/// markdown (e.g. `images/_page_4_Figure_7.jpeg`) keep working.
fn copy_dir_contents_except(src_dir: &Path, dst_dir: &Path, skip: &Path) {
    let Ok(entries) = std::fs::read_dir(src_dir) else { return };
    for entry in entries.flatten() {
        let src = entry.path();
        if src == skip {
            continue;
        }
        let Some(name) = src.file_name() else { continue };
        let dst = dst_dir.join(name);
        if src.is_dir() {
            let _ = std::fs::create_dir_all(&dst);
            copy_dir_contents_except(&src, &dst, skip);
        } else if src.is_file() {
            let _ = std::fs::copy(&src, &dst);
        }
    }
}

/// Recursively find the first `.md` file under `dir`.
fn find_md_file(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_md_file(&path) {
                return Some(found);
            }
        }
    }
    None
}

/// Convert a PDF to EPUB using the marker-pdf ML pipeline:
///   1. `marker input.pdf --output_dir tmp` — produces Markdown + images
///   2. `pandoc *.md -t epub` — packages them into EPUB
///
/// marker produces significantly better output than pdftohtml for complex PDFs
/// (proper heading detection, table extraction, image placement) but requires
/// Python + `pip install marker-pdf` and can take several minutes on CPU.
pub async fn convert_pdf_with_marker(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let marker = find_marker_binary()
        .ok_or_else(|| "marker not found — install with: pip install marker-pdf".to_string())?;
    let pandoc = get_pandoc()?;

    // Save before it is shadowed by the internal temp-dir variable below.
    let user_output_dir = output_dir;

    let out = output_path(path, "epub", user_output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    ).ok();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let tmp_base = std::env::temp_dir().join(format!("swift_shifter_marker_{timestamp}"));
    let input_dir = tmp_base.join("input");
    let output_dir = tmp_base.join("output");

    std::fs::create_dir_all(&input_dir)
        .map_err(|e| format!("Failed to create input temp directory: {e}"))?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output temp directory: {e}"))?;

    // Copy the PDF in so marker works on a clean temp path
    let tmp_pdf = input_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF: {e}"))?;

    // Build the marker command.
    // Always pass the PDF file directly — some marker versions (2.x) do not
    // accept a directory as the positional argument and silently produce no
    // output when given one.
    let marker_name = marker.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut cmd = tokio::process::Command::new(&marker);
    if marker_name.contains("marker_single") {
        cmd.args([tmp_pdf.to_str().unwrap_or(""), output_dir.to_str().unwrap_or("")]);
    } else {
        cmd.args([
            input_dir.to_str().unwrap_or(""),
            "--output_dir",
            output_dir.to_str().unwrap_or(""),
        ]);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to spawn marker: {e}"))?;

    // Analyze marker output for progress
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let app_handle = app.clone();
    let path_str = path.to_string();

    let progress_task = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stdout).lines();
        let mut err_reader = BufReader::new(stderr).lines();

        // Named pipeline stages and the progress % they represent when first seen.
        // Keep keywords specific enough that they don't fire on marker's startup
        // "Output directory: …" line (which would jump straight to 90%).
        let stages: &[(&str, f32)] = &[
            // Model loading
            ("load model",          5.0),
            ("detection model",     8.0),
            ("texify",             12.0),
            ("recognition model",  12.0),
            ("surya",              14.0),
            // PDF parsing
            ("reading pdf",        18.0),
            ("pdf loaded",         20.0),
            // Layout / OCR
            ("running layout",     28.0),
            ("layout detection",   28.0),
            ("running ocr",        38.0),
            ("text extraction",    40.0),
            ("running line",       44.0),
            // Post-processing
            ("post-processing",    60.0),
            ("ordering blocks",    65.0),
            ("merging lines",      68.0),
            ("cleaning text",      72.0),
            ("formatting",         75.0),
            // Saving
            ("saving output",      88.0),
            ("writing markdown",   88.0),
            ("saved to",           90.0),
        ];

        let mut current_pct: f32 = 2.0;
        let mut stdout_done = false;
        let mut stderr_done = false;

        app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();

        /// Advance progress if the line matches a known stage keyword.
        /// Returns the new percentage (unchanged if no match).
        fn advance_from_line(line: &str, stages: &[(&str, f32)], current: f32) -> f32 {
            let lower = line.to_lowercase();
            let mut best = current;
            for (kw, pct) in stages {
                if *pct > current && lower.contains(kw) {
                    if *pct > best { best = *pct; }
                }
            }
            best
        }

        /// Try to parse a tqdm-style percentage from a line, e.g. " 42%|█…".
        fn tqdm_percent(line: &str) -> Option<f32> {
            let pos = line.find('%')?;
            // Scan backwards up to 4 chars for digits
            let start = pos.saturating_sub(4);
            let num_str = line[start..pos].trim_start_matches(|c: char| !c.is_ascii_digit());
            num_str.parse::<f32>().ok()
        }

        loop {
            if stdout_done && stderr_done { break; }

            tokio::select! {
                line = reader.next_line(), if !stdout_done => {
                    match line {
                        Ok(Some(l)) => {
                            // tqdm percentage on stdout
                            if let Some(p) = tqdm_percent(&l) {
                                let mapped = 20.0 + (p / 100.0) * 68.0;
                                if mapped > current_pct { current_pct = mapped; }
                            } else {
                                let new_pct = advance_from_line(&l, stages, current_pct);
                                if new_pct > current_pct { current_pct = new_pct; }
                            }
                            app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();
                        }
                        _ => { stdout_done = true; }
                    }
                }
                line = err_reader.next_line(), if !stderr_done => {
                    match line {
                        Ok(Some(l)) => {
                            // tqdm bars go to stderr; try percentage first
                            if let Some(p) = tqdm_percent(&l) {
                                let mapped = 20.0 + (p / 100.0) * 68.0;
                                if mapped > current_pct { current_pct = mapped; }
                            } else {
                                let new_pct = advance_from_line(&l, stages, current_pct);
                                if new_pct > current_pct { current_pct = new_pct; }
                            }
                            app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();
                        }
                        _ => { stderr_done = true; }
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(800)) => {
                    // Slow creep so the bar never looks frozen
                    if current_pct < 85.0 {
                        current_pct += 0.3;
                        app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();
                    }
                }
            }
        }
    });

    let status = child.wait().await
        .map_err(|e| format!("marker wait error: {e}"))?;
    let _ = progress_task.await;

    // On failure or missing output, fall back to the pdftohtml pipeline rather
    // than surfacing a confusing marker-specific error to the user.
    if !status.success() || find_md_file(&output_dir).is_none() {
        let _ = std::fs::remove_dir_all(&tmp_base);
        return convert_pdf_to_epub(app, path, user_output_dir).await;
    }

    let md_file = find_md_file(&output_dir).unwrap();
    
    // Fix tag mismatch error in generated markdown (e.g. unclosed <sup> or <sub>)
    if let Ok(mut content) = tokio::fs::read_to_string(&md_file).await {
        let mut changed = false;
        let tags = ["sup", "sub"];
        for tag in tags {
            let open = format!("<{}>", tag);
            let close = format!("</{}>", tag);
            let open_count = content.matches(&open).count();
            let close_count = content.matches(&close).count();
            
            if open_count > close_count {
                for _ in 0..(open_count - close_count) {
                    content.push_str(&close);
                }
                changed = true;
            }
        }
        if changed {
            let _ = tokio::fs::write(&md_file, content).await;
        }
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 85.0 },
    ).ok();

    let md_dir = md_file.parent().unwrap_or(&output_dir);

    let file_title = Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Write a small CSS file into the temp dir so pandoc can embed it.
    // This preserves whitespace in code blocks and ensures math isn't crushed.
    // Only pass --css to pandoc if the file was actually written successfully;
    // a silently-missing file causes pandoc to ignore or error on the flag.
    static EPUB_CSS: &str = concat!(
        "code, pre, kbd, samp {\n",
        "  white-space: pre;\n",
        "  font-family: monospace;\n",
        "}\n",
        "pre {\n",
        "  overflow-x: auto;\n",
        "  padding: 0.5em;\n",
        "}\n",
        ".math, .MathML, math {\n",
        "  white-space: pre;\n",
        "}\n",
    );
    let epub_css_path: Option<String> = {
        let p = md_dir.join("_swift_shifter_epub.css");
        if std::fs::write(&p, EPUB_CSS).is_ok() {
            p.to_str().map(|s| s.to_string())
        } else {
            None
        }
    };

    let mut pandoc_cmd = tokio::process::Command::new(&pandoc);
    pandoc_cmd.current_dir(md_dir);
    pandoc_cmd.args([
        "-f", "markdown+footnotes+superscript+subscript+tex_math_dollars+tex_math_single_backslash",
        "-t", "epub3",
        "--mathml",
    ]);
    if let Some(ref css_str) = epub_css_path {
        pandoc_cmd.args(["--css", css_str]);
    }
    pandoc_cmd.args([
        "--metadata", &format!("title={}", file_title),
        "-o", out.to_str().unwrap_or(""),
        md_file.file_name().unwrap_or_default().to_str().unwrap_or(""),
    ]);
    pandoc_cmd.stdout(std::process::Stdio::null());
    pandoc_cmd.stderr(std::process::Stdio::piped());

    let mut child = pandoc_cmd.spawn()
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    let stderr_out = if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncReadExt, BufReader};
        let mut buf = String::new();
        BufReader::new(stderr).read_to_string(&mut buf).await.ok();
        buf
    } else {
        String::new()
    };

    let status = child.wait().await
        .map_err(|e| format!("pandoc wait error: {e}"))?;

    let _ = std::fs::remove_dir_all(&tmp_base);

    if !status.success() {
        return Err(if stderr_out.trim().is_empty() {
            format!("pandoc exited with code {}", status.code().unwrap_or(-1))
        } else {
            stderr_out.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    ).ok();

    Ok(out.to_string_lossy().to_string())
}

// ── SVG helper ────────────────────────────────────────────────────────────────

/// Try to convert an SVG file to PNG using `rsvg-convert` or ImageMagick.
/// Returns the PNG path on success, None if no suitable tool is found.
async fn convert_svg_to_png(svg_path: &Path) -> Option<PathBuf> {
    let png_path = svg_path.with_extension("png");

    // Prefer rsvg-convert (brew install librsvg) — lossless, no Ghostscript needed
    if let Ok(rsvg) = which::which("rsvg-convert") {
        let ok = tokio::process::Command::new(&rsvg)
            .args(["-o", png_path.to_str()?, svg_path.to_str()?])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && png_path.exists() {
            return Some(png_path);
        }
    }

    // Fall back to ImageMagick 7 (`magick`) or 6 (`convert`)
    let magick = which::which("magick")
        .or_else(|_| which::which("convert"))
        .ok()?;
    let ok = tokio::process::Command::new(&magick)
        .args([svg_path.to_str()?, png_path.to_str()?])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    if ok && png_path.exists() {
        Some(png_path)
    } else {
        None
    }
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

/// Convert an image file to PDF by generating a temporary markdown wrapper and
/// running pandoc with `--resource-path` pointing at the image's directory.
pub async fn convert_image_to_pdf(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let pandoc = get_pandoc()?;

    let input = Path::new(path);
    let filename = input
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let image_dir = input
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();

    let out = output_path(path, "pdf", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let tmp_md = std::env::temp_dir().join(format!("swift_shifter_{timestamp}.md"));
    std::fs::write(&tmp_md, format!("![]({})", filename))
        .map_err(|e| format!("Failed to create temp file: {e}"))?;

    let mut cmd = tokio::process::Command::new(&pandoc);
    cmd.args([
        "-f",
        "markdown",
        "-t",
        "pdf",
        "--resource-path",
        &image_dir,
        "-o",
        out.to_str().unwrap_or(""),
    ]);

    if let Some(engine) = detect_pdf_engine() {
        cmd.args(["--pdf-engine", engine]);
    }

    cmd.arg(tmp_md.to_str().unwrap_or(""));
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

    let _ = std::fs::remove_file(&tmp_md);

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

/// Convert a PDF to EPUB via a two-step pipeline:
///   1. `pdftohtml -noframes` converts the PDF to HTML, extracting embedded images
///   2. `pandoc -f html -t epub` packages the HTML + images into an EPUB
pub async fn convert_pdf_to_epub(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let pdftohtml = get_pdftohtml()?;
    let pandoc = get_pandoc()?;

    let out = output_path(path, "epub", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    // Create a temp directory so pdftohtml can write the HTML + image files
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let tmp_dir = std::env::temp_dir().join(format!("swift_shifter_{timestamp}"));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp directory: {e}"))?;

    // Copy the PDF into tmp_dir and run pdftohtml from there.
    // This ensures that both the HTML and all extracted image files land in
    // the same directory regardless of poppler version or cwd behaviour.
    let tmp_pdf = tmp_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF to temp dir: {e}"))?;

    let tmp_html = tmp_dir.join("doc.html");

    // Step 1: PDF → HTML (preserves images)
    // Run from tmp_dir with relative paths so image files are always written
    // alongside doc.html. Capture stderr to suppress harmless poppler warnings
    // like "Syntax Warning: Bad annotation destination".
    let pdftohtml_out = tokio::process::Command::new(&pdftohtml)
        .current_dir(&tmp_dir)
        .args(["-noframes", "-nodrm", "-fmt", "png", "input.pdf", "doc"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pdftohtml: {e}"))?;

    if !pdftohtml_out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&pdftohtml_out.stderr);
        let msg = if stderr.trim().is_empty() {
            format!(
                "pdftohtml exited with code {}",
                pdftohtml_out.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        };
        return Err(msg);
    }

    // Post-process the HTML pdftohtml generated before handing it to pandoc.
    if let Ok(mut html_content) = tokio::fs::read_to_string(&tmp_html).await {
        // ── 1. Strip the entire <style> block ────────────────────────────────
        // pdftohtml emits class-level rules such as `.page { height:1262px }`
        // that survive inline-style stripping.  A 600-page PDF at zoom 1.5
        // would leave ~2000 blank EPUB pages from those fixed-height containers
        // alone.  We don't need pdftohtml's presentation CSS in an EPUB.
        let re_style_block = RE_STYLE_BLOCK.get_or_init(|| {
            regex::Regex::new(r"(?s)<style\b[^>]*>.*?</style>").unwrap()
        });
        html_content = re_style_block.replace_all(&html_content, "").to_string();

        // ── 2. Strip residual inline layout CSS ──────────────────────────────
        // position:absolute with large top/left values (derived from the PDF
        // coordinate system) would create thousands of blank pages even without
        // the class rules.  Strip all four properties from inline styles.
        let re_pos = RE_CSS_POS.get_or_init(|| {
            regex::Regex::new(r"position\s*:\s*(?:absolute|relative)\s*;?\s*").unwrap()
        });
        html_content = re_pos.replace_all(&html_content, "").to_string();

        let re_top = RE_CSS_TOP.get_or_init(|| {
            regex::Regex::new(r"top\s*:\s*-?\d+(?:\.\d+)?px\s*;?\s*").unwrap()
        });
        html_content = re_top.replace_all(&html_content, "").to_string();

        let re_left = RE_CSS_LEFT.get_or_init(|| {
            regex::Regex::new(r"left\s*:\s*-?\d+(?:\.\d+)?px\s*;?\s*").unwrap()
        });
        html_content = re_left.replace_all(&html_content, "").to_string();

        let re_height = RE_CSS_HEIGHT.get_or_init(|| {
            regex::Regex::new(r"height\s*:\s*\d+(?:\.\d+)?px\s*;?\s*").unwrap()
        });
        html_content = re_height.replace_all(&html_content, "").to_string();

        // ── 3. Fix wrapped sentence line-breaks ──────────────────────────────
        let re_num = RE_NUM.get_or_init(|| {
            regex::Regex::new(r"(?m)^\s*(?:<a name=\d+></a>)?\s*\d+\s*<br/>\s*\r?\n?").unwrap()
        });
        html_content = re_num.replace_all(&html_content, "").to_string();

        let re_merge = RE_MERGE.get_or_init(|| {
            regex::Regex::new(r"([a-zA-Z,\-]|&#160;)\s*<br/>\s*\r?\n?\s*(?:&#160;)*([a-z])")
                .unwrap()
        });
        html_content = re_merge.replace_all(&html_content, "${1} ${2}").to_string();

        // ── 4. Convert SVG images to PNG ──────────────────────────────────────
        // pdftohtml extracts vector graphics as .svg files.  Many EPUB readers
        // cannot render SVG, so try rsvg-convert / ImageMagick.  If conversion
        // fails, remove the broken <img> tag entirely.
        let re_svg = RE_SVG_SRC.get_or_init(|| {
            regex::Regex::new(r#"(?i)src="([^"]+\.svg)""#).unwrap()
        });
        let svg_names: Vec<String> = re_svg
            .captures_iter(&html_content)
            .map(|c| c[1].to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for svg_name in svg_names {
            let svg_path = tmp_dir.join(&svg_name);
            if let Some(png_path) = convert_svg_to_png(&svg_path).await {
                let png_name = png_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                html_content = html_content
                    .replace(&format!(r#"src="{}""#, svg_name), &format!(r#"src="{}""#, png_name));
            } else {
                // Remove the entire <img> tag so no broken image appears.
                if let Ok(re_img) = regex::Regex::new(&format!(
                    r#"(?i)<img\b[^>]*\bsrc="{}"[^>]*/?>[ \t]*"#,
                    regex::escape(&svg_name)
                )) {
                    html_content = re_img.replace_all(&html_content, "").to_string();
                }
            }
        }

        let _ = tokio::fs::write(&tmp_html, html_content).await;
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 50.0,
        },
    )
    .ok();

    // Extract title from original file path
    let file_title = Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    // Step 2: HTML + images → EPUB
    // Run pandoc from tmp_dir so that relative image paths inside doc.html
    // (e.g. "doc001.png") resolve to the files pdftohtml wrote alongside it.
    let mut cmd = tokio::process::Command::new(&pandoc);
    cmd.current_dir(&tmp_dir);
    cmd.args([
        "-f",
        "html",
        "-t",
        "epub",
        "--metadata",
        &format!("title={}", file_title),
        "-o",
        out.to_str().unwrap_or(""),
        "doc.html",
    ]);
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

    let _ = std::fs::remove_dir_all(&tmp_dir);

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

// ── ebook-convert (Calibre) helpers ──────────────────────────────────────────

/// Run `ebook-convert input output` for any format Calibre supports.
async fn run_ebook_convert(
    app: &tauri::AppHandle,
    input: &str,
    output: &PathBuf,
) -> Result<(), String> {
    let ec = get_ebook_convert()?;
    app.emit(
        "convert:progress",
        ProgressPayload { path: input.to_string(), percent: 0.0 },
    )
    .ok();

    let result = tokio::process::Command::new(&ec)
        .args([
            input,
            output.to_str().unwrap_or(""),
            // Strip any dark background/color styling baked into the source document
            "--filter-css", "background-color,background,color",
            // Then impose a clean white-on-black stylesheet
            "--extra-css", "body, html { background-color: white !important; color: black !important; }",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn ebook-convert: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("ebook-convert exited with code {}", result.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: input.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(())
}

/// Convert a MOBI file to pdf, epub, html, or md.
pub async fn convert_mobi(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    match target_format {
        "pdf" | "epub" | "html" => {
            let out = output_path(path, target_format, output_dir)?;
            run_ebook_convert(app, path, &out).await?;
            Ok(out.to_string_lossy().to_string())
        }
        "md" => {
            // MOBI → temp EPUB → pandoc markdown
            let pandoc = get_pandoc()?;
            let out = output_path(path, "md", output_dir)?;
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let tmp_epub =
                std::env::temp_dir().join(format!("swift_shifter_mobi_{timestamp}.epub"));
            run_ebook_convert(app, path, &tmp_epub).await?;

            app.emit(
                "convert:progress",
                ProgressPayload { path: path.to_string(), percent: 50.0 },
            )
            .ok();

            let status = tokio::process::Command::new(&pandoc)
                .args([
                    "-f", "epub",
                    "-t", "markdown",
                    "-o", out.to_str().unwrap_or(""),
                    tmp_epub.to_str().unwrap_or(""),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

            let _ = std::fs::remove_file(&tmp_epub);

            if !status.success() {
                return Err(format!("pandoc exited with code {}", status.code().unwrap_or(-1)));
            }

            app.emit(
                "convert:progress",
                ProgressPayload { path: path.to_string(), percent: 100.0 },
            )
            .ok();
            Ok(out.to_string_lossy().to_string())
        }
        _ => Err(format!("Unsupported MOBI target format: {target_format}")),
    }
}

/// Convert an EPUB to MOBI via ebook-convert.
pub async fn convert_epub_to_mobi(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let out = output_path(path, "mobi", output_dir)?;
    run_ebook_convert(app, path, &out).await?;
    Ok(out.to_string_lossy().to_string())
}

/// Convert a PDF to MOBI.
/// When `use_marker` is true and marker-pdf is available, the pipeline is
/// PDF→EPUB (via marker) → MOBI (via ebook-convert), which preserves images
/// and structure better than a direct PDF→MOBI via ebook-convert alone.
pub async fn convert_pdf_to_mobi(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    use_marker: bool,
) -> Result<String, String> {
    let out = output_path(path, "mobi", output_dir)?;

    if use_marker && marker_available() {
        // Build a temp EPUB via the marker pipeline, then convert to MOBI.
        let tmp_epub = std::env::temp_dir().join(format!(
            "swift_shifter_marker_mobi_{}.epub",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        // convert_pdf_with_marker writes to output_dir; pass the temp file's
        // parent so the EPUB lands where we expect it.
        let tmp_epub_dir = tmp_epub.parent().map(|p| p.to_string_lossy().to_string());
        let epub_path = convert_pdf_with_marker(
            app,
            path,
            tmp_epub_dir.as_deref(),
        ).await?;
        // The returned path may differ in stem; rename to our known tmp path.
        if std::path::Path::new(&epub_path) != tmp_epub {
            let _ = std::fs::rename(&epub_path, &tmp_epub);
        }
        run_ebook_convert(app, tmp_epub.to_str().unwrap_or(path), &out).await?;
        let _ = std::fs::remove_file(&tmp_epub);
    } else {
        run_ebook_convert(app, path, &out).await?;
    }

    Ok(out.to_string_lossy().to_string())
}

// ── PDF → HTML / MD ──────────────────────────────────────────────────────────

/// Convert a PDF to a standalone HTML file via pdftohtml.
/// Companion image files land in the same directory as the HTML.
pub async fn convert_pdf_to_html(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let pdftohtml = get_pdftohtml()?;
    let out = output_path(path, "html", output_dir)?;
    // pdftohtml appends ".html" to the stem argument; strip the extension we added.
    let out_stem = out.with_extension("");

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let result = tokio::process::Command::new(&pdftohtml)
        .args(["-noframes", "-nodrm", "-fmt", "png", path, out_stem.to_str().unwrap_or("")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pdftohtml: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("pdftohtml exited with code {}", result.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

/// Convert a PDF to Markdown using the pdftohtml → pandoc pipeline.
async fn convert_pdf_to_md_via_pdftohtml(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let pdftohtml = get_pdftohtml()?;
    let pandoc = get_pandoc()?;
    let out = output_path(path, "md", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let tmp_dir = std::env::temp_dir().join(format!("swift_shifter_pdf_md_{timestamp}"));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let tmp_pdf = tmp_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF: {e}"))?;

    let html_result = tokio::process::Command::new(&pdftohtml)
        .current_dir(&tmp_dir)
        .args(["-noframes", "-nodrm", "-fmt", "png", "input.pdf", "doc"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pdftohtml: {e}"))?;

    if !html_result.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&html_result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("pdftohtml exited with code {}", html_result.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 50.0 },
    )
    .ok();

    let md_result = tokio::process::Command::new(&pandoc)
        .current_dir(&tmp_dir)
        .args(["-f", "html", "-t", "markdown", "-o", out.to_str().unwrap_or(""), "doc.html"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !md_result.status.success() {
        let stderr = String::from_utf8_lossy(&md_result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("pandoc exited with code {}", md_result.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

/// Convert a PDF to Markdown.
/// Uses marker-pdf (ML) if `use_marker` is true and marker is installed;
/// otherwise falls back to pdftohtml → pandoc markdown.
pub async fn convert_pdf_to_md(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    use_marker: bool,
) -> Result<String, String> {
    if use_marker && marker_available() {
        return convert_pdf_with_marker_to_md(app, path, output_dir).await;
    }
    convert_pdf_to_md_via_pdftohtml(app, path, output_dir).await
}

/// Run marker on a PDF and copy the resulting .md file to the output location.
async fn convert_pdf_with_marker_to_md(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let marker = find_marker_binary()
        .ok_or_else(|| "marker not found — install with: pip install marker-pdf".to_string())?;
    let out = output_path(path, "md", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let tmp_base = std::env::temp_dir().join(format!("swift_shifter_marker_md_{timestamp}"));
    let input_dir = tmp_base.join("input");
    let output_dir_tmp = tmp_base.join("output");

    std::fs::create_dir_all(&input_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    std::fs::create_dir_all(&output_dir_tmp)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let tmp_pdf = input_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF: {e}"))?;

    let marker_name = marker.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut cmd = tokio::process::Command::new(&marker);
    if marker_name.contains("marker_single") {
        cmd.args([tmp_pdf.to_str().unwrap_or(""), output_dir_tmp.to_str().unwrap_or("")]);
    } else {
        cmd.args([
            input_dir.to_str().unwrap_or(""),
            "--output_dir",
            output_dir_tmp.to_str().unwrap_or(""),
        ]);
    }
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let status = cmd
        .status()
        .await
        .map_err(|e| format!("Failed to spawn marker: {e}"))?;

    // On failure or missing output, fall back to the pdftohtml pipeline.
    if !status.success() || find_md_file(&output_dir_tmp).is_none() {
        let _ = std::fs::remove_dir_all(&tmp_base);
        return convert_pdf_to_md_via_pdftohtml(app, path, output_dir).await;
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 90.0 },
    )
    .ok();

    let md_file = find_md_file(&output_dir_tmp).unwrap();

    std::fs::copy(&md_file, &out)
        .map_err(|e| format!("Failed to copy marker output: {e}"))?;

    // Copy all assets (images, subdirs) that sit alongside the .md so that
    // relative image references (e.g. `![](images/_page_4_Figure_7.jpeg)`)
    // resolve correctly next to the output file.
    let out_dir = out.parent().unwrap_or_else(|| std::path::Path::new("."));
    if let Some(md_parent) = md_file.parent() {
        copy_dir_contents_except(md_parent, out_dir, &md_file);
    }

    let _ = std::fs::remove_dir_all(&tmp_base);

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}
