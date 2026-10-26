use crate::converter::document::{PANDOC_PATH, PYMUPDF4LLM_PYTHON};
use crate::converter::tools::Tool;
use std::path::PathBuf;

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
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let bin_name = if cfg!(target_os = "windows") {
            "pandoc.exe"
        } else {
            "pandoc"
        };
        let candidate = crate::converter::tools::earlier_download_dir()
            .join("bin")
            .join(bin_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Where pipx may keep its venvs: `PIPX_HOME` when it's set; otherwise the
/// legacy `~/.local/pipx` (and `~\pipx` on Windows), which pipx keeps
/// using where it exists, then its default under `data_dir`.
fn pipx_homes(
    pipx_home: Option<PathBuf>,
    home: Option<PathBuf>,
    data_dir: Option<PathBuf>,
) -> Vec<PathBuf> {
    if let Some(dir) = pipx_home {
        return vec![dir];
    }
    let mut homes = Vec::new();
    if let Some(home) = home {
        homes.push(home.join(".local").join("pipx"));
        if cfg!(target_os = "windows") {
            homes.push(home.join("pipx"));
        }
    }
    homes.extend(data_dir);
    homes
}

/// pipx's default home: under the user's data directory, which is
/// `~/.local/share` on Linux and `~/Library/Application Support` on macOS.
/// On Windows it's `AppData\Local\pipx\pipx`.
fn pipx_data_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        dirs::data_local_dir().map(|d| d.join("pipx").join("pipx"))
    } else {
        dirs::data_dir().map(|d| d.join("pipx"))
    }
}

fn python_has_pymupdf4llm(python: &PathBuf) -> bool {
    crate::process::sync_command(python)
        .args(["-c", "import pymupdf4llm"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn find_pymupdf4llm_python() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // Check pipx venvs first: that's how the install hint installs it.
    let pipx_homes = pipx_homes(
        std::env::var_os("PIPX_HOME").map(PathBuf::from),
        dirs::home_dir(),
        pipx_data_dir(),
    );
    for pipx_home in pipx_homes {
        #[cfg(not(target_os = "windows"))]
        let venv_python = pipx_home
            .join("venvs")
            .join("pymupdf4llm")
            .join("bin")
            .join("python");
        #[cfg(target_os = "windows")]
        let venv_python = pipx_home
            .join("venvs")
            .join("pymupdf4llm")
            .join("Scripts")
            .join("python.exe");
        if venv_python.exists() {
            candidates.push(venv_python);
        }
    }

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
    for path in &[
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python",
    ] {
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

    candidates.into_iter().find(python_has_pymupdf4llm)
}

pub fn get_pymupdf4llm_python() -> Result<PathBuf, String> {
    Tool::Pymupdf4llm.locate(&PYMUPDF4LLM_PYTHON)
}

pub fn get_pandoc() -> Result<PathBuf, String> {
    Tool::Pandoc.locate(&PANDOC_PATH)
}

pub fn get_marker() -> Result<PathBuf, String> {
    Tool::Marker.require(find_marker_binary())
}

pub fn find_any_binary(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if let Ok(p) = which::which(name) {
            return Some(p);
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let extra: &[&str] = &[
        "/opt/homebrew/bin",
        "/usr/local/bin",
        &format!("{home}/.local/bin"),
    ];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipx_venvs_are_looked_for_where_pipx_puts_them() {
        let home = PathBuf::from("/home/u");
        let data = PathBuf::from("/home/u/.local/share/pipx");

        // PIPX_HOME, when it's set, is the only place.
        let set = PathBuf::from("/opt/pipx");
        let homes = pipx_homes(Some(set.clone()), Some(home.clone()), Some(data.clone()));
        assert_eq!(homes, [set]);

        // Otherwise the legacy home, then the default under the data directory.
        let homes = pipx_homes(None, Some(home), Some(data.clone()));
        assert_eq!(homes.first(), Some(&PathBuf::from("/home/u/.local/pipx")));
        assert_eq!(homes.last(), Some(&data));
    }
}
