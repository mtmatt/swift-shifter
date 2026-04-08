use std::path::PathBuf;
use tauri::Emitter;
use crate::converter::document::{PANDOC_PATH, EBOOK_CONVERT_PATH, PYMUPDF4LLM_PYTHON};

#[cfg(target_os = "macos")]
pub const BREW_PATHS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

pub fn find_pandoc_binary() -> Option<PathBuf> {
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
pub async fn brew_install(brew: &PathBuf, args: &[&str]) -> bool {
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

    if stderr.contains("has already locked") {
        if let Some(lock_path) = extract_brew_incomplete_path(&stderr) {
            let _ = std::fs::remove_file(&lock_path);
        }
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

#[cfg(target_os = "macos")]
pub fn extract_brew_incomplete_path(stderr: &str) -> Option<String> {
    let marker = "has already locked ";
    let start = stderr.find(marker)? + marker.len();
    let rest = &stderr[start..];
    let end = rest.find('\n').unwrap_or(rest.len());
    let path = rest[..end].trim().trim_end_matches('.');
    if path.ends_with(".incomplete") {
        Some(path.to_string())
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
pub fn find_brew_binary() -> Option<PathBuf> {
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
            tokio::process::Command::new("pkexec")
                .args(["dnf", "install", "-y", "pandoc"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("pacman").is_ok() {
            tokio::process::Command::new("pkexec")
                .args(["pacman", "-S", "--noconfirm", "pandoc"])
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
        } else if which::which("apt-get").is_ok() {
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

pub fn find_ebook_convert_binary() -> Option<PathBuf> {
    if let Ok(p) = which::which("ebook-convert") {
        return Some(p);
    }
    #[cfg(target_os = "macos")]
    {
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

pub fn get_ebook_convert() -> Result<PathBuf, String> {
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

fn python_has_pymupdf4llm(python: &PathBuf) -> bool {
    std::process::Command::new(python)
        .args(["-c", "import pymupdf4llm"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn find_pymupdf4llm_python() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    for name in &["python3", "python"] {
        if let Ok(p) = which::which(name) {
            candidates.push(p);
        }
    }

    #[cfg(target_os = "macos")]
    for dir in BREW_PATHS {
        for name in &["python3", "python"] {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                candidates.push(p);
            }
        }
    }

    #[cfg(target_os = "linux")]
    for path in &["/usr/bin/python3", "/usr/local/bin/python3", "/usr/bin/python"] {
        candidates.push(PathBuf::from(path));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(p) = which::which("py") {
            candidates.push(p);
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            for ver in &["Python313", "Python312", "Python311", "Python310"] {
                let candidate = PathBuf::from(&local)
                    .join("Programs")
                    .join("Python")
                    .join(ver)
                    .join("python.exe");
                if candidate.exists() {
                    candidates.push(candidate);
                }
            }
        }
    }

    // Deduplicate while preserving priority order
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|p| seen.insert(p.clone()));

    candidates.into_iter().find(|p| python_has_pymupdf4llm(p))
}

pub async fn ensure_pymupdf4llm(app: &tauri::AppHandle) -> Result<(), String> {
    if PYMUPDF4LLM_PYTHON.get().is_some() {
        return Ok(());
    }

    if let Some(path) = find_pymupdf4llm_python() {
        PYMUPDF4LLM_PYTHON.set(Some(path)).ok();
        return Ok(());
    }

    app.emit("pymupdf:missing", ()).ok();
    app.emit("pymupdf:installing", ()).ok();

    // Try standalone pip tools first
    for pip_name in &["pip3", "pip"] {
        if let Ok(pip_path) = which::which(pip_name) {
            let ok = tokio::process::Command::new(&pip_path)
                .args(["install", "pymupdf4llm"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                if let Some(path) = find_pymupdf4llm_python() {
                    PYMUPDF4LLM_PYTHON.set(Some(path)).ok();
                    app.emit("pymupdf:installed", ()).ok();
                    return Ok(());
                }
            }
        }
    }

    // Fall back to python -m pip
    #[cfg(not(target_os = "windows"))]
    let python_names = vec!["python3", "python"];
    #[cfg(target_os = "windows")]
    let python_names = vec!["python3", "python", "py"];

    for python_name in &python_names {
        if let Ok(python_path) = which::which(python_name) {
            let ok = tokio::process::Command::new(&python_path)
                .args(["-m", "pip", "install", "pymupdf4llm"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                if let Some(path) = find_pymupdf4llm_python() {
                    PYMUPDF4LLM_PYTHON.set(Some(path)).ok();
                    app.emit("pymupdf:installed", ()).ok();
                    return Ok(());
                }
            }
        }
    }

    let err = "pymupdf4llm not found — install with: pip install pymupdf4llm".to_string();
    PYMUPDF4LLM_PYTHON.set(None).ok();
    app.emit("pymupdf:failed", err).ok();
    Ok(())
}

pub fn get_pymupdf4llm_python() -> Result<PathBuf, String> {
    match PYMUPDF4LLM_PYTHON.get() {
        Some(Some(p)) => Ok(p.clone()),
        _ => find_pymupdf4llm_python().ok_or_else(|| {
            "pymupdf4llm not found — install with: pip install pymupdf4llm".to_string()
        }),
    }
}

pub fn get_pandoc() -> Result<PathBuf, String> {
    match PANDOC_PATH.get() {
        Some(Some(p)) => Ok(p.clone()),
        _ => find_pandoc_binary().ok_or_else(|| {
            "pandoc not found — install it to enable document conversion".to_string()
        }),
    }
}

pub fn detect_pdf_engine() -> Option<&'static str> {
    const ENGINES: &[&str] = &["tectonic", "xelatex", "pdflatex", "lualatex", "wkhtmltopdf"];
    for engine in ENGINES {
        if which::which(engine).is_ok() {
            return Some(engine);
        }
        #[cfg(target_os = "macos")]
        for dir in BREW_PATHS {
            if std::path::Path::new(dir).join(engine).exists() {
                return Some(engine);
            }
        }
    }
    None
}

pub fn find_any_binary(names: &[&str]) -> Option<PathBuf> {
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

pub fn find_marker_binary() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates: &[&str] = &["marker", "marker_single"];
    if let Some(p) = find_any_binary(candidates) {
        return Some(p);
    }
    #[cfg(target_os = "macos")]
    for ver in ["3.13", "3.12", "3.11", "3.10"] {
        for name in candidates {
            let p = PathBuf::from(format!("{home}/Library/Python/{ver}/bin/{name}"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    for name in candidates {
        let p = PathBuf::from(format!("{home}/.local/bin/{name}"));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

pub fn marker_available() -> bool {
    find_marker_binary().is_some()
}

pub fn marker_step(app: &tauri::AppHandle, msg: &str) {
    app.emit("marker:step", msg).ok();
}

pub async fn run_silent(program: &PathBuf, args: &[&str]) -> Result<(), String> {
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

pub async fn install_marker(app: &tauri::AppHandle) -> Result<(), String> {
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

    if let Some(ref pipx) = pipx {
        marker_step(app, "Downloading marker-pdf — this may take a few minutes…");
        match run_silent(pipx, &["install", "marker-pdf"]).await {
            Ok(()) => {
                run_silent(pipx, &["inject", "marker-pdf", "psutil"]).await.ok();
                return Ok(());
            }
            Err(e) => {
                marker_step(app, &format!("pipx install failed ({e}), trying pip…"));
            }
        }
    }

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

    if let Some(ref pip) = pip {
        marker_step(app, "Downloading marker-pdf — this may take a few minutes…");
        return run_silent(pip, &["install", "--user", "marker-pdf", "psutil"]).await
            .map_err(|e| format!("Installation failed: {e}"));
    }

    Err("Could not install marker-pdf automatically. Please install pipx or pip and try again.".to_string())
}
