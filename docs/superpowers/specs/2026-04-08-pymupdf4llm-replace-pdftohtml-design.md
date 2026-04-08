# Design: Replace pdftohtml with PyMuPDF4LLM

**Date:** 2026-04-08  
**Branch:** feat/pdf-operation  
**Status:** Approved

---

## Summary

Replace `pdftohtml` (poppler) as the default PDF extraction backend with `pymupdf4llm`, a Python library that produces clean, LLM-friendly markdown directly from PDFs. This removes a C binary subprocess dependency and replaces it with a Python subprocess that follows the same detection/install/event pattern already used for pandoc.

---

## Scope

- Full removal of all `pdftohtml` code and events
- New `pymupdf4llm` detection, installation, and invocation layer
- Three conversion paths updated: PDF→MD, PDF→EPUB, PDF→HTML
- No new Rust crates
- No changes to `marker-pdf` integration

---

## Architecture

### Binary Detection (`binaries.rs` + `mod.rs`)

New static in `mod.rs`:
```rust
pub static PYMUPDF4LLM_PYTHON: OnceLock<Option<PathBuf>> = OnceLock::new();
```

**`find_pymupdf4llm_python() -> Option<PathBuf>`**  
Iterates Python candidate binaries in priority order:
- `python3`, `python` via `which`
- macOS brew paths: `/opt/homebrew/bin/python3`, `/usr/local/bin/python3`
- Linux: `/usr/bin/python3`, `/usr/local/bin/python3`
- Windows: `py.exe`, `python.exe` via `which` and `%LOCALAPPDATA%\Programs\Python\`

For each candidate, runs `python -c "import pymupdf4llm"` and returns the first binary where that exits successfully.

**`ensure_pymupdf4llm(app: &tauri::AppHandle) -> Result<(), String>`**  
Called at app startup alongside `ensure_pandoc`. Logic:
1. If `PYMUPDF4LLM_PYTHON` already set → return `Ok(())`
2. Try `find_pymupdf4llm_python()` → if found, set OnceLock and return
3. Emit `pymupdf:missing`
4. Try install in order: `pip3 install pymupdf4llm` → `pip install pymupdf4llm` → `python3 -m pip install pymupdf4llm` → `python -m pip install pymupdf4llm` → `py -m pip install pymupdf4llm` (Windows `py` launcher)
5. Re-run `find_pymupdf4llm_python()`
6. If found: set OnceLock, emit `pymupdf:installed`, return `Ok(())`
7. If not: set OnceLock to `None`, emit `pymupdf:failed` with error string

**`get_pymupdf4llm_python() -> Result<PathBuf, String>`**  
Returns stored path or falls back to `find_pymupdf4llm_python()`, error if unavailable.

---

## Conversion Pipeline (`conversion.rs`)

### PDF→MD (`convert_pdf_to_md_via_pymupdf4llm`)

```
python -c "import pymupdf4llm, sys; open(sys.argv[2],'w').write(pymupdf4llm.to_markdown(sys.argv[1]))" input.pdf output.md
```

Then applies existing chunked LLM post-processing (Ollama) unchanged.

Routing in `convert_pdf_to_md`:
```rust
if use_marker && marker_available() {
    return convert_pdf_with_marker_to_md(app, path, output_dir, llm).await;
}
convert_pdf_to_md_via_pymupdf4llm(app, path, output_dir, llm).await
```

The marker fallback path (when marker fails mid-conversion) also routes to `_via_pymupdf4llm` instead of `_via_pdftohtml`.

### PDF→EPUB (`convert_pdf_to_epub_via_pymupdf4llm`)

1. Run pymupdf4llm → write to temp `output.md`
2. Run `pandoc output.md -t epub -o out.epub` (same pandoc EPUB flags as existing marker path)
3. Clean up temp dir

### PDF→HTML (`convert_pdf_to_html_via_pymupdf4llm`)

1. Run pymupdf4llm → write to temp `output.md`
2. Run `pandoc output.md -t html -o out.html`
3. Clean up temp dir

This replaces the previous direct `pdftohtml -noframes -nodrm` HTML dump.

---

## Event Channels

Replaces `pdftohtml:*` events:

| Event | Payload | When |
|---|---|---|
| `pymupdf:missing` | — | pymupdf4llm not importable, attempting install |
| `pymupdf:installing` | — | pip install running |
| `pymupdf:installed` | — | successfully installed |
| `pymupdf:failed` | error string | install failed |

Frontend (`main.ts`) event listeners are renamed from `pdftohtml:*` to `pymupdf:*`.

---

## Removals

### `binaries.rs`
- `PDFTOHTML_PATH` static
- `find_pdftohtml_binary()`
- `ensure_pdftohtml()`
- `get_pdftohtml()`

### `conversion.rs`
- `convert_pdf_to_epub_via_pdftohtml()`
- `convert_pdf_to_html_via_pdftohtml()`
- `convert_pdf_to_md_via_pdftohtml()`

### `main.rs`
- `ensure_pdftohtml(app)` startup call

### `mod.rs`
- `PDFTOHTML_PATH` declaration and re-export

### Frontend (`main.ts`)
- `pdftohtml:*` event listeners

### `CLAUDE.md`
- pdftohtml constraint block
- pdftohtml rows in event table

---

## Dependencies

No `Cargo.toml` changes required. `pymupdf4llm` is a Python package installed at runtime via pip. No new Rust crates.

---

## Out of Scope

- Changes to `marker-pdf` integration
- Changes to LLM post-processing
- Any new UI for pymupdf4llm (follows silent pandoc-style install)
- Image extraction from PDFs (pymupdf4llm supports this but is not enabled in this change)
