# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, etc.) working in this repository. `CLAUDE.md` is a local symlink to this file.

Swift Shifter is a Tauri v2 menu-bar file converter: a Rust backend doing the conversions, a plain-DOM TypeScript/Vite frontend, and a heavy reliance on external tools (ffmpeg, pandoc, pymupdf4llm, Calibre, Ollama).

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
- `graph::route_hop(from, to) -> Option<Hop>` classifies a single hop to its converter; `convert_file` → `run_single_hop` dispatches on the exhaustive `Hop` enum to `image.rs`, `media.rs` (ffmpeg), `data.rs` (serde), or `document/` (pandoc/pymupdf/Calibre/marker). The exhaustive match + `test_route_hop_covers_every_satisfiable_edge` structurally guarantee every offered edge is handled.

`convert_file` finds the path, then runs each hop; multi-hop chains stage intermediates in a `tempfile::TempDir` (auto-cleaned) and write only the final artifact to `output_dir`, emitting overall `convert:progress` keyed to the original path.

**To add a format pairing, edit `graph.rs`:** add the `Edge`(s) and a `route_hop` arm (+ a `run_single_hop` branch if it's a new `Hop`). Quirks encoded here: HEIC is macOS-only (`sips`); JPEG/`tif`/`typ` normalize via `normalize_ext` (so no jpg→jpg or pointless tif→tiff; `.typ` maps to the `typst` key, and Typst output is written as `.typ` via `target_to_ext`); PDF routing branches on `use_marker_pdf` and the Ollama LLM config; csv→toml is a semantic dead-end (`is_semantically_blocked`) even though csv→json→toml exists structurally, because a top-level array has no TOML form. **Document→PDF** uses a native tool when one fits: Typst (`.typ`/`.typst`) input compiles via `typst compile` (see `convert_typst_to_pdf`), and for md/txt/tex/image input pandoc is invoked with `--pdf-engine` via `document/pdf.rs` (`configure_pandoc_pdf`): it prefers a LaTeX engine but falls back to `typst`. Documents containing CJK text prefer typst, with an explicit CJK font fallback list, except `.tex` input, which prefers a XeLaTeX-family engine with `CJKmainfont`; the default fonts have no CJK glyphs. Pandoc is given the engine's _name_, never a full path, with engine dirs appended to its `PATH`. typst gets `-V mainfont`/`codefont` only when needed: when pandoc's typst template is the old one with an empty font list, or when the document has CJK text. Font variables set in the document's own YAML front matter are never overridden. See the comments in `pdf.rs` for the pandoc quirks this works around. `.txt` input reads as Markdown (`ext_to_pandoc_input_format`) because pandoc has no `plain` reader.

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
