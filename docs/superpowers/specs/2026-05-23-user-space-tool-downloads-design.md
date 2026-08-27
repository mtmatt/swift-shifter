# Design: User-Space Tool Downloads (No Sudo)

**Date:** 2026-05-23  
**Branch:** fix/user-space-tool-downloads  
**Status:** Approved

## Problem

Swift Shifter shells out to external tools (ffmpeg, pandoc, ebook-convert/Calibre) that it auto-installs when missing. On Linux the install paths all call `pkexec` (graphical sudo) to run `apt-get`, `dnf`, or `pacman` — requiring elevated privileges the user should not need to grant. On Linux ffmpeg has no install fallback at all. Neither Linux nor Windows paths have locked tool versions.

## Goals

- No `pkexec` / sudo on Linux, ever
- No UAC elevation on Windows for ffmpeg and pandoc (Calibre keeps winget)
- Tool versions locked in a single repo-root manifest (`tools.toml`)
- macOS Homebrew flow unchanged

## Architecture

```
tools.toml  (repo root)
  └── pinned versions + per-platform download URLs + SHA256 checksums
        ↓  include_str! at compile time, parsed at runtime
swift-shifter/src/downloader.rs
  ├── ensure_tool(app, "ffmpeg")      → user data dir / bin / ffmpeg
  ├── ensure_tool(app, "pandoc")      → user data dir / bin / pandoc
  └── ensure_tool_dir(app, "calibre") → user data dir / calibre / ebook-convert
        ↓  called by
converter/media.rs       ensure_ffmpeg   (Linux + Windows arm)
converter/document/
  binaries.rs            ensure_pandoc   (Linux arm)
                         ensure_ebook_convert (Linux arm)
                         install_marker  (Linux: remove pkexec for pipx install)
```

**User data dirs** (always writable, no elevation):
- Linux: `~/.local/share/swift-shifter/`
- Windows: `%LOCALAPPDATA%\swift-shifter\`

## `tools.toml` Schema

Two extraction modes:

- `mode = "binary"` — extract one binary from the archive (ffmpeg, pandoc)
- `mode = "dir"` — extract the full archive into a named subdirectory (Calibre, which bundles its own shared libs and cannot work as a lone binary)

```toml
[ffmpeg]
version = "7.1"

[ffmpeg.linux-x86_64]
url     = "<BtbN static build .tar.xz>"
sha256  = "<hex>"
archive = "tar.xz"
mode    = "binary"
binary  = "ffmpeg-…/bin/ffmpeg"

[ffmpeg.linux-aarch64]
# same shape, aarch64 URL

[ffmpeg.windows-x86_64]
url     = "<BtbN static build .zip>"
sha256  = "<hex>"
archive = "zip"
mode    = "binary"
binary  = "ffmpeg-…/bin/ffmpeg.exe"

[pandoc]
version = "3.6.4"

[pandoc.linux-x86_64]
url     = "https://github.com/jgm/pandoc/releases/download/3.6.4/pandoc-3.6.4-linux-amd64.tar.gz"
sha256  = "<hex>"
archive = "tar.gz"
mode    = "binary"
binary  = "pandoc-3.6.4/bin/pandoc"

[pandoc.linux-aarch64]
# arm64 variant

[pandoc.windows-x86_64]
url     = "https://github.com/jgm/pandoc/releases/download/3.6.4/pandoc-3.6.4-windows-x86_64.zip"
sha256  = "<hex>"
archive = "zip"
mode    = "binary"
binary  = "pandoc-3.6.4/pandoc.exe"

[calibre]
version  = "7.26.0"   # also used as --version flag for winget on Windows

[calibre.linux-x86_64]
url      = "https://download.calibre-ebook.com/7.26.0/calibre-7.26.0-x86_64.txz"
sha256   = "<hex>"
archive  = "tar.xz"
mode     = "dir"
dest_dir = "calibre"
binary   = "ebook-convert"

[calibre.linux-aarch64]
# aarch64 variant
```

Checksums are fetched from each project's official release checksums file during implementation and hard-coded into `tools.toml`. Version bumps require updating `url`, `sha256`, and `version` — a single-file diff with no Rust changes.

## `downloader.rs` Module

New file: `swift-shifter/src/downloader.rs`

```
pub fn user_tool_dir() -> PathBuf
  Returns ~/.local/share/swift-shifter (Linux) or %LOCALAPPDATA%\swift-shifter (Windows).
  Callers are already cfg-gated to Linux/Windows, so this function is never reachable on macOS.

pub async fn ensure_tool(app, tool_name) -> Result<PathBuf>
  For mode = "binary". Checks user_tool_dir()/bin/<name> exists; downloads + verifies + extracts if not.
  Emits "<tool>:installing" / "<tool>:installed" events (matches existing event names).

pub async fn ensure_tool_dir(app, tool_name, binary_name) -> Result<PathBuf>
  For mode = "dir". Checks user_tool_dir()/<dest_dir>/<binary> exists; downloads + extracts all
  archive entries into dest_dir if not.
```

Internal helpers (private):
- `download_bytes(app, phase, url)` — streams via `reqwest`, emits `install:progress` events
- `verify_sha256(bytes, expected_hex)` — uses `sha2::Sha256`
- `extract_binary(bytes, archive_type, path_in_archive)` — `flate2`+`tar` or `xz2`+`tar` or `zip`
- `extract_dir(bytes, archive_type, dest_dir)` — same decompressors, writes all entries

The manifest string is embedded at compile time:
```rust
const MANIFEST: &str = include_str!("../../tools.toml");
```
Parsed at runtime via the existing `toml` crate into strongly-typed structs mirroring the schema above.

## Changes to Existing Files

### `converter/media.rs` — `ensure_ffmpeg`

Add Linux + Windows arm after the existing macOS block:

```rust
#[cfg(any(target_os = "linux", target_os = "windows"))]
{
    match crate::downloader::ensure_tool(app, "ffmpeg").await {
        Ok(path) => { FFMPEG_PATH.set(Some(path)).ok(); return Ok(()); }
        Err(e)   => { app.emit("ffmpeg:failed", e).ok(); }
    }
}
```

`find_ffmpeg_binary()` gains a check of `downloader::user_tool_dir()/bin/ffmpeg[.exe]` for Linux/Windows so it is found on subsequent launches without re-downloading.

### `converter/document/binaries.rs` — `ensure_pandoc`

Linux arm: remove `pkexec` block, replace with `downloader::ensure_tool(app, "pandoc")`.  
Windows arm: keep `winget` but add `--version <pandoc.version>` parsed from manifest.  
`find_pandoc_binary()` gains a user-dir check for Linux/Windows.

### `converter/document/binaries.rs` — `ensure_ebook_convert`

Linux arm: remove `pkexec` block, replace with `downloader::ensure_tool_dir(app, "calibre", "ebook-convert")`.  
Windows arm: keep `winget` but add `--version <calibre.version> --scope user`.  
`find_ebook_convert_binary()` gains a user-dir check for Linux/Windows.

### `converter/document/binaries.rs` — `install_marker` (Linux)

Remove the `pkexec` call for pipx installation. Replace with:
1. Try `pip install --user pipx` (no elevation)
2. Fall through to `pip install --user marker-pdf` directly if pipx unavailable

### `swift-shifter/Cargo.toml` — new dependencies

```toml
flate2 = "1"
tar    = "0.4"
xz2    = "0.1"
zip    = "2"
sha2   = "0.10"
```

### `swift-shifter/src/main.rs`

Add `mod downloader;` declaration.

## What Does Not Change

- macOS: all Homebrew paths untouched
- Windows Calibre: continues using winget (no clean portable build exists)
- Python tools (marker-pdf, pymupdf4llm): install via `pipx`/`pip --user`, already user-space
- CI workflows: no changes needed; Linux CI already uses `apt-get` at build time for system libs (separate from runtime tools)
- App bundle size: tools are downloaded on first use, not bundled

## Out of Scope

- Automatic version-bump PRs when upstream tools release
- GUI for managing downloaded tools
- Offline installer (tools bundled in app binary)
