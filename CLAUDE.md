# CLAUDE.md — Swift Shifter

This file tells Claude Code how to work on this project.

---

## Project in one sentence

Swift Shifter is a Tauri v2 desktop app (Rust backend, TypeScript/Vite frontend) that converts images, video, audio, data, and document files via a floating drop window and a global hotkey. Binary target: <3 MB, launch target: <100 ms.

---

## Repo layout

```
swift-shifter/           ← git root
├── swift-shifter/       ← Rust/Tauri workspace member
│   ├── Cargo.toml
│   ├── build.rs         ← Tauri build script (do not edit)
│   ├── tauri.conf.json
│   ├── capabilities/    ← Tauri v2 permission manifests
│   │   ├── default.json
│   │   └── settings.json
│   └── src/
│       ├── main.rs
│       ├── config.rs
│       ├── hotkey.rs
│       ├── tray.rs
│       └── converter/
│           ├── mod.rs
│           ├── image.rs    ← image crate + ravif + macOS sips (PNG/JPEG/WebP/AVIF/HEIC…)
│           ├── media.rs    ← ffmpeg subprocess (video/audio)
│           ├── data.rs     ← serde (JSON/YAML/TOML/CSV)
│           ├── document.rs ← pandoc + pymupdf4llm + marker-pdf (md/txt/pdf/epub/tex/typst)
│           └── tests.rs    ← unit tests
├── ui/                  ← Tauri frontend (TypeScript + Vite, no framework)
│   ├── index.html
│   ├── settings.html
│   └── src/
│       ├── main.ts
│       ├── settings.ts
│       ├── style.css
│       ├── settings.css
│       └── tokens.css
├── scripts/
│   ├── bump-version.sh  ← syncs version across all three manifest files
│   └── build-icons.sh
├── .github/workflows/
│   ├── build.yml        ← compile check on macOS/Windows/Ubuntu/Fedora/Arch
│   ├── tag.yml          ← auto-creates v* tag after build passes
│   └── release.yml      ← builds + signs + publishes on v* tag
├── docs/
│   └── assets/
├── README.md
└── CLAUDE.md
```

---

## Build & run commands

```bash
# Install JS dependencies (first time)
npm install

# Dev mode (Rust hot-reload + webview)
npm run tauri -- dev

# Release build
npm run tauri -- build

# Run Rust unit tests only (no Tauri runtime needed)
cargo test --manifest-path swift-shifter/Cargo.toml

# Check for compile errors without building
cargo check --manifest-path swift-shifter/Cargo.toml

# Bump version across all three manifest files, then commit and push
./scripts/bump-version.sh 0.2.0
```

---

## Key constraints — respect these at all times

### Size budget
- App shell (excluding ffmpeg) must stay under 3 MB.
- Use `opt-level = "z"`, `lto = true`, `strip = true` in the release profile.
- Do NOT add JS frameworks (React, Vue, Svelte, etc.). Plain DOM only.
- Do NOT add crate dependencies without checking whether the feature can be implemented with an existing dep or stdlib.

### Performance
- The drop window must appear in <100 ms from hotkey press.
- Conversion runs on a Tauri async command (non-blocking). Never block the main thread.
- Batch conversion uses `tokio::sync::Semaphore` to cap concurrency at `config.max_concurrent` (default 4, clamped 1–8).

### ffmpeg is a runtime dep, not compile-time
- Video and audio conversion shells out to the system `ffmpeg` binary.
- Detect its path at startup; if not found, auto-install via `brew` (macOS) and notify the user.
- Never link ffmpeg statically unless the user explicitly asks for a self-contained bundle.

### pandoc is a runtime dep, not compile-time
- Document conversion (md/txt/pdf/tex/typst) shells out to the system `pandoc` binary.
- Detect its path at startup; if not found, auto-install:
  - macOS: `brew install pandoc`
  - Windows: `winget install pandoc`
  - Linux: `apt`/`dnf`/`pacman` (detected automatically) with `pkexec` for privilege elevation
- PDF output requires a PDF engine (`tectonic`, `xelatex`, `pdflatex`, `lualatex`, or `wkhtmltopdf`); the first found on PATH is used automatically.

### pymupdf4llm is a runtime dep, not compile-time
- Required for PDF → EPUB, PDF → HTML, and PDF → MD conversion paths.
- Python library; auto-installed via pipx (`pipx install pymupdf4llm`) on first use.
- Detection: probes pipx venv python first, then `python3`/`python` candidates, for importability (`python -c "import pymupdf4llm"`).
- Emits events during detection/install: `pymupdf:missing`, `pymupdf:installing`, `pymupdf:installed`, `pymupdf:failed`.
- If install fails, PDF→EPUB/HTML/MD formats are silently disabled.

### ebook-convert (Calibre) is a runtime dep, not compile-time
- Required for all MOBI conversions (MOBI → PDF/EPUB/HTML/MD, EPUB → MOBI, PDF → MOBI).
- Auto-install: macOS (`brew install --cask calibre`), Windows (`winget install calibre.calibre`), Linux (apt/dnf/pacman install calibre).
- Emits events during detection/install: `ebook-convert:missing`, `ebook-convert:installing`, `ebook-convert:installed`.
- If install fails, MOBI-related formats are silently disabled.
- Binary locations: macOS `/Applications/calibre.app/Contents/MacOS/ebook-convert`, Windows `C:\Program Files\Calibre2\ebook-convert.exe`.

### marker-pdf is an optional runtime dep
- ML-based PDF → EPUB converter; higher quality than pymupdf4llm but requires Python + pipx.
- Only activated when the user enables `use_marker_pdf` in settings.
- Installation is fully automated and streamed to the UI via `marker:step` events.
- Falls back to pymupdf4llm if marker is unavailable or disabled.

### Image conversion uses the `image` crate
- No ffmpeg for images. The `image` crate handles PNG, JPEG, WebP, BMP, TIFF, GIF.
- For AVIF, use the `ravif` + `rgb` crates.
- For HEIC/HEIF on macOS, use the native `sips` CLI tool.
- Enable only the image format feature flags that are needed.

### Data conversion is pure Rust
- JSON ↔ YAML ↔ TOML ↔ CSV uses `serde_json`, `serde_yaml`, `toml`, `csv`.
- No subprocess, no ffmpeg.

### Auto-update
- Uses `tauri-plugin-updater`. On startup, checks the GitHub releases endpoint in the background.
- Emits `update:available` with `{version, body}` if a newer version exists.
- Emits `update:progress` (percentage `f32`) during download.
- `check_update` and `install_update` Tauri commands are available for the frontend to trigger manually.
- The updater public key lives in `tauri.conf.json → plugins.updater.pubkey`. Generate a keypair with `npm run tauri signer generate -- -w ~/.tauri/swift-shifter.key` and store the private key as `TAURI_SIGNING_PRIVATE_KEY` in GitHub secrets.

---

## Tauri command conventions

All backend entry points are `#[tauri::command]` async functions in `src/main.rs` or re-exported from submodules. Follow this pattern:

```rust
#[tauri::command]
async fn convert(path: String, target_format: String) -> Result<String, String> {
    // return Ok(output_path) or Err(human-readable message)
}
```

- Return `Result<T, String>` — the `String` error is shown directly in the UI.
- Emit Tauri events for progress: `app_handle.emit("convert:progress", payload)`.
- Never `unwrap()` or `expect()` in command handlers — propagate errors.

### Registered commands (invoke_handler)

| Command | Description |
|---|---|
| `detect_format` | Return available output formats for a given input file |
| `convert` | Convert a single file; returns output path |
| `convert_batch` | Convert multiple files concurrently (semaphore-limited) |
| `get_config` | Fetch current `Config` as JSON |
| `set_config` | Persist updated `Config` |
| `check_marker` | Returns `bool` — whether marker-pdf is installed |
| `install_marker` | Async: install marker-pdf, streaming `marker:step` events |
| `check_ebook_convert` | Returns `bool` — whether ebook-convert (Calibre) is installed |
| `open_output_folder` | Open the output directory in Finder/Explorer |
| `check_update` | Manually trigger update check |
| `install_update` | Download and apply update |
| `quit` | Exit the app |

---

## Config schema

Config is persisted as TOML at `~/.config/swift-shifter/config.toml` (via the `dirs` crate).

```rust
pub struct Config {
    pub output_dir: Option<String>,  // None = same folder as source
    pub jpeg_quality: u8,            // 1–100, default 75
    pub avif_quality: u8,            // 1–100, default 65
    pub max_concurrent: usize,       // 1–8, default 4
    pub use_marker_pdf: bool,        // default false
}
```

Values are clamped to valid ranges on load. Access via `get_config` / `set_config` commands — never read the file from the frontend.

---

## Event channels (Rust → frontend)

| Event | Payload | When |
|---|---|---|
| `update:available` | `{version, body}` | Newer release found on startup |
| `update:progress` | `f32` (0–100) | Download progress during update |
| `ffmpeg:failed` | error string | ffmpeg auto-install failed |
| `pandoc:missing` | — | pandoc not on PATH, attempting install |
| `pandoc:installing` | — | pandoc installation running |
| `pandoc:installed` | — | pandoc successfully installed |
| `pandoc:failed` | error string | pandoc install failed |
| `pymupdf:missing` | — | pymupdf4llm not importable, attempting install |
| `pymupdf:installing` | — | pip install running |
| `pymupdf:installed` | — | pymupdf4llm successfully installed |
| `pymupdf:failed` | error string | pymupdf4llm install failed |
| `ebook-convert:missing` | — | ebook-convert (Calibre) not on PATH |
| `ebook-convert:installing` | — | Calibre installation running |
| `ebook-convert:installed` | — | Calibre successfully installed |
| `ebook-convert:failed` | error string | Calibre install failed |
| `marker:step` | step message string | marker-pdf installation progress |
| `install:log` | `{line, phase}` | Streaming log from ffmpeg/dep install |
| `convert:progress` | progress payload | Per-file conversion progress |

---

## Frontend conventions

- The UI is in `ui/src/`. TypeScript is compiled by Vite; output goes to `ui/dist/`.
- No JS framework — plain DOM only. Write `.ts` files, not `.js` files.
- Two entry points: `ui/index.html` (main drop window) and `ui/settings.html` (preferences).
- Communicate with Rust via `window.__TAURI__.core.invoke(command, args)`.
- Listen for events with `window.__TAURI__.event.listen("event:name", handler)`.
- Keep the drop zone window small: 340×460 px, always-on-top, no title bar chrome.
- Settings window: 480×540 px; reuses an existing window if already open (no duplicates).
- `ui/src/main.js` is a compiled artifact — do not edit it.

---

## Error handling philosophy

- Conversion errors are shown inline next to the file name, never as modal dialogs.
- A missing `ffmpeg` silently disables video/audio options — it does not block the app.
- A missing `pandoc` silently disables document options — same pattern.
- Invalid or unsupported files show a brief inline message; they do not crash or log to stderr in release builds.

---

## Testing approach

- Unit tests live in `swift-shifter/src/converter/tests.rs`.
- Unit-test each converter module independently (`converter::image`, `converter::data`).
- Integration tests shell out to `cargo tauri build` in CI to verify the binary compiles.
- Do not write tests that require a display or a running Tauri runtime.

---

## CI / release pipeline

| Workflow | Trigger | What it does |
|---|---|---|
| `build.yml` | push to `main`, PRs | Compile check on macOS, Windows, Ubuntu, Fedora, Arch |
| `tag.yml` | `build.yml` passes on `main` | Verifies all version files agree, creates `v{version}` tag via PAT |
| `release.yml` | `v*` tag pushed | Builds signed installers on all platforms, publishes GitHub draft release |

### Version bumping
Always use `./scripts/bump-version.sh <version>` — it updates `package.json`, `swift-shifter/Cargo.toml`, and `swift-shifter/tauri.conf.json` in one step. Pushing to `main` after that triggers the full pipeline automatically.

### Required GitHub secrets
- `TAG_TOKEN` — PAT with `contents: write` (so auto-tag triggers release workflow)
- `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — updater signing
- `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` — macOS code signing
- `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` — macOS notarization
- `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` — Windows Authenticode signing

---

## Design Suggestions

- 保留最常用的 3-4 個格式按鈕，其餘的收納進一個 ... 更多選項中。
- 「監控資料夾」或「預設工作流」需要配置空間。可以整合到設定空間裡面。
- 將使用者的自定義預設集 (Presets) 直接顯示在檔案下方的按鈕列中，但讓使用者決定他要的視覺效果。用以區分原生功能。
- AI 功能（如去背、超解析度）不一定要放在「轉換」的前面，可以放在「轉換」的結果之後。轉換成功後，原本的按鈕位置可以出現微小的 AI 增強選項（例如：HI-RES）。
- 與其給使用者一個包含 100 種語言的選單，不如利用 Rust 後端獲取系統資訊來縮小範圍：系統語系優先 (System Locale)：透過 Rust 的 sys-locale crate 獲取使用者當前的作業系統語言。如果系統是繁體中文，那麼「轉為繁體中文」應該是預設的第一個選項。地理位置推測 (Geographical Context)：根據使用者的 IP 或時區進行初步推測。例如在歐洲，德語、法語、西班牙語的權重會提高；在亞洲，日、韓、中、英文則是核心。語言偵測 (Auto-detection)：當檔案丟入時，後端先利用輕量級的 whatlang crate 快速偵測原始檔案語言。如果偵測到是日文，UI 就不應該顯示「轉為日文」的按鈕，而是顯示「轉為繁體中文」或「轉為英文」。
- 不要顯示所有語言，只顯示 2 個最可能的建議按鈕（例如：ZH-TW、EN）。建議按鈕尾端放一個微小的 + 或 ...。點擊後，可以出現一個搜尋的視窗。
- LRU 讓較常使用的功能放在前台。
- For Offline LLM Support, the user should explicitly select this option in the setting to download the model.

---

## What NOT to do

- Do not add Chromium / Electron. This is Tauri; the webview is OS-native.
- Do not add JS frameworks (React, Vue, Svelte, etc.). Plain DOM + TypeScript only.
- Do not write `.js` files in `ui/src/` — TypeScript only; `main.js` is a compiled artifact.
- Do not store state in the frontend — source of truth is always Rust.
- Do not add telemetry, analytics, or network calls of any kind (except the updater check).
- Do not use `async-std`; the project uses `tokio` (Tauri's default async runtime).
- Do not bump versions manually in individual files — always use `scripts/bump-version.sh`.
- Do not use `rayon` for batch conversion — use `tokio::sync::Semaphore` with async tasks.
