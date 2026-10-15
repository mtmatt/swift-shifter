# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, etc.) working in this repository. `CLAUDE.md` is a local symlink to this file.

Swift Shifter is a Tauri v2 menu-bar file converter: a Rust backend doing the conversions, a plain-DOM TypeScript/Vite frontend, and external tools (ffmpeg, pandoc, pymupdf4llm, Calibre, Ollama) for what isn't native yet. typst is linked in for PDF output.

## Commands

```bash
npm install                 # install Tauri CLI, Vite, TypeScript
npm run tauri -- dev        # dev mode: Vite hot-reload + Rust watch (primary dev loop)
npm run tauri -- build      # release bundle -> swift-shifter/target/release/bundle/
npm run build               # frontend only: tsc typecheck + vite build

# Rust (run from swift-shifter/)
cargo test                          # all Rust tests
cargo test test_merge_pdfs          # single test by name substring
cargo clippy --all-targets -- -D warnings  # lint (CI enforces this)
cargo fmt                           # format (CI runs `cargo fmt --check`)
cargo build                         # compile check without bundling
```

### Developer CLI

The same binary doubles as a headless CLI for testing converters (no GUI). Run from `swift-shifter/` with `cargo run -- <subcommand>`:

```bash
cargo run -- doctor                          # report which external tools are found
cargo run -- detect-formats input.pdf        # list valid target formats
cargo run -- convert webp a.png b.jpg        # convert (FORMAT first, then inputs)
cargo run -- trim clip.mp4 00:00:05 00:00:10 # trim media (HH:MM:SS)
cargo run -- merge a.pdf b.pdf               # merge >=2 PDFs
cargo run -- duration clip.mp4               # print duration in seconds
```

`convert`/`trim` build a windowless Tauri app to get a real `AppHandle`; the rest run without one. Global flags override `Config::default()` (the saved GUI `config.toml` is never read): `--output-dir`, `--jpeg-quality`, `--avif-quality`, `--marker`, `--llm`, `--llm-model`, `--llm-url`. Output paths print to stdout, errors to stderr, non-zero exit on failure. Any `argv[1]` that isn't a known subcommand launches the GUI as before (`cli::is_cli_invocation` in `src/cli.rs`). Note: `generate_context!()` is called once in `main()` and passed into `cli::run` — it must not appear twice in the binary.

To check document→PDF output end to end, run `cargo run -- convert pdf in.md`, then `pdffonts in.pdf` to see which fonts were embedded (e.g. that CJK fonts were) and `pdftotext in.pdf -` to check that the text came through.

Bump the version with `./scripts/bump-version.sh 0.5.0` — it syncs the three version files (`package.json`, `swift-shifter/Cargo.toml`, `swift-shifter/tauri.conf.json`), which must always agree. See **Release process** below for how a version bump turns into a published release.

## Architecture

### Backend ↔ frontend boundary

The frontend never imports Rust; it calls `#[tauri::command]` functions over Tauri's IPC. The full command surface is the `invoke_handler!` list in `swift-shifter/src/main.rs` — that list is the contract. Adding a backend capability means writing a command there and `invoke`-ing it from `ui/src/`. The backend pushes progress and status to the UI via `app.emit(...)` events (e.g. `convert:progress`, `update:available`, `ffmpeg:failed`, `ollama:status`); the frontend listens for these.

### The conversion graph (`swift-shifter/src/converter/graph.rs`) — single source of truth

All conversions are edges in one directed graph. `graph::edges()` is the authoritative list (`Edge { from, to, cost, cond }`, where `cond` gates platform/capability — e.g. `MacOnly` for HEIC). Everything else derives from it, so detection and conversion can't drift:

- `detect_output_formats(path)` (in `mod.rs`) returns `graph::direct_targets()` — the direct (1-hop) out-edges, platform-filtered. The UI's `detect_format` command instead returns `graph::detect_with_chains()` → `DetectResult { direct, chained }`, where `chained` are formats reachable only via a multi-hop chain (each with its `route` and `hops`), shown behind the UI's "…" expander.
- `graph::find_path(from, to)` runs Dijkstra over satisfiable edges (weighted by `cost`, capped at `MAX_HOPS = 6`) to pick the min-cost chain.
- `graph::route_hop(from, to) -> Option<Hop>` classifies a single hop to its converter; `convert_file` → `run_single_hop` dispatches on the exhaustive `Hop` enum to `image.rs`, `media.rs` (ffmpeg), `data.rs` (serde), or `document/` (native engine, pandoc, pymupdf, Calibre, marker). The exhaustive match + `test_route_hop_covers_every_satisfiable_edge` structurally guarantee every offered edge is handled.

`convert_file` finds the path, then runs each hop; multi-hop chains stage intermediates in a `tempfile::TempDir` (auto-cleaned) and write only the final artifact to `output_dir`, emitting overall `convert:progress` keyed to the original path.

**To add a format pairing, edit `graph.rs`:** add the `Edge`(s) and a `route_hop` arm (+ a `run_single_hop` branch if it's a new `Hop`). Quirks encoded here: HEIC is macOS-only (`sips`); JPEG/`tif`/`typ` normalize via `normalize_ext` (so no jpg→jpg or pointless tif→tiff; `.typ` maps to the `typst` key, and Typst output is written as `.typ` via `target_to_ext`); PDF routing branches on `use_marker_pdf` and the Ollama LLM config; csv→toml is a semantic dead-end (`is_semantically_blocked`) even though csv→json→toml exists structurally, because a top-level array has no TOML form. **Document→PDF is typeset in-process** with typst, which is linked as a library (`document/native/typst/`). There is no typst binary. The one exception is `.tex` input: when a LaTeX engine is installed (tectonic, xelatex, lualatex or pdflatex; see `document/pdf.rs`), pandoc drives it, since LaTeX keeps raw LaTeX that pandoc's typst writer would drop. Otherwise `.tex`, like epub, goes pandoc → typst source → the embedded typst.
- **md, txt, html → pdf** go through the native typst writer (`native/write/typst.rs`, which also writes `.typ` output) and `native::typeset`. TeX math is converted with `mitex`. Its Typst scope is vendored in `native/typst/mitex/` and served under the virtual `/__shifter__/` path, together with a symbol-rename table for typst 0.15 (`typst/math.rs`). A formula that still fails to compile is shown as TeX code instead of failing the document.
- **typst input** compiles directly (`native::typst_file_to_pdf`); `@preview` packages are downloaded through reqwest into typst's usual cache.
- **Images** become a page the size of the image (`native::image_to_pdf`).
- **epub, and tex without a LaTeX engine,** go pandoc `-t typst` → embedded typst with our preamble (`convert_via_pandoc_typst`).
- **`.tex` with a LaTeX engine installed** still uses pandoc + that engine (`pdf.rs`, which is now LaTeX-only and sets `CJKmainfont` for CJK), because it keeps raw LaTeX.
- **Fonts** are typst's embedded ones plus system fonts, scanned once (`typst::warm_up` at startup). CJK families are picked per detected language (`zh-Hant`/`zh-Hans`/`ja`/`ko`). A document with CJK text but no CJK font fails with an install hint instead of drawing empty boxes.

Pandoc has no `plain` reader, so for the txt→tex hop `convert_document` first turns `.txt` into GFM with `native::plain_text_to_markdown`. That keeps `.txt` literal on every route.

**Native document engine (`document/native/`).** This replaces pandoc one hop at a time; see `docs/superpowers/specs/2026-10-15-native-document-converter-design.md`. Readers (`read/markdown.rs` via comrak, which keeps HTML blocks verbatim as `Block::Raw`; `read/html.rs` via scraper; and `read/plain.rs`) build an `ast::Document`, and writers (`write/html.rs`, `write/markdown.rs`, `write/plain.rs`) render it. `route_hop` sends a pair to `Hop::DocumentNative` whenever `native::handles(from, to)` holds, which today means md, txt and html into each other, plus pdf and typst output. Everything else stays on `DocumentPandoc`.

- **Inputs.** html is an input format only: html → md/txt/pdf/typst directly, and html → tex by chaining through md. Edges into `txt` and `typst` cost 5, so chains pass through md instead. The native `.txt` reader is literal: blank lines separate paragraphs, newlines are line breaks, and there's no Markdown interpretation.
- **pdf→html** keeps pymupdf4llm for extraction and uses `native::markdown_to_html` for the HTML step.
- **Output.** HTML is standalone, with a light/dark stylesheet, `lang` (CJK auto-detected), MathML via pulldown-latex with a TeX annotation, and numbered footnotes. Relative image and link targets become absolute `file://` URLs when the output lands in a different directory.

### External binary management — two strategies by platform

`document/binaries.rs` and `downloader.rs` locate or install tools. macOS resolves system/Homebrew binaries (`/opt/homebrew/bin`, `/usr/local/bin`) and auto-installs via brew. **Linux/Windows download pinned prebuilt binaries** described in `tools.toml` into `dirs::data_local_dir()/swift-shifter`. `tools.toml` is the single source of truth for tool versions/URLs/checksums (`include_str!`'d at compile time) — bump a tool there, no Rust changes needed. Each external tool has an `ensure_*` function spawned at startup in `main.rs setup()`; failures are non-fatal and surfaced to the UI via events.

### Config and shared state

`config.rs` defines `Config` (persisted as TOML in `dirs::config_dir()/swift-shifter/config.toml`) and `AppState`, a Tauri-managed struct holding `Mutex<Config>` and the optional Ollama child process. Values are clamped on both load and save. **Critical async rule** (see comments in `main.rs`): never hold the `std::sync::Mutex` guard across an `.await` — always `.clone()` the config out of the lock first, then await. `set_config` validates/clamps, persists, and emits `config:updated`.

### Windows behavior

Closing the main window hides it rather than quitting (tray app stays resident); real exit goes through the `quit` command / tray, which also kills the spawned Ollama process on `ExitRequested`.

## Frontend

`ui/` is plain TypeScript + Vite, no framework — direct DOM manipulation. Two windows: `index.html`/`main.ts` (drop zone, batch conversion, trim) and `settings.html`/`settings.ts`. Styling uses shared `tokens.css` plus per-window CSS, adaptive light/dark. Tauri capability grants live in `swift-shifter/capabilities/*.json` — a command or plugin API used by a window must be permitted there or the IPC call is rejected.

## Release process

Releases are cut from a long-lived `release` branch, not automatically from every push to `main`. Reconstructed from the commit history and the current `.github/workflows/*.yml` (there is no separate release doc):

1. Features land on `main` via small, single-purpose PRs (squash-merged).
2. Periodically, `main` is merged into `release` (`Merge branch 'main' into release`).
3. On `release`, run `./scripts/bump-version.sh X.Y.Z` and commit the result as `chore: bump version to X.Y.Z`. The script syncs `package.json`, `swift-shifter/Cargo.toml`, and `swift-shifter/tauri.conf.json` — these three **must agree**, since the release build matrix reads them.
4. Push the bump commit, then tag it by hand: `git tag vX.Y.Z && git push <remote> vX.Y.Z` (check `git remote -v`; the remote may be named `swift-shifter`, not `origin`). Nothing creates tags automatically: the old `tag.yml` workflow was deleted in `chore: remove unused workflow` (commit `aa66082`), and recent tags were placed manually by the maintainer on each `chore: bump version` commit (`v1.0.0` is lightweight; earlier tags are annotated). `release.yml` starts with a `verify-version` job that runs `scripts/check-version.sh <tag>` and fails the release if the tag and the three files disagree; run that script yourself before tagging to catch it earlier. If an older checkout still has the annotated `v0.2.1` tag (the remote's is lightweight), `git fetch --tags` fails with "would clobber existing tag"; use plain `git fetch`.
5. The `v*` tag push triggers `.github/workflows/release.yml`, which builds signed bundles across a matrix — macOS (dmg + universal updater artifact), Windows (msi/nsis, Authenticode-signed if `WINDOWS_CERTIFICATE`/`WINDOWS_CERTIFICATE_PASSWORD` are configured), Ubuntu (deb + AppImage), and Fedora (rpm only, built inside a `fedora:latest` container since AppImage needs FUSE, which containers don't have) — via `tauri-apps/tauri-action`. macOS builds are signed/notarized when `APPLE_*` secrets are present. Each target with a `rust-target` also gets a raw `swift-shifter` binary archive uploaded for `cargo binstall`. Everything is published as a **draft** GitHub release (`releaseDraft: true`) — a maintainer reviews and publishes it manually afterward.
6. `release` is then merged back into `main` (`Merge branch 'release'`) so `main` picks up the version bump.

Every push to `main` or `release`, and every pull request, also runs `.github/workflows/build.yml`: a compile-only matrix (macOS, Windows, Ubuntu, Fedora, Arch) running `tsc --noEmit`, `npm run build`, `cargo clippy --all-targets -- -D warnings` (per OS, since cfg-gated code differs), and `tauri build --no-bundle`, plus `cargo fmt --check` and `scripts/check-version.sh` on Ubuntu — a build-health gate producing no artifacts, distinct from the signed release build.

## Rules

Don't add claude as a coauthor.
