# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, etc.) working in this repository. `CLAUDE.md` is a local symlink to this file.

Swift Shifter is a Tauri v2 menu-bar file converter: a Rust backend doing the conversions, a plain-DOM TypeScript/Vite frontend, and external tools (ffmpeg, pandoc, pymupdf4llm, Calibre) for what isn't native yet. typst is linked in for PDF output.

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

`convert`/`trim` build a windowless Tauri app to get a real `AppHandle`; the rest run without one. Global flags override `Config::default()` (the saved GUI `config.toml` is never read): `--output-dir`, `--jpeg-quality`, `--avif-quality`, `--marker`. Output paths print to stdout, errors to stderr, non-zero exit on failure. Any `argv[1]` that isn't a known subcommand launches the GUI as before (`cli::is_cli_invocation` in `src/cli.rs`). Note: `generate_context!()` is called once in `main()` and passed into `cli::run` — it must not appear twice in the binary.

To check document→PDF output end to end, run `cargo run -- convert pdf in.md`, then `pdffonts in.pdf` to see which fonts were embedded (e.g. that CJK fonts were) and `pdftotext in.pdf -` to check that the text came through.

Bump the version with `./scripts/bump-version.sh 0.5.0` — it syncs the three version files (`package.json`, `swift-shifter/Cargo.toml`, `swift-shifter/tauri.conf.json`), which must always agree. See **Release process** below for how a version bump turns into a published release.

## Architecture

### Backend ↔ frontend boundary

The frontend never imports Rust; it calls `#[tauri::command]` functions over Tauri's IPC. The full command surface is the `invoke_handler!` list in `swift-shifter/src/main.rs` — that list is the contract. Adding a backend capability means writing a command there and `invoke`-ing it from `ui/src/`. The backend pushes progress and status to the UI via `app.emit(...)` events (e.g. `convert:progress`, `update:available`, `ffmpeg:failed`); the frontend listens for these.

### The conversion graph (`swift-shifter/src/converter/graph.rs`) — single source of truth

All conversions are edges in one directed graph. `graph::edges()` is the authoritative list (`Edge { from, to, cost, cond }`, where `cond` gates platform/capability — e.g. `MacOnly` for HEIC). Everything else derives from it, so detection and conversion can't drift:

- `detect_output_formats(path)` (in `mod.rs`) returns `graph::direct_targets()` — the direct (1-hop) out-edges, platform-filtered. The UI's `detect_format` command instead returns `graph::detect_with_chains()` → `DetectResult { direct, chained }`, where `chained` are formats reachable only via a multi-hop chain (each with its `route` and `hops`), shown behind the UI's "…" expander.
- `graph::find_path(from, to)` runs Dijkstra over satisfiable edges (weighted by `cost`, capped at `MAX_HOPS = 6`) to pick the min-cost chain.
- `graph::route_hop(from, to) -> Option<Hop>` classifies a single hop to its converter; `convert_file` → `run_single_hop` dispatches on the exhaustive `Hop` enum to `image.rs`, `media.rs` (ffmpeg), `data.rs` (serde), or `document/` (native engine, pandoc, pymupdf, Calibre, marker). The exhaustive match + `test_route_hop_covers_every_satisfiable_edge` structurally guarantee every offered edge is handled.

`convert_file` finds the path, then runs each hop; multi-hop chains stage intermediates in a `tempfile::TempDir` (auto-cleaned) and write only the final artifact to `output_dir`, emitting overall `convert:progress` keyed to the original path.

**To add a format pairing, edit `graph.rs`:** add the `Edge`(s) and a `route_hop` arm (+ a `run_single_hop` branch if it's a new `Hop`). Quirks encoded here: HEIC is macOS-only (`sips`); JPEG/`tif`/`typ` normalize via `normalize_ext` (so no jpg→jpg or pointless tif→tiff; `.typ` maps to the `typst` key, and Typst output is written as `.typ` via `target_to_ext`); PDF routing branches on `use_marker_pdf`; csv→toml is a semantic dead-end (`is_semantically_blocked`) even though csv→json→toml exists structurally, because a top-level array has no TOML form. **Document→PDF is typeset in-process** with typst, which is linked as a library (`document/native/typst/`). There is no typst binary. The one exception is `.tex` input: when a LaTeX engine is installed (tectonic, xelatex, lualatex or pdflatex; see `document/pdf.rs`), pandoc drives it, since LaTeX keeps raw LaTeX that pandoc's typst writer would drop. Otherwise `.tex` goes pandoc → typst source → the embedded typst.

- **md, txt, html → pdf** go through the native typst writer (`native/write/typst.rs`, which also writes `.typ` output) and `native::typeset`. TeX math is converted with `mitex`. Its Typst scope is vendored in `native/typst/mitex/` and served under the virtual `/__shifter__/` path, together with a symbol-rename table for typst 0.15 (`typst/math.rs`). A formula that still fails to compile is shown as TeX code instead of failing the document.
- **typst → pdf** compiles the file directly (`native::typst_file_to_pdf`, called from `native::convert`), not through the Typst reader. `@preview` packages are downloaded through reqwest into typst's usual cache.
- **Images** become a page the size of the image (`native::image_to_pdf`).
- **tex without a LaTeX engine** goes pandoc `-t typst` → embedded typst with our preamble (`convert_via_pandoc_typst`). epub → pdf is native (EPUB reader → `native::typeset`).
- **`.tex` with a LaTeX engine installed** still uses pandoc + that engine (`pdf.rs`, which is now LaTeX-only and sets `CJKmainfont` for CJK), because it keeps raw LaTeX.
- **Fonts** are typst's embedded ones plus system fonts, scanned once (`typst::warm_up` at startup). CJK families are picked per detected language (`zh-Hant`/`zh-Hans`/`ja`/`ko`). A document with CJK text but no CJK font fails with an install hint instead of drawing empty boxes.

**Native document engine (`document/native/`).** This replaces pandoc one hop at a time; see `docs/superpowers/specs/2026-10-15-native-document-converter-design.md`. Readers (`read/markdown.rs` via comrak, which keeps HTML blocks verbatim as `Block::Raw` and folds bare inline tag pairs like `<sup>…</sup>` into AST nodes; `read/html.rs` via scraper, which also reads EPUB/pandoc/ARIA footnote markup; `read/epub.rs`; `read/typst/`, with math converted to TeX by `read/typst_math.rs`; and `read/plain.rs`) build an `ast::Document`, and writers (`write/html.rs`, `write/markdown.rs`, `write/plain.rs`, `write/typst.rs`, `write/epub.rs`, `write/latex.rs`) render it. `route_hop` sends a pair to `Hop::DocumentNative` whenever `native::handles(from, to)` holds, which today means md, txt, html, epub and typst input, and md, txt, html, pdf, typst, epub and tex output. Only `tex` input stays on `DocumentPandoc`.

- **Inputs.** html is an input format only: html → md/txt/pdf/typst/epub/tex directly. Edges into `txt` and `typst` cost 5, so chains pass through md instead. The native `.txt` reader is literal: blank lines separate paragraphs, newlines are line breaks, and there's no Markdown interpretation.
- **pdf→html and pdf→epub** keep pymupdf4llm (or marker) for extraction and use `native::markdown_to_html` / `native::markdown_to_epub` for the last step. **mobi→md** keeps Calibre for MOBI→EPUB and then reads the EPUB natively.
- **Typst input** (`read/typst/`):
  - **Files:** `mod.rs` reads markup and calls; `names.rs` follows what `#import` and `#let` bind; `wrapper.rs` has the wrappers of tables and their parts, the presets made with `.with`, and the arguments they give; `signature.rs` works out what a call shows of its arguments; `table.rs` reads tables and grids; and `finish.rs` settles labels, references and smart quotes once the whole document is read.
  - **Parsing:** `typst::syntax` markup is walked into the AST. `#set document(…)` and `#set text(lang:)` fill the metadata, and `#set enum(start:)` numbers lists.
  - **Content:** headings with labels (so `@label` refs become heading links), lists and term lists, line breaks, raw text, footnotes (including `<label>` repeats), and `image`, `figure` (with its caption), `link`, `ref`, `quote` (with a block quote's attribution), `heading`, `list`, `enum`, `terms` (and their items) and `table` calls are read. Images, shapes and `#par` are blocks of their own, as typst lays them out (an image in a `#box` stays in the line), `#hide` shows nothing, and symbols (`#sym.arrow`, `#emoji.rocket`) show their text. Math is converted to TeX by `typst_math.rs`.
  - **Other calls:** what a call shows stays where typst puts it. A table in `#text[…]` breaks the paragraph around the call, and the text before and after it joins that paragraph. A `#box` keeps what it holds in the line, unless that can't be written inline (a table). A function of typst's own shows the arguments that go to its parameters taking content, matched as typst matches them (by type for those that may be left out, so `#align(body)` shows `body`), so `#text(rgb("#f00"))[x]` shows `x`; and of a value it makes of a string, that string (`#int("12")`, `#str.trim(s)`, but not a path or a pattern: `#read("a.txt")`, `#numbering("1.", n)`). A function the document defines shows the content and strings it's given for the parameters its body shows (`#let thm(name, body) = [#name: #body]`), as far as the reader can tell: through set rules (`set text(lang: x)` shows no `x`), math, `.with` presets (which take its first parameters), and what it gives a function the reader can't see (content given there by name, where a string is a setting, as for one it can't see itself: `showybox(frame: f, body)`). It shows nothing if it returns a setting (`#let gap(x) = 1em`). One imported from where the reader can't see shows its content, by position or by name (`title: emph[…]`), and its strings, except where it takes settings: strings before its first content argument (`#badge("#f00")[hot]`), and strings by name, but by a name for what it shows (`title:`, `caption:`, `body:` and the like, so `#showybox(title: "Hint")` shows `Hint`). By name, what the reader can't tell is content is a setting too (`stroke: thick()`, `fill: black.lighten(20%)`). A function a module or a dictionary holds (`#lib.note("x")`, `#d.note("x")`) is called as by its own name, and a preset of either kind (`#let tip = clue.with(title: [Tip])`) is given what it presets first, as typst gives it. A method of a value typst makes (`#state("x").update[…]`, `#arr.push(x)`) shows nothing, and one of a value the reader can't tell (`#theme.callout[…]`, `theme` made by a package) the content it's given.
  - **Names:** names `#import` and `#let` bind (destructuring included) are followed: to the tables, grids and table parts they stand for (a header made beforehand keeps its cells, and a function or `.with` preset wrapping one keeps the arguments it adds, the caller's going where it spreads them, or what it picks of them: `..args.named()`, `args.pos().at(1)`, `.slice(…)`, `.map(…)`; the table's parts it gives using its parameters, `table.header(title, [B])`, are given what those are given), to the functions of typst's own they wrap under the same name (`#let link(dest, body) = std.link(dest, underline(body))`), and so a document's own `line` or `h` isn't typst's. That includes `.typ` files imported from inside the input's folder, each read once for what it defines. In a table, a call to a name the reader can't see gives a cell per argument it shows if it shows only arguments given by position and one is content (a header's wrapper); otherwise, shown anything by name (`showybox(title: […])[…]`), it's one cell. Given nothing but settings by name, it's a line, as is such a name placed on its own (`toprule`). The work of unwrapping calls to wrappers is capped in proportion to the document's length (`MAX_UNWRAPPED`), so wrappers doubling what they're given (`#let w1(..c) = w0(..c, ..c)`, again and again) stay fast.
  - **Lists:** a list is loose when one of an item's blocks would run into the one before it, written on the next line (`loosens`): two paragraphs, a paragraph after a quote or a table, and so on, as Markdown reads them.
  - **Dropped:** styling, layout and code (`set`/`show` rules, `let`, spacing). Spacing and breaks between blocks (`#v`, `#pagebreak()`) still end the paragraph. Calls and markup nested more than 128 deep (typst stops at 64) are kept as written, so the reader's recursion fits a 2 MiB stack. Values nest in one another (`#let a = (a,)`, `#let g = g.with()`) no deeper than that, and a document's tables grow past their cells (spans, columns left empty at their end) and its references copy heading text only up to a bound (`MAX_GROWN_CELLS`, `MAX_REFERENCE_TEXT`).
- **LaTeX output** (`write/latex.rs`):
  - **Compatibility:** a standalone `article` that compiles with pdfLaTeX, XeLaTeX or LuaLaTeX (`iftex`). CJK documents load xeCJK or luatexja and stop pdfLaTeX with a clear message. xeCJK sets the punctuation Latin text shares with CJK (`“ ” ‘ ’ – — … ·`) as CJK, dropping the spaces next to it, so in those documents the writer writes it as its text commands (`\textquotedblleft{}`) wherever it isn't next to CJK text.
  - **Content:** headings get ASCII `\label`s (other characters as their hex code point between colons, `:3b1:` for α, so no two ids share one) that in-document links reach through `\hyperref`, and a plain short form for the table of contents and bookmarks, with their math under its LaTeX names (each command protected in math that defines one, as LaTeX writes the short form out). Footnotes go inline at the first reference, and later references point back through `\ref`. A note inside a note is marked there (`\footnotemark[n]`) and set after the outermost note (`\footnotetext[n]`), numbered as typst numbers it; so is a note in struck-out text, a script or a table's header row, where LaTeX would lose it (a header row's are set at the start of the first body row). Past 16 notes deep (`MAX_NOTE_NESTING`), a note's text is queued rather than written inside the one before, so a chain of notes stays within the stack; the notes in it are numbered after those marked by then. Tables are longtables, and a cell with line breaks becomes a nested tabular.
  - **Math:** the AST keeps math characters as typed (`α`, `→`, `ℝ`), which every other writer's renderer draws. `write/latex_math.rs` gives them their amsmath/amssymb names, leaving text arguments like `\text{…}` alone but for the math inside them (and `\verb`'s entirely), and the preamble defines the few names those packages lack (`\coloneqq`, `\Alpha`, …) under `\ifdefined`, only for a document that uses them. A formula that `\newcommand`s or `\providecommand`s one of them itself keeps its own.
  - **Images** are always copied beside the output as `<name>-images/N.ext` (`copy_images_for_latex`). PNG, JPEG and PDF are copied as they are, other rasters become PNG, and SVG falls back to its alt text.
- **EPUB input** (`read/epub.rs`):
  - **Extraction:** the book is extracted to a temp folder (zip-slip safe, size-capped). The OPF gives the metadata, the cover and the spine; the nav page is skipped.
  - **Chapters:** each chapter is parsed as XML first, so self-closing `<a/>` tags work, and falls back to HTML. Chapters are read with `read::html::read_book_page`, where `<header>`/`<aside>` are content and title pages are skipped.
  - **Links:** links to another chapter's heading become `#slug`, matching the writers' heading ids. Other in-book links keep only their text.
  - **Footnotes and images:** footnote labels are made unique across chapters. For md/html/typst output, images are copied beside the output (`<name>-images/`).
- **Output.** HTML is standalone, with a light/dark stylesheet, `lang` (CJK auto-detected), MathML via pulldown-latex with a TeX annotation (typed large operators like `∑` take typst's limits), and numbered footnotes. Relative image and link targets become absolute `file://` URLs when the output lands in a different directory.
- **EPUB 3** (`write/epub.rs`):
  - **Rendering:** chapters are rendered by the HTML writer, then re-parsed with html5ever and serialized as XHTML (`write/xhtml.rs`). That keeps raw HTML from the source well-formed, puts MathML and SVG in their namespaces, and drops scripts and remote embeds.
  - **Chapters:** the book is split at the top heading level, or the level below a lone title heading.
  - **Footnotes:** each note is an `<aside epub:type="footnote">` in the chapter that first cites it.
  - **Images:** local images are packaged, and non-EPUB formats are converted to PNG. Remote images become their alt text.
  - **Cover and TOC:** front matter `cover-image:` sets the cover, and `nav.xhtml` and `toc.ncx` are built from the headings.

### External binary management — two strategies by platform

`document/binaries.rs` and `downloader.rs` locate or install tools. macOS resolves system/Homebrew binaries (`/opt/homebrew/bin`, `/usr/local/bin`) and auto-installs via brew. **Linux/Windows download pinned prebuilt binaries** described in `tools.toml` into `dirs::data_local_dir()/swift-shifter`. `tools.toml` is the single source of truth for tool versions/URLs/checksums (`include_str!`'d at compile time) — bump a tool there, no Rust changes needed. Each external tool has an `ensure_*` function spawned at startup in `main.rs setup()`; failures are non-fatal and surfaced to the UI via events.

### Config and shared state

`config.rs` defines `Config` (persisted as TOML in `dirs::config_dir()/swift-shifter/config.toml`) and `AppState`, a Tauri-managed struct holding `Mutex<Config>`. A config saved by an older version loads with the settings it no longer knows skipped (`use_local_llm`, `local_llm_*`) and the rest kept. Values are clamped on both load and save. **Critical async rule** (see comments in `main.rs`): never hold the `std::sync::Mutex` guard across an `.await` — always `.clone()` the config out of the lock first, then await. `set_config` validates/clamps, persists, and emits `config:updated`.

### Windows behavior

Closing the main window hides it rather than quitting (tray app stays resident); real exit goes through the `quit` command / tray.

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
