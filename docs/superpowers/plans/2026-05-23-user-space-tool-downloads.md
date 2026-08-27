# User-Space Tool Downloads Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace all Linux `pkexec` (sudo) calls and add user-space binary downloads for Linux/Windows so ffmpeg, pandoc, and ebook-convert install without elevation.

**Architecture:** A new `downloader.rs` module reads a compile-time-embedded `tools.toml` manifest (pinned versions + per-platform URLs + SHA256 checksums), downloads missing binaries to `~/.local/share/swift-shifter/` (Linux) or `%LOCALAPPDATA%\swift-shifter\` (Windows), verifies checksums, and extracts them. Existing `ensure_*` functions on Linux/Windows call this module instead of `pkexec`. macOS Homebrew paths are untouched.

**Tech Stack:** Rust, `reqwest` (already in Cargo), `sha2 0.10`, `flate2 1`, `tar 0.4`, `xz2 0.1` (static feature), `zip 2`, `toml` (already in Cargo), `dirs` (already in Cargo).

---

## File Map

| Action | Path | Responsibility |
|--------|------|---------------|
| CREATE | `tools.toml` | Pinned versions + download URLs + SHA256 per platform |
| CREATE | `swift-shifter/src/downloader.rs` | Manifest parsing, download, verify, extract |
| MODIFY | `swift-shifter/Cargo.toml` | Add sha2, flate2, tar, xz2, zip |
| MODIFY | `swift-shifter/src/main.rs` | Add `mod downloader;` |
| MODIFY | `swift-shifter/src/converter/media.rs` | ensure_ffmpeg Linux/Windows arm + find check |
| MODIFY | `swift-shifter/src/converter/document/binaries.rs` | ensure_pandoc/ebook_convert Linux/Windows + remove pkexec from install_marker |

---

## Task 1: Add Cargo dependencies

**Files:**
- Modify: `swift-shifter/Cargo.toml`

- [ ] **Add five new crates under `[dependencies]`**

Open `swift-shifter/Cargo.toml` and add after the `reqwest` line:

```toml
sha2   = "0.10"
flate2 = "1"
tar    = "0.4"
xz2    = { version = "0.1", features = ["static"] }
zip    = "2"
```

The `static` feature on `xz2` compiles liblzma from source (bundled) so no system `liblzma-dev` is needed in CI.

- [ ] **Verify dependencies resolve**

```bash
cd swift-shifter && cargo fetch
```

Expected: fetches packages, no errors. If `xz2` fails on macOS because Xcode tools are absent, that's an environment issue — not a code issue.

- [ ] **Commit**

```bash
git add swift-shifter/Cargo.toml
git commit -m "build: add sha2, flate2, tar, xz2, zip for user-space tool downloads"
```

---

## Task 2: Create `tools.toml` with pinned download entries

**Files:**
- Create: `tools.toml` (repo root)

This file is embedded at compile time in `downloader.rs`. Each entry pins a version and provides one download per platform/arch. Two modes exist: `binary` (extract a single file) and `dir` (extract entire archive into a subdirectory — used for Calibre, which bundles its own shared libs).

- [ ] **Write the initial tools.toml with placeholder SHA256 values**

Create `/Users/matt/Programming/swift-shifter/tools.toml`:

```toml
# External tool version manifest.
# Bump version/url/sha256 here when upgrading a tool — no Rust changes needed.
# sha256 values are lowercase hex. Compute with: curl -sL <url> | sha256sum

[ffmpeg]
version = "7.1"

[ffmpeg.linux-x86_64]
url     = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2025-04-30-12-36/ffmpeg-n7.1-latest-linux64-gpl.tar.xz"
sha256  = ""
archive = "tar.xz"
mode    = "binary"
binary  = "ffmpeg-n7.1-latest-linux64-gpl/bin/ffmpeg"

[ffmpeg.linux-aarch64]
url     = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2025-04-30-12-36/ffmpeg-n7.1-latest-linuxarm64-gpl.tar.xz"
sha256  = ""
archive = "tar.xz"
mode    = "binary"
binary  = "ffmpeg-n7.1-latest-linuxarm64-gpl/bin/ffmpeg"

[ffmpeg.windows-x86_64]
url     = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2025-04-30-12-36/ffmpeg-n7.1-latest-win64-gpl.zip"
sha256  = ""
archive = "zip"
mode    = "binary"
binary  = "ffmpeg-n7.1-latest-win64-gpl/bin/ffmpeg.exe"

[pandoc]
version = "3.6.4"

[pandoc.linux-x86_64]
url     = "https://github.com/jgm/pandoc/releases/download/3.6.4/pandoc-3.6.4-linux-amd64.tar.gz"
sha256  = ""
archive = "tar.gz"
mode    = "binary"
binary  = "pandoc-3.6.4/bin/pandoc"

[pandoc.linux-aarch64]
url     = "https://github.com/jgm/pandoc/releases/download/3.6.4/pandoc-3.6.4-linux-arm64.tar.gz"
sha256  = ""
archive = "tar.gz"
mode    = "binary"
binary  = "pandoc-3.6.4/bin/pandoc"

[pandoc.windows-x86_64]
url     = "https://github.com/jgm/pandoc/releases/download/3.6.4/pandoc-3.6.4-windows-x86_64.zip"
sha256  = ""
archive = "zip"
mode    = "binary"
binary  = "pandoc-3.6.4/pandoc.exe"

[calibre]
version = "7.26.0"

[calibre.linux-x86_64]
url      = "https://download.calibre-ebook.com/7.26.0/calibre-7.26.0-x86_64.txz"
sha256   = ""
archive  = "tar.xz"
mode     = "dir"
dest_dir = "calibre"
binary   = "ebook-convert"

[calibre.linux-aarch64]
url      = "https://download.calibre-ebook.com/7.26.0/calibre-7.26.0-aarch64.txz"
sha256   = ""
archive  = "tar.xz"
mode     = "dir"
dest_dir = "calibre"
binary   = "ebook-convert"
```

- [ ] **Verify URLs exist (spot-check two)**

```bash
curl -sI "https://github.com/jgm/pandoc/releases/download/3.6.4/pandoc-3.6.4-linux-amd64.tar.gz" | head -5
curl -sI "https://download.calibre-ebook.com/7.26.0/calibre-7.26.0-x86_64.txz" | head -5
```

Expected: `HTTP/2 302` (GitHub redirect) or `HTTP/2 200`. If you get 404, the version doesn't exist — find the latest release on each project's GitHub releases page and update the URL + version.

- [ ] **Verify the BtbN ffmpeg autobuild tag exists**

```bash
curl -sI "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2025-04-30-12-36/ffmpeg-n7.1-latest-linux64-gpl.tar.xz" | head -5
```

If 404, open `https://github.com/BtbN/FFmpeg-Builds/releases` and find the latest `autobuild-*` tag that has `ffmpeg-n7.1-*` files. Update the URL in `tools.toml`.

- [ ] **Compute and fill in SHA256 checksums (takes a few minutes — files are large)**

Run each command, copy the 64-char hex hash it prints, and paste it as the `sha256` value in `tools.toml`:

```bash
# ffmpeg linux x86_64
curl -sL "$(grep -A5 '\[ffmpeg.linux-x86_64\]' tools.toml | grep url | cut -d'"' -f2)" | sha256sum

# pandoc linux x86_64
curl -sL "$(grep -A5 '\[pandoc.linux-x86_64\]' tools.toml | grep url | cut -d'"' -f2)" | sha256sum

# pandoc linux aarch64
curl -sL "$(grep -A5 '\[pandoc.linux-aarch64\]' tools.toml | grep url | cut -d'"' -f2)" | sha256sum

# pandoc windows x86_64
curl -sL "$(grep -A5 '\[pandoc.windows-x86_64\]' tools.toml | grep url | cut -d'"' -f2)" | sha256sum

# calibre linux x86_64
curl -sL "$(grep -A6 '\[calibre.linux-x86_64\]' tools.toml | grep url | cut -d'"' -f2)" | sha256sum

# calibre linux aarch64
curl -sL "$(grep -A6 '\[calibre.linux-aarch64\]' tools.toml | grep url | cut -d'"' -f2)" | sha256sum
```

Skip ffmpeg arm64 and windows for now if bandwidth is a concern — those can be filled before the first aarch64/Windows release.

- [ ] **Verify the Calibre archive has binaries at the root (no top-level directory wrapper)**

```bash
curl -sL "https://download.calibre-ebook.com/7.26.0/calibre-7.26.0-x86_64.txz" | tar -tJ | head -20
```

Expected: entries like `ebook-convert`, `calibre`, `python/` — not `calibre-7.26.0/ebook-convert`. If there IS a top-level directory, update `binary` in `tools.toml` to `calibre-7.26.0/ebook-convert`.

- [ ] **Verify the ffmpeg archive has the expected binary path**

```bash
curl -sL "<ffmpeg-linux-x86_64-url>" | tar -tJ | grep "bin/ffmpeg$"
```

Expected output: `ffmpeg-n7.1-latest-linux64-gpl/bin/ffmpeg`. If different, update the `binary` field.

- [ ] **Commit**

```bash
git add tools.toml
git commit -m "feat: add tools.toml with pinned ffmpeg/pandoc/calibre versions"
```

---

## Task 3: Create `downloader.rs` — types, manifest, `user_tool_dir`

**Files:**
- Create: `swift-shifter/src/downloader.rs`
- Test in: `swift-shifter/src/downloader.rs` (inline `#[cfg(test)]` module)

- [ ] **Write the failing test**

Create `swift-shifter/src/downloader.rs` with just the test first:

```rust
use std::path::PathBuf;

const MANIFEST: &str = include_str!("../../tools.toml");

#[cfg(test)]
mod tests {
    use super::*;

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
}
```

- [ ] **Run to verify they fail**

```bash
cd swift-shifter && cargo test downloader 2>&1 | tail -20
```

Expected: compile error `cannot find function 'parse_manifest'`.

- [ ] **Write the types and functions**

Complete `swift-shifter/src/downloader.rs` (replace the stub):

```rust
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
```

- [ ] **Run tests — expect failure because `mod downloader` not wired yet**

```bash
cd swift-shifter && cargo test --lib -p swift-shifter 2>&1 | grep -E "error|FAILED|ok"
```

The tests themselves can't run yet because `main.rs` doesn't declare `mod downloader;`. That's Task 8. For now, just verify the file compiles in isolation:

```bash
cd swift-shifter && cargo check 2>&1 | head -30
```

Expected: compile errors about `mod downloader` not found — that's fine, we add it in Task 8. If there are type errors in `downloader.rs` itself, fix those now.

---

## Task 4: Add `verify_sha256` with unit test

**Files:**
- Modify: `swift-shifter/src/downloader.rs`

- [ ] **Write the failing test first** (add to the `tests` module in `downloader.rs`)

```rust
    #[test]
    fn sha256_accepts_correct_hash() {
        // SHA256 of the empty byte string
        verify_sha256(b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855").unwrap();
    }

    #[test]
    fn sha256_rejects_wrong_hash() {
        let err = verify_sha256(b"", "0000000000000000000000000000000000000000000000000000000000000000");
        assert!(err.is_err(), "should reject wrong hash");
    }
```

- [ ] **Add `verify_sha256` to `downloader.rs`** (above the `#[cfg(test)]` module)

```rust
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(format!("SHA256 mismatch: expected {expected}, got {actual}"));
    }
    Ok(())
}
```

- [ ] **Run tests (after Task 8 wires the module, or run in isolation)**

```bash
cd swift-shifter && cargo test verify_sha256 2>&1
```

If `mod downloader` isn't wired yet, run:

```bash
cd swift-shifter && rustc --edition 2024 --test src/downloader.rs \
  --extern sha2=$(find target -name 'libsha2-*.rlib' | head -1) 2>&1 | head -20
```

Alternatively defer running until after Task 8. The test code is correct; proceed.

---

## Task 5: Add archive extraction — binary mode — with tests

**Files:**
- Modify: `swift-shifter/src/downloader.rs`

- [ ] **Write the failing tests** (add to the `tests` module)

```rust
    fn make_tar_gz(name: &str, content: &[u8]) -> Vec<u8> {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let buf = Vec::new();
        let enc = GzEncoder::new(buf, Compression::default());
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
        let buf = Vec::new();
        let enc = XzEncoder::new(buf, 1);
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
        let buf = std::io::Cursor::new(Vec::new());
        let mut z = ZipWriter::new(buf);
        z.start_file(name, SimpleFileOptions::default()).unwrap();
        z.write_all(content).unwrap();
        z.finish().unwrap().into_inner()
    }

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
```

- [ ] **Add `extract_binary` and helpers to `downloader.rs`** (above the tests module)

```rust
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
```

---

## Task 6: Add archive extraction — dir mode — with test

**Files:**
- Modify: `swift-shifter/src/downloader.rs`

- [ ] **Add `tempfile` to dev-dependencies in `swift-shifter/Cargo.toml`**

```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Write the failing test** (add to the tests module)

```rust
    #[test]
    fn extract_dir_tar_gz_writes_files() {
        use std::io::Write;
        // Build a tar.gz with two files
        let buf = Vec::new();
        let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
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
```

This test uses `tempfile`. Add to `Cargo.toml` under `[dev-dependencies]`:

```toml
tempfile = "3"
```

- [ ] **Add `extract_dir` to `downloader.rs`** (above the tests module)

```rust
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
```

---

## Task 7: Add `ensure_tool` and `ensure_tool_dir` public async functions

**Files:**
- Modify: `swift-shifter/src/downloader.rs`

These are the two public entry points called by the rest of the codebase.

- [ ] **Add the download helper and public functions** (above the `verify_sha256` function)

```rust
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
```

- [ ] **Run all downloader unit tests (after Task 8 wires the module)**

Defer running until after Task 8. Note for later:

```bash
cd swift-shifter && cargo test downloader::tests 2>&1
```

Expected: all tests pass.

---

## Task 8: Wire `mod downloader` into `main.rs` + compile check

**Files:**
- Modify: `swift-shifter/src/main.rs`

- [ ] **Add `mod downloader;` near the top of main.rs**

Open `swift-shifter/src/main.rs`. After the existing `mod` declarations at the top (look for `mod config;`, `mod converter;`, etc.), add:

```rust
mod downloader;
```

- [ ] **Compile check**

```bash
cd swift-shifter && cargo check 2>&1 | grep -E "^error"
```

Expected: no errors. If you see `include_str!` path errors, verify the path `../../tools.toml` resolves from `swift-shifter/src/downloader.rs` to the repo root.

- [ ] **Run all downloader unit tests**

```bash
cd swift-shifter && cargo test downloader 2>&1
```

Expected: all tests pass (manifest_parses_without_error, user_tool_dir_is_under_home, tool_version_returns_pandoc_version, sha256_accepts_correct_hash, sha256_rejects_wrong_hash, extract_binary_from_tar_gz, extract_binary_from_tar_xz, extract_binary_from_zip, extract_binary_missing_path_returns_err, extract_dir_tar_gz_writes_files).

- [ ] **Commit**

```bash
git add swift-shifter/src/downloader.rs swift-shifter/src/main.rs swift-shifter/Cargo.toml
git commit -m "feat: add downloader module with verify + extract (no pkexec)"
```

---

## Task 9: Update `ensure_ffmpeg` for Linux and Windows

**Files:**
- Modify: `swift-shifter/src/converter/media.rs`

- [ ] **Add user-dir check to `find_ffmpeg_binary`**

In `find_ffmpeg_binary()`, after the macOS block (around line 113), add:

```rust
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let bin_name = if cfg!(target_os = "windows") { "ffmpeg.exe" } else { "ffmpeg" };
        let candidate = crate::downloader::user_tool_dir().join("bin").join(bin_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
```

- [ ] **Add Linux/Windows download arm to `ensure_ffmpeg`**

In `ensure_ffmpeg()`, after the closing `}` of the `#[cfg(target_os = "macos")]` block (around line 190), add:

```rust
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        match crate::downloader::ensure_tool(app, "ffmpeg").await {
            Ok(path) => {
                FFMPEG_PATH.set(Some(path)).ok();
                app.emit("ffmpeg:installed", ()).ok();
                return Ok(());
            }
            Err(e) => {
                app.emit("ffmpeg:failed", &e).ok();
            }
        }
    }
```

- [ ] **Compile check**

```bash
cd swift-shifter && cargo check 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Commit**

```bash
git add swift-shifter/src/converter/media.rs
git commit -m "feat: ensure_ffmpeg downloads to user dir on Linux/Windows (no sudo)"
```

---

## Task 10: Update `ensure_pandoc` for Linux and Windows

**Files:**
- Modify: `swift-shifter/src/converter/document/binaries.rs`

- [ ] **Add user-dir check to `find_pandoc_binary`**

In `find_pandoc_binary()`, after the Windows block (around line 30), add before the `None` at the end:

```rust
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        let bin_name = if cfg!(target_os = "windows") { "pandoc.exe" } else { "pandoc" };
        let candidate = crate::downloader::user_tool_dir().join("bin").join(bin_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
```

- [ ] **Replace the Linux pkexec block in `ensure_pandoc` with a downloader call**

Find and **delete** the entire `#[cfg(target_os = "linux")]` block in `ensure_pandoc` (lines 162–197 in the current file — from `#[cfg(target_os = "linux")]` through the closing `}`).

Replace it with:

```rust
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    {
        app.emit("pandoc:installing", ()).ok();
        match crate::downloader::ensure_tool(app, "pandoc").await {
            Ok(path) => {
                PANDOC_PATH.set(Some(path)).ok();
                app.emit("pandoc:installed", ()).ok();
                return Ok(());
            }
            Err(e) => {
                app.emit("pandoc:failed", &e).ok();
            }
        }
    }
```

Also **delete** the existing `#[cfg(target_os = "windows")]` winget block (lines 139–160) — the new combined block above handles both.

- [ ] **Compile check**

```bash
cd swift-shifter && cargo check 2>&1 | grep -E "^error"
```

- [ ] **Commit**

```bash
git add swift-shifter/src/converter/document/binaries.rs
git commit -m "feat: ensure_pandoc downloads to user dir on Linux/Windows (no sudo)"
```

---

## Task 11: Update `ensure_ebook_convert` for Linux

**Files:**
- Modify: `swift-shifter/src/converter/document/binaries.rs`

- [ ] **Add user-dir check to `find_ebook_convert_binary`**

In `find_ebook_convert_binary()`, after the Linux block that checks `/usr/bin/ebook-convert` and `/usr/local/bin/ebook-convert`, add before `None`:

```rust
    #[cfg(target_os = "linux")]
    {
        let candidate = crate::downloader::user_tool_dir()
            .join("calibre")
            .join("ebook-convert");
        if candidate.exists() {
            return Some(candidate);
        }
    }
```

- [ ] **Replace the Linux pkexec block in `ensure_ebook_convert` with a downloader call**

Find and **delete** the `#[cfg(target_os = "linux")]` block in `ensure_ebook_convert` (the one with the `pkexec` + dnf/pacman/apt-get calls).

Replace it with:

```rust
    #[cfg(target_os = "linux")]
    {
        app.emit("ebook-convert:installing", ()).ok();
        match crate::downloader::ensure_tool(app, "calibre").await {
            Ok(path) => {
                EBOOK_CONVERT_PATH.set(Some(path)).ok();
                app.emit("ebook-convert:installed", ()).ok();
                return Ok(());
            }
            Err(e) => {
                app.emit("ebook-convert:failed", &e).ok();
            }
        }
    }
```

- [ ] **Pin the Windows winget calibre version**

Find the existing `#[cfg(target_os = "windows")]` block in `ensure_ebook_convert`. It currently calls:

```rust
tokio::process::Command::new("winget")
    .args(["install", "--id", "calibre.calibre", "-e", "--silent"])
```

Change it to include the pinned version from the manifest:

```rust
let version = crate::downloader::tool_version("calibre")
    .unwrap_or_default();
tokio::process::Command::new("winget")
    .args([
        "install", "--id", "calibre.calibre",
        "--version", &version,
        "--scope", "user",
        "-e", "--silent",
    ])
```

- [ ] **Compile check**

```bash
cd swift-shifter && cargo check 2>&1 | grep -E "^error"
```

- [ ] **Commit**

```bash
git add swift-shifter/src/converter/document/binaries.rs
git commit -m "feat: ensure_ebook_convert downloads calibre to user dir on Linux (no sudo)"
```

---

## Task 12: Remove `pkexec` from `install_marker` on Linux

**Files:**
- Modify: `swift-shifter/src/converter/document/binaries.rs`

`install_marker` has two `pkexec` callsites on Linux: one to install `pipx` and one to install `python3-pip`. Both are replaced with user-space pip fallbacks.

- [ ] **Replace the Linux pkexec block for pipx installation**

Find this block in `install_marker` (around line 587–594):

```rust
        #[cfg(target_os = "linux")]
        if which::which("dnf").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["dnf", "install", "-y", "pipx"]).await.ok();
        } else if which::which("pacman").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["pacman", "-S", "--noconfirm", "python-pipx"]).await.ok();
        } else if which::which("apt-get").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["apt-get", "install", "-y", "pipx"]).await.ok();
        }
```

Replace with:

```rust
        #[cfg(target_os = "linux")]
        if let Some(python) = find_any_binary(&["python3", "python"]) {
            // Install pipx to user space — no sudo needed
            run_silent(&python, &["-m", "pip", "install", "--user", "pipx"]).await.ok();
        }
```

- [ ] **Replace the Linux pkexec block for python3-pip installation**

Find this block (around line 627–633):

```rust
        #[cfg(target_os = "linux")]
        if which::which("dnf").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["dnf", "install", "-y", "python3-pip"]).await.ok();
        } else if which::which("pacman").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["pacman", "-S", "--noconfirm", "python-pip"]).await.ok();
        } else if which::which("apt-get").is_ok() {
            run_silent(&PathBuf::from("pkexec"), &["apt-get", "install", "-y", "python3-pip"]).await.ok();
        }
```

Delete the entire block. `pip` being absent on Linux is a signal that the user's Python setup is non-standard. The outer `if let Some(ref pip) = pip` fallback already handles missing pip gracefully.

- [ ] **Compile check**

```bash
cd swift-shifter && cargo check 2>&1 | grep -E "^error"
```

- [ ] **Confirm no `pkexec` references remain in the codebase**

```bash
grep -rn "pkexec" swift-shifter/src/
```

Expected: no output.

- [ ] **Commit**

```bash
git add swift-shifter/src/converter/document/binaries.rs
git commit -m "fix: remove pkexec from install_marker on Linux — use pip --user instead"
```

---

## Task 13: Full compile check across all targets

- [ ] **Compile for the local target**

```bash
cd swift-shifter && cargo build --release 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Cross-compile check for Linux x86_64 (if on macOS)**

```bash
rustup target add x86_64-unknown-linux-gnu
cd swift-shifter && cargo check --target x86_64-unknown-linux-gnu 2>&1 | grep -E "^error"
```

Expected: no errors. If a linker is missing, `cargo check` (not `build`) is sufficient for type/cfg correctness.

- [ ] **Verify `pkexec` is truly gone**

```bash
grep -rn "pkexec" swift-shifter/src/ tools.toml
```

Expected: no output.

- [ ] **Run the full unit test suite**

```bash
cd swift-shifter && cargo test 2>&1
```

Expected: all tests pass.

- [ ] **Build the Tauri app in no-bundle mode (fastest end-to-end compile check)**

```bash
npm run tauri -- build --no-bundle 2>&1 | tail -10
```

Expected: `Finished release [optimized] target(s)`.

- [ ] **Commit**

```bash
git add -A
git commit -m "chore: final compile verification — user-space tool downloads complete"
```
