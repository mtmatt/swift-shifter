use std::path::PathBuf;

const MANIFEST: &str = include_str!("../../tools.toml");

#[derive(serde::Deserialize)]
struct Manifest {
    ffmpeg:  ToolSpec,
    pandoc:  ToolSpec,
    calibre: ToolSpec,
}

#[derive(serde::Deserialize)]
struct ToolSpec {
    version: String,
    #[serde(rename = "linux-x86_64")]
    linux_x86_64: Option<PlatformEntry>,
    #[serde(rename = "linux-aarch64")]
    linux_aarch64: Option<PlatformEntry>,
    #[serde(rename = "windows-x86_64")]
    windows_x86_64: Option<PlatformEntry>,
}

#[derive(serde::Deserialize, Clone)]
pub struct PlatformEntry {
    pub url:      String,
    pub sha256:   String,
    pub archive:  String,   // "tar.gz" | "tar.xz" | "zip"
    pub mode:     String,   // "binary" | "dir"
    pub binary:   String,   // path-in-archive (binary mode) or filename within dest_dir (dir mode)
    pub dest_dir: Option<String>, // dir mode only
}

fn parse_manifest() -> Result<Manifest, String> {
    toml::from_str(MANIFEST).map_err(|e| format!("tools.toml: {e}"))
}

fn get_platform_entry(spec: &ToolSpec) -> Option<PlatformEntry> {
    get_current_entry(spec)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn get_current_entry(spec: &ToolSpec) -> Option<PlatformEntry> { spec.linux_x86_64.clone() }

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn get_current_entry(spec: &ToolSpec) -> Option<PlatformEntry> { spec.linux_aarch64.clone() }

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
fn get_current_entry(spec: &ToolSpec) -> Option<PlatformEntry> { spec.windows_x86_64.clone() }

#[cfg(not(any(
    all(target_os = "linux",   target_arch = "x86_64"),
    all(target_os = "linux",   target_arch = "aarch64"),
    all(target_os = "windows", target_arch = "x86_64"),
)))]
fn get_current_entry(_spec: &ToolSpec) -> Option<PlatformEntry> { None }

pub fn user_tool_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("swift-shifter")
}

pub fn tool_version(name: &str) -> Option<String> {
    let m = parse_manifest().ok()?;
    match name {
        "ffmpeg"  => Some(m.ffmpeg.version),
        "pandoc"  => Some(m.pandoc.version),
        "calibre" => Some(m.calibre.version),
        _         => None,
    }
}

pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), String> {
    // Skip verification when sha256 is not yet populated in tools.toml
    if expected.is_empty() {
        return Ok(());
    }
    use sha2::{Digest, Sha256};
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(format!("SHA256 mismatch: expected {expected}, got {actual}"));
    }
    Ok(())
}

fn extract_binary(bytes: &[u8], archive: &str, path_in_archive: &str) -> Result<Vec<u8>, String> {
    match archive {
        "tar.gz"  => extract_from_tar(
            flate2::read::GzDecoder::new(std::io::Cursor::new(bytes)),
            path_in_archive,
        ),
        "tar.xz"  => extract_from_tar(
            xz2::read::XzDecoder::new(std::io::Cursor::new(bytes)),
            path_in_archive,
        ),
        "zip"     => extract_from_zip(bytes, path_in_archive),
        other     => Err(format!("unsupported archive type: {other}")),
    }
}

fn extract_from_tar<R: std::io::Read>(reader: R, path_in_archive: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let entry_path = entry.path().map_err(|e| e.to_string())?;
        if entry_path.to_string_lossy() == path_in_archive {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            return Ok(buf);
        }
    }
    Err(format!("{path_in_archive} not found in archive"))
}

fn extract_from_zip(bytes: &[u8], path_in_archive: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| e.to_string())?;
    let mut file = archive
        .by_name(path_in_archive)
        .map_err(|_| format!("{path_in_archive} not found in zip"))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn extract_dir(bytes: &[u8], archive: &str, dest: &std::path::Path) -> Result<(), String> {
    match archive {
        "tar.gz"  => {
            let dec = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
            tar::Archive::new(dec)
                .unpack(dest)
                .map_err(|e| format!("tar.gz unpack: {e}"))
        }
        "tar.xz"  => {
            let dec = xz2::read::XzDecoder::new(std::io::Cursor::new(bytes));
            tar::Archive::new(dec)
                .unpack(dest)
                .map_err(|e| format!("tar.xz unpack: {e}"))
        }
        "zip"     => {
            let cursor = std::io::Cursor::new(bytes);
            zip::ZipArchive::new(cursor)
                .map_err(|e| e.to_string())?
                .extract(dest)
                .map_err(|e| format!("zip extract: {e}"))
        }
        other => Err(format!("unsupported archive type: {other}")),
    }
}

#[cfg(unix)]
fn make_executable(dir: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("chmod {path:?}: {e}"))?;
        }
    }
    Ok(())
}

async fn download_bytes(
    app: &tauri::AppHandle,
    tool_name: &str,
    url: &str,
) -> Result<Vec<u8>, String> {
    use futures_util::StreamExt;
    use tauri::Emitter;

    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {tool_name}", resp.status()));
    }

    let total = resp.content_length();
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        buf.extend_from_slice(&chunk);
        if let Some(t) = total {
            let pct = (buf.len() as f64 / t as f64 * 100.0) as u32;
            app.emit(
                "install:download-progress",
                serde_json::json!({ "tool": tool_name, "percent": pct }),
            )
            .ok();
        }
    }

    Ok(buf)
}

pub async fn ensure_tool(app: &tauri::AppHandle, tool_name: &str) -> Result<PathBuf, String> {
    use tauri::Emitter;

    let manifest = parse_manifest()?;
    let spec = match tool_name {
        "ffmpeg"  => &manifest.ffmpeg,
        "pandoc"  => &manifest.pandoc,
        "calibre" => &manifest.calibre,
        other     => return Err(format!("unknown tool: {other}")),
    };

    let entry = get_platform_entry(spec)
        .ok_or_else(|| format!("{tool_name}: no download entry for this platform/arch"))?;

    match entry.mode.as_str() {
        "binary" => ensure_binary_mode(app, tool_name, &entry).await,
        "dir"    => ensure_dir_mode(app, tool_name, &entry).await,
        m        => Err(format!("{tool_name}: unknown mode {m}")),
    }
}

async fn ensure_binary_mode(
    app: &tauri::AppHandle,
    tool_name: &str,
    entry: &PlatformEntry,
) -> Result<PathBuf, String> {
    use tauri::Emitter;

    let file_name = std::path::Path::new(&entry.binary)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(tool_name);

    let dest = user_tool_dir().join("bin").join(file_name);
    if dest.exists() {
        return Ok(dest);
    }

    app.emit(&format!("{tool_name}:installing"), ()).ok();

    let bytes = download_bytes(app, tool_name, &entry.url).await?;
    verify_sha256(&bytes, &entry.sha256)?;
    let binary_bytes = extract_binary(&bytes, &entry.archive, &entry.binary)?;

    std::fs::create_dir_all(user_tool_dir().join("bin"))
        .map_err(|e| format!("create bin dir: {e}"))?;
    std::fs::write(&dest, &binary_bytes).map_err(|e| format!("write {tool_name}: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {tool_name}: {e}"))?;
    }

    app.emit(&format!("{tool_name}:installed"), ()).ok();
    Ok(dest)
}

async fn ensure_dir_mode(
    app: &tauri::AppHandle,
    tool_name: &str,
    entry: &PlatformEntry,
) -> Result<PathBuf, String> {
    use tauri::Emitter;

    let dest_dir_name = entry
        .dest_dir
        .as_deref()
        .ok_or_else(|| format!("{tool_name}: missing dest_dir"))?;

    let dest_dir = user_tool_dir().join(dest_dir_name);
    let binary_path = dest_dir.join(&entry.binary);

    if binary_path.exists() {
        return Ok(binary_path);
    }

    app.emit(&format!("{tool_name}:installing"), ()).ok();

    let bytes = download_bytes(app, tool_name, &entry.url).await?;
    verify_sha256(&bytes, &entry.sha256)?;

    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("create dir: {e}"))?;
    extract_dir(&bytes, &entry.archive, &dest_dir)?;

    #[cfg(unix)]
    make_executable(&dest_dir)?;

    app.emit(&format!("{tool_name}:installed"), ()).ok();
    Ok(binary_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_tar_gz(name: &str, content: &[u8]) -> Vec<u8> {
        use flate2::{write::GzEncoder, Compression};
        let enc = GzEncoder::new(Vec::new(), Compression::default());
        let mut tar = tar::Builder::new(enc);
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(content.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        tar.append_data(&mut hdr, name, content).unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn make_tar_xz(name: &str, content: &[u8]) -> Vec<u8> {
        use xz2::write::XzEncoder;
        let enc = XzEncoder::new(Vec::new(), 1);
        let mut tar = tar::Builder::new(enc);
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(content.len() as u64);
        hdr.set_mode(0o644);
        hdr.set_cksum();
        tar.append_data(&mut hdr, name, content).unwrap();
        tar.into_inner().unwrap().finish().unwrap()
    }

    fn make_zip(name: &str, content: &[u8]) -> Vec<u8> {
        use std::io::Write;
        use zip::{write::SimpleFileOptions, ZipWriter};
        let mut z = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        z.start_file(name, SimpleFileOptions::default()).unwrap();
        z.write_all(content).unwrap();
        z.finish().unwrap().into_inner()
    }

    // ── manifest ─────────────────────────────────────────────────────────────

    #[test]
    fn manifest_parses_without_error() {
        parse_manifest().unwrap();
    }

    #[test]
    fn user_tool_dir_is_under_home() {
        let dir = user_tool_dir();
        let home = dirs::home_dir().unwrap();
        assert!(dir.starts_with(&home), "{dir:?} should be under home {home:?}");
    }

    #[test]
    fn tool_version_returns_pandoc_version() {
        let v = tool_version("pandoc").unwrap();
        assert!(!v.is_empty());
    }

    // ── sha256 ───────────────────────────────────────────────────────────────

    #[test]
    fn sha256_accepts_correct_hash() {
        verify_sha256(b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855").unwrap();
    }

    #[test]
    fn sha256_rejects_wrong_hash() {
        let err = verify_sha256(b"", "0000000000000000000000000000000000000000000000000000000000000000");
        assert!(err.is_err(), "should reject wrong hash");
    }

    #[test]
    fn sha256_skips_empty_expected() {
        // Empty sha256 in tools.toml means "not yet computed — skip verification"
        verify_sha256(b"anything", "").unwrap();
    }

    // ── extract binary ────────────────────────────────────────────────────────

    #[test]
    fn extract_binary_from_tar_gz() {
        let data = b"#!/bin/sh\necho hi";
        let archive = make_tar_gz("dir/mybinary", data);
        let out = extract_binary(&archive, "tar.gz", "dir/mybinary").unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn extract_binary_from_tar_xz() {
        let data = b"hello xz";
        let archive = make_tar_xz("bin/tool", data);
        let out = extract_binary(&archive, "tar.xz", "bin/tool").unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn extract_binary_from_zip() {
        let data = b"zip content";
        let archive = make_zip("tools/mybin.exe", data);
        let out = extract_binary(&archive, "zip", "tools/mybin.exe").unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn extract_binary_missing_path_returns_err() {
        let archive = make_tar_gz("other/path", b"x");
        let result = extract_binary(&archive, "tar.gz", "does/not/exist");
        assert!(result.is_err());
    }

    // ── extract dir ───────────────────────────────────────────────────────────

    #[test]
    fn extract_dir_tar_gz_writes_files() {
        use flate2::{write::GzEncoder, Compression};
        let enc = GzEncoder::new(Vec::new(), Compression::default());
        let mut tar = tar::Builder::new(enc);
        for (name, content) in [("alpha", b"aaa" as &[u8]), ("beta", b"bbb")] {
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(content.len() as u64);
            hdr.set_mode(0o644);
            hdr.set_cksum();
            tar.append_data(&mut hdr, name, content).unwrap();
        }
        let gz_bytes = tar.into_inner().unwrap().finish().unwrap();

        let dest = tempfile::tempdir().unwrap();
        extract_dir(&gz_bytes, "tar.gz", dest.path()).unwrap();
        assert_eq!(std::fs::read(dest.path().join("alpha")).unwrap(), b"aaa");
        assert_eq!(std::fs::read(dest.path().join("beta")).unwrap(), b"bbb");
    }
}
