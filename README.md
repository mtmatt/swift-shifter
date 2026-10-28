# Swift Shifter

**Convert files on your own machine. Nothing is uploaded.** Swift Shifter is a menu-bar converter for images, audio, video and documents, and it aims to be the best at two things: **LaTeX → Typst**, and **PDF → Markdown, LaTeX or Typst**.

<div align="center">
<img src="docs/assets/example.png" alt="Swift Shifter's drop zone, with two HEIC photos converted to PNG">
</div>

Press Ctrl + Shift + Space anywhere and a search bar suggests the files you changed lately. Type a few letters, pick a format, and press Enter. You can also drop files on the main window.

## When it goes online

Your files never leave your computer. The app goes online only for these two things:

- **Checking for updates:** at launch, it asks GitHub whether there is a newer version. Updates are signature-checked before they install.
- **Typst packages:** when a Typst document has `#import "@preview/…"`, the package is downloaded from the official Typst package registry. Only the package's name is sent, not your document.

The [tools you install yourself](#tools-you-install-yourself) run as you have them, and a LaTeX engine may go online on its own: `.tex → PDF` uses one where it's installed, and Tectonic downloads the TeX files it's missing.

## Download and install

Get the latest version from [Releases](https://github.com/mtmatt/swift-shifter/releases):

| Platform | File                                     |
| -------- | ---------------------------------------- |
| macOS    | `.dmg` (Apple silicon)                   |
| Windows  | `.msi`                                   |
| Linux    | `.deb` (Debian, Ubuntu), `.rpm` (Fedora) |

Each release also has the bare `swift-shifter` binary for each platform, as a `.tar.gz` or `.zip`.

If your system warns about the app the first time you open it:

- **macOS:** open System Settings → Privacy & Security, and click **Open Anyway** next to Swift Shifter.
- **Windows:** in the SmartScreen dialog, click **More info**, then **Run anyway**.

### Tools you install yourself

Some conversions still run another program, which isn't built in yet. Since v2.0.0 the app doesn't install them for you. When one is missing, the conversion that needs it tells you how to install it, and `swift-shifter doctor` lists which ones it finds.

| Tool                       | Needed for                                                                                                  | Install                                                                                                                                      |
| -------------------------- | ----------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| [pandoc][pandoc]           | LaTeX (`.tex`) documents                                                                                    | macOS: `brew install pandoc` · Windows: `winget install JohnMacFarlane.Pandoc` · Linux: your package manager, e.g. `sudo apt install pandoc` |
| [ffmpeg][ffmpeg]           | audio, video, and images to GIF                                                                             | macOS: `brew install ffmpeg` · Windows: `winget install Gyan.FFmpeg` · Linux: your package manager, e.g. `sudo apt install ffmpeg`           |
| [pymupdf4llm][pymupdf4llm] | reading PDFs (the default engine)                                                                           | `pipx install pymupdf4llm --include-deps` (with [pipx](https://pipx.pypa.io))                                                                |
| [marker-pdf][marker]       | optional: reading PDFs to Markdown or EPUB, and on to LaTeX or Typst, where marker is turned on in Settings | `pipx install marker-pdf`, then `pipx inject marker-pdf psutil`                                                                              |

PDF output is built in: Typst is part of the app, so a PDF needs no LaTeX install. For `.tex → PDF`, a LaTeX engine you have installed (Tectonic, XeLaTeX, LuaLaTeX or pdfLaTeX) is used, since it keeps raw LaTeX; without one, the built-in Typst typesets it.

[pandoc]: https://pandoc.org/installing.html
[ffmpeg]: https://ffmpeg.org/download.html
[pymupdf4llm]: https://pypi.org/project/pymupdf4llm/
[marker]: https://github.com/datalab-to/marker

## Formats

- **Documents:** Markdown, plain text, HTML, LaTeX, Typst, PDF and EPUB, to one another. LaTeX input needs pandoc, and PDF input needs pymupdf4llm, or marker where it's turned on in Settings. PDF output is typeset by the built-in Typst.
- **Images:** PNG, JPEG, WebP, GIF, BMP and TIFF, to one another and to AVIF and PDF, and HEIC/HEIF too on macOS. AVIF is written but not read yet. Converting to GIF needs ffmpeg.
- **Video:** MP4, MOV, MKV, WebM, AVI and GIF, with ffmpeg.
- **Audio:** MP3, AAC, FLAC, OGG, WAV, OPUS and M4A, with ffmpeg.
- **Data:** JSON, YAML, TOML and CSV.

When there's no direct route between two formats, the app chains conversions and keeps only the final file.

## How it differs

- **From online converters:** your files stay on your computer, and there is no upload size limit or queue.
- **From pandoc:** most conversions are Swift Shifter's own, written for the formats it aims to do best. Its PDF output is typeset with Typst, without a LaTeX install. It also converts images, audio, video and data, from a menu bar and a hotkey rather than a command line. For LaTeX input it still uses pandoc, until its own LaTeX reader is done ([#73](https://github.com/mtmatt/swift-shifter/issues/73)).

## Features

- **Quick bar:** Ctrl + Shift + Space, or a click on the tray icon, opens a search bar with the files you recently changed in Desktop, Documents and Downloads. Type to filter, Tab to pick a format (or type "photo to jpg"), and Enter to convert. The shortcut and the folders can be changed in Settings → Quick.
- **Drop zone:** the main window, opened from the Dock, the Start menu or the tray menu, takes files dragged from Finder or any file manager, several at once.
- **PDF merge:** combine several PDFs into one.
- **Clipboard:** paste (⌘V) to convert what's on the clipboard. The result goes back to the clipboard or to disk, as you choose.
- **Output next to the source:** a converted file lands in the input's folder, unless you set another in Settings. Click the result to show it in your file manager.
- **Settings:** the output folder, JPEG and AVIF quality, how many files convert at once, and the PDF engine.

## License

Copyright 2026 ShangJhe Li (mtmatt)

From v2.0.0, Swift Shifter is licensed under the GNU General Public License
v3.0 or later ([`LICENSE`](LICENSE), SPDX `GPL-3.0-or-later`).

v1.0.1 and earlier were released under the Apache License 2.0, and stay
under it. Code that came in under Apache-2.0 keeps its notices, and
third-party code keeps its own license, such as the vendored mitex Typst
scope (`swift-shifter/src/converter/document/native/typst/mitex/LICENSE`).

## Building from source

| Tool    | Version         | Notes                  |
| ------- | --------------- | ---------------------- |
| Rust    | stable (≥ 1.92) | via `rustup`           |
| Node.js | ≥ 24            | for Tauri CLI and Vite |

To run the conversions that need them, also install the [tools above](#tools-you-install-yourself).

```bash
# Clone
git clone https://github.com/mtmatt/swift-shifter.git
cd swift-shifter

# Install dependencies (Tauri CLI + Vite + TypeScript)
npm install

# Run in dev mode (Vite hot-reload + Rust watch)
npm run tauri -- dev

# Build release binary
npm run tauri -- build
```

The release `.app` lands in `swift-shifter/target/release/bundle/`.

### Tech stack

| Layer                  | Technology                                                                                                                   |
| ---------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| App shell              | [Tauri v2](https://tauri.app)                                                                                                |
| Backend logic          | Rust                                                                                                                         |
| Image processing       | [`image`](https://crates.io/crates/image) crate + [`ravif`](https://crates.io/crates/ravif) for AVIF + macOS `sips` for HEIC |
| Video / audio          | System `ffmpeg` (install it yourself; the app says how)                                                                      |
| Document conversion    | Native Rust engine for Markdown / plain text / HTML / EPUB / Typst; `pandoc` for LaTeX input (install it yourself)           |
| PDF output             | [typst](https://typst.app) compiler linked in (no install, CJK font selection, TeX math via `mitex`)                         |
| PDF ↔ EPUB / HTML / MD | `pymupdf4llm` (install it with pipx); optional ML engine via `marker-pdf`                                                    |
| Data serialization     | `serde_json`, `serde_yaml`, `toml`, `csv` crates                                                                             |
| Frontend UI            | TypeScript + [Vite](https://vitejs.dev) (plain DOM, no framework)                                                            |
| Global hotkeys         | `tauri-plugin-global-shortcut`                                                                                               |
| System tray            | Tauri built-in tray API                                                                                                      |
| Update check           | `tauri-plugin-updater`                                                                                                       |

### Architecture overview

```
swift-shifter/
├── swift-shifter/           # Rust/Tauri workspace member
│   ├── src/
│   │   ├── main.rs          # Tauri app entry point, window + tray + menu setup
│   │   ├── config.rs        # Persisted settings (quality, concurrency)
│   │   ├── clipboard.rs     # Clipboard read/write (paste-to-convert, copy result back)
│   │   ├── hotkey.rs        # Global shortcut registration
│   │   ├── tray.rs          # System tray icon and menu
│   │   └── converter/
│   │       ├── mod.rs       # Dispatcher: routes files to the right converter
│   │       ├── image.rs     # image crate + ravif (AVIF) + sips (HEIC)
│   │       ├── media.rs     # ffmpeg subprocess wrapper
│   │       ├── data.rs      # JSON/YAML/TOML/CSV converters
│   │       └── document/    # native engine + embedded typst, pandoc, pymupdf4llm/marker-pdf
│   │           ├── mod.rs
│   │           ├── binaries.rs  # finding the external tools
│   │           ├── conversion.rs # conversion logic per format
│   │           ├── merge.rs     # PDF merge (combine multiple PDFs into one)
│   │           ├── types.rs
│   │           └── utils.rs
│   ├── capabilities/
│   │   └── default.json     # Tauri v2 permission grants
│   ├── Cargo.toml
│   └── tauri.conf.json
├── ui/                      # Frontend (loaded by Tauri webview)
│   ├── index.html           # Main drop-zone window
│   ├── settings.html        # Preferences window
│   └── src/
│       ├── main.ts          # Drag-drop, file picker, conversion UI logic
│       ├── settings.ts      # Settings panel logic
│       ├── style.css        # Main window styles (adaptive light/dark)
│       ├── settings.css     # Settings panel styles
│       └── tokens.css       # Shared design tokens
├── .github/
│   └── workflows/
│       ├── build.yml        # Compile check on macOS, Windows, Ubuntu, Fedora, Arch
│       └── release.yml      # On a v* tag: builds signed installers as a draft release
├── scripts/
│   ├── bump-version.sh      # Syncs version across package.json, Cargo.toml, tauri.conf.json
│   └── build-icons.sh
├── vite.config.ts
├── tsconfig.json
└── package.json
```

### Releasing a version

Releases are cut from the `release` branch, and tags are pushed by hand:

```bash
git switch release && git merge main
./scripts/bump-version.sh 0.2.0
git commit -am "chore: bump version to 0.2.0"
./scripts/check-version.sh v0.2.0
git push origin release
git tag v0.2.0 && git push origin v0.2.0
```

`bump-version.sh` syncs `package.json`, `Cargo.toml`, and `tauri.conf.json`, which must agree. Pushing the `v0.2.0` tag starts `release.yml`: it checks the tag against those three files, then builds the signed installers into a draft GitHub release, which a maintainer reviews and publishes. Afterwards, merge `release` back into `main`. Nothing creates tags automatically.

### CI / release pipeline

| Workflow      | Trigger                                   | What it does                                                                            |
| ------------- | ----------------------------------------- | --------------------------------------------------------------------------------------- |
| `build.yml`   | Push to `main` or `release`, PRs          | Compile check on macOS, Windows, Ubuntu, Fedora, Arch; runs the full test suite         |
| `release.yml` | `v*` tag pushed, or run by hand for a tag | Verifies the tag matches the version files, builds signed installers as a draft release |

## Roadmap

- [x] Core Tauri shell + system tray
- [x] Drop zone window with drag-and-drop
- [x] Global hotkey (Ctrl+Shift+Space) → quick bar with recent-file search
- [x] Image conversion (PNG/WebP/JPEG/AVIF/HEIC)
- [x] Data conversion (JSON/YAML/TOML/CSV)
- [x] Video/audio via ffmpeg subprocess
- [x] Progress events streamed to UI
- [x] Output folder reveal
- [x] Batch conversion with concurrency
- [x] Config file + settings panel
- [x] Update check via Tauri updater plugin
- [x] Windows and Linux support
- [x] CI/CD pipeline with signed releases
- [x] Document conversion (native engine, embedded typst for PDF; pandoc for LaTeX input)
- [x] Images to PDF
- [x] EPUB <-> PDF
- [x] PDF/EPUB -> HTML and Markdown
- [x] PDF merge (combine multiple PDFs into one)
- [x] M4A audio format support
- [x] Clipboard integration (paste to convert; copy result back to clipboard)

### Coming next

- [ ] A native LaTeX reader, so `.tex` input no longer goes through pandoc ([#73](https://github.com/mtmatt/swift-shifter/issues/73))
- [ ] Removing pandoc ([#74](https://github.com/mtmatt/swift-shifter/issues/74))
- [ ] Linking ffmpeg's libraries rather than running its binary ([#81](https://github.com/mtmatt/swift-shifter/issues/81))
