# Swift Shifter

A featherweight, always-on file converter that lives in your menu bar. Drop any file onto the floating window — or select files and hit a hotkey — and get instant format conversion without opening a browser or bulky software.

**Target binary size: < 3 MB. Target launch time: < 100 ms.**


## Features

- **Floating drop zone** — a small, always-on-top window accepts drag-and-drop from Finder or any file manager
- **Global hotkey** — cmd + shift + Space toggles the window; conversion options appear immediately
- **Zero-friction UX** — one click to pick the output format, conversion starts instantly
- **Broad format support**
  - Images: WebP, PNG, JPEG, AVIF, GIF, BMP, TIFF, HEIC/HEIF
  - Video: MP4, MOV, MKV, WebM, AVI, GIF (video-to-GIF and GIF-to-video)
  - Audio: MP3, AAC, FLAC, OGG, WAV, OPUS
  - Data: JSON, YAML, TOML, CSV
- **Output next to source** — converted file lands in the same folder as the input
- **Batch conversion** — drop multiple files at once
- **Progress indicator** — lightweight inline progress bar per file, no modal dialogs
- **Click to reveal** — success label opens the output folder in Finder


## Tech Stack

| Layer | Technology |
|---|---|
| App shell | [Tauri v2](https://tauri.app) |
| Backend logic | Rust |
| Image processing | [`image`](https://crates.io/crates/image) crate + [`ravif`](https://crates.io/crates/ravif) for AVIF + macOS `sips` for HEIC |
| Video / audio | System `ffmpeg` (auto-installed via Homebrew if missing) |
| Data serialization | `serde_json`, `serde_yaml`, `toml`, `csv` crates |
| Frontend UI | TypeScript + [Vite](https://vitejs.dev) |
| Global hotkeys | `tauri-plugin-global-shortcut` |
| System tray | Tauri built-in tray API |


## Architecture Overview

```
swift-shifter/
├── swift-shifter/           # Rust/Tauri workspace member
│   ├── src/
│   │   ├── main.rs          # Tauri app entry point, window + tray setup
│   │   ├── converter/
│   │   │   ├── mod.rs       # Dispatcher: routes files to the right converter
│   │   │   ├── image.rs     # image crate + ravif (AVIF) + sips (HEIC)
│   │   │   ├── media.rs     # ffmpeg subprocess wrapper
│   │   │   └── data.rs      # JSON/YAML/TOML/CSV converters
│   │   ├── hotkey.rs        # Global shortcut registration
│   │   └── tray.rs          # System tray icon and menu
│   ├── capabilities/
│   │   └── default.json     # Tauri v2 permission grants
│   ├── Cargo.toml
│   └── tauri.conf.json
├── ui/                      # Frontend (loaded by Tauri webview)
│   ├── index.html
│   └── src/
│       ├── main.ts          # Drag-drop, file picker, conversion UI logic
│       └── style.css        # Adaptive light/dark theme
├── .github/
│   └── workflows/
│       └── build.yml        # macOS CI (compile check, no signing required)
├── vite.config.ts
├── tsconfig.json
└── package.json
```

The Rust backend exposes a small set of Tauri commands:
- `detect_format(path)` — returns supported output formats for the given file
- `convert(path, target_format)` — run conversion, stream progress events via `convert:progress`
- `open_output_folder(path)` — reveal result in Finder
- `quit()` — exit the app


## Prerequisites

| Tool | Version | Notes |
|---|---|---|
| Rust | stable (≥ 1.78) | via `rustup` |
| Node.js | ≥ 24 | for Tauri CLI and Vite |
| ffmpeg | ≥ 6 | for video/audio; auto-installed via `brew` if missing |

The Tauri CLI is installed as a local npm dev dependency — no global install needed.


## Getting Started

```bash
# Clone
git clone https://github.com/yourname/swift-shifter.git
cd swift-shifter

# Install dependencies (Tauri CLI + Vite + TypeScript)
npm install

# Run in dev mode (Vite hot-reload + Rust watch)
npm run tauri -- dev

# Build release binary
npm run tauri -- build
```

The release `.app` lands in `swift-shifter/target/release/bundle/`.


## Size Budget

The <3 MB goal applies to the app shell excluding `ffmpeg`. Tactics:

- Tauri webview uses the OS-native renderer (no Chromium embedded)
- No JS framework — TypeScript compiled to vanilla JS by Vite
- `image` crate compiled with only the feature flags needed
- `strip = true` + `opt-level = "z"` + `lto = true` in release profile
- `ffmpeg` is a runtime dependency, not bundled


## Roadmap

- [x] Core Tauri shell + system tray
- [x] Drop zone window with drag-and-drop
- [x] Global hotkey (⌘⇧Space) → file picker fallback
- [x] Image conversion (PNG/WebP/JPEG/AVIF/HEIC)
- [x] Data conversion (JSON/YAML/TOML/CSV)
- [x] Video/audio via ffmpeg subprocess
- [x] Progress events streamed to UI
- [x] Output folder reveal
- [x] macOS CI
- [x] Batch conversion with concurrency
- [x] Config file + settings panel
- [ ] Handle ffmpeg and brew installation automatically
- [ ] Auto-update via Tauri updater plugin
- [ ] Windows and Linux support
- [ ] Windows and Linux builds in CI


## License

Apache License Version 2.0
