# PyMuPDF4LLM: Replace pdftohtml Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `pdftohtml` (poppler) as the default PDF extraction backend with `pymupdf4llm`, a Python library producing clean LLM-friendly markdown directly from PDFs.

**Architecture:** New detection/install functions mirror the existing pandoc/pdftohtml pattern — a `OnceLock<Option<PathBuf>>` tracks a Python binary known to have `pymupdf4llm` importable, auto-installed silently via pip on first use. Three conversion functions (PDF→MD, PDF→EPUB, PDF→HTML) are rewritten to call `pymupdf4llm` via `python -c`, removing the pdftohtml subprocess + HTML cleanup regex pipeline entirely.

**Tech Stack:** Rust/Tokio (existing), `which` crate (existing), `pymupdf4llm` Python package (runtime pip install), `pandoc` (existing runtime dep)

---

## File Map

| File | Action | What changes |
|---|---|---|
| `swift-shifter/src/converter/document/mod.rs` | Modify | Add `PYMUPDF4LLM_PYTHON` static, remove `PDFTOHTML_PATH` |
| `swift-shifter/src/converter/document/binaries.rs` | Modify | Add 4 pymupdf4llm functions, remove 4 pdftohtml functions |
| `swift-shifter/src/converter/document/conversion.rs` | Modify | Rewrite 3 conversion functions, remove HTML cleanup code |
| `swift-shifter/src/converter/document/utils.rs` | Modify | Remove 8 regex statics + `convert_svg_to_png` |
| `swift-shifter/src/converter/tests.rs` | Modify | Remove regex test, add pymupdf4llm detection smoke test |
| `swift-shifter/src/main.rs` | Modify | Replace `ensure_pdftohtml` block with `ensure_pymupdf4llm` |
| `swift-shifter/Cargo.toml` | Modify | Remove `regex = "1.12.3"` dependency |
| `ui/src/main.ts` | Modify | Update `DEP_DISPLAY`, `DEP_ERROR_FRAGMENTS`, `BACKGROUND_DEPS` |
| `CLAUDE.md` | Modify | Update pdftohtml constraint block and event table |

---

## Task 1: pymupdf4llm detection infrastructure

**Files:**
- Modify: `swift-shifter/src/converter/document/mod.rs`
- Modify: `swift-shifter/src/converter/document/binaries.rs`
- Modify: `swift-shifter/src/converter/tests.rs`

- [ ] **Step 1: Write the failing test**

In `swift-shifter/src/converter/tests.rs`, add inside the `mod tests` block:

```rust
#[test]
fn test_find_pymupdf4llm_python_does_not_panic() {
    // Returns Some(path) if pymupdf4llm is installed, None otherwise. Never panics.
    let _result = crate::converter::document::find_pymupdf4llm_python();
}
```

- [ ] **Step 2: Verify it fails to compile**

```bash
cargo test --manifest-path swift-shifter/Cargo.toml 2>&1 | head -20
```

Expected: error `cannot find function \`find_pymupdf4llm_python\``

- [ ] **Step 3: Add `PYMUPDF4LLM_PYTHON` static to mod.rs**

In `swift-shifter/src/converter/document/mod.rs`, add the new static alongside the existing ones:

```rust
pub static PYMUPDF4LLM_PYTHON: OnceLock<Option<PathBuf>> = OnceLock::new();
```

The file should now read:

```rust
use std::path::PathBuf;
use std::sync::OnceLock;

pub mod types;
mod utils;
pub mod binaries;
pub mod llm;
pub mod conversion;

pub use types::*;
pub use binaries::*;
pub use llm::*;
pub use conversion::*;

pub static PANDOC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static PDFTOHTML_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static EBOOK_CONVERT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static PYMUPDF4LLM_PYTHON: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static OLLAMA_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
```

(PDFTOHTML_PATH stays for now — it's removed in Task 6.)

- [ ] **Step 4: Add pymupdf4llm functions to binaries.rs**

At the end of `swift-shifter/src/converter/document/binaries.rs`, add:

```rust
fn python_has_pymupdf4llm(python: &PathBuf) -> bool {
    std::process::Command::new(python)
        .args(["-c", "import pymupdf4llm"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn find_pymupdf4llm_python() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    for name in &["python3", "python"] {
        if let Ok(p) = which::which(name) {
            candidates.push(p);
        }
    }

    #[cfg(target_os = "macos")]
    for dir in BREW_PATHS {
        for name in &["python3", "python"] {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                candidates.push(p);
            }
        }
    }

    #[cfg(target_os = "linux")]
    for path in &["/usr/bin/python3", "/usr/local/bin/python3", "/usr/bin/python"] {
        candidates.push(PathBuf::from(path));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(p) = which::which("py") {
            candidates.push(p);
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            for ver in &["Python313", "Python312", "Python311", "Python310"] {
                let candidate = PathBuf::from(&local)
                    .join("Programs")
                    .join("Python")
                    .join(ver)
                    .join("python.exe");
                if candidate.exists() {
                    candidates.push(candidate);
                }
            }
        }
    }

    // Deduplicate while preserving priority order
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|p| seen.insert(p.clone()));

    candidates.into_iter().find(|p| python_has_pymupdf4llm(p))
}

pub async fn ensure_pymupdf4llm(app: &tauri::AppHandle) -> Result<(), String> {
    use crate::converter::document::PYMUPDF4LLM_PYTHON;

    if PYMUPDF4LLM_PYTHON.get().is_some() {
        return Ok(());
    }

    if let Some(path) = find_pymupdf4llm_python() {
        PYMUPDF4LLM_PYTHON.set(Some(path)).ok();
        return Ok(());
    }

    app.emit("pymupdf:missing", ()).ok();
    app.emit("pymupdf:installing", ()).ok();

    // Try standalone pip tools first
    for pip_name in &["pip3", "pip"] {
        if let Ok(pip_path) = which::which(pip_name) {
            let ok = tokio::process::Command::new(&pip_path)
                .args(["install", "pymupdf4llm"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                if let Some(path) = find_pymupdf4llm_python() {
                    PYMUPDF4LLM_PYTHON.set(Some(path)).ok();
                    app.emit("pymupdf:installed", ()).ok();
                    return Ok(());
                }
            }
        }
    }

    // Fall back to python -m pip
    let python_names: Vec<&str> = {
        let mut names = vec!["python3", "python"];
        #[cfg(target_os = "windows")]
        names.push("py");
        names
    };

    for python_name in &python_names {
        if let Ok(python_path) = which::which(python_name) {
            let ok = tokio::process::Command::new(&python_path)
                .args(["-m", "pip", "install", "pymupdf4llm"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if ok {
                if let Some(path) = find_pymupdf4llm_python() {
                    PYMUPDF4LLM_PYTHON.set(Some(path)).ok();
                    app.emit("pymupdf:installed", ()).ok();
                    return Ok(());
                }
            }
        }
    }

    let err = "pymupdf4llm not found — install with: pip install pymupdf4llm".to_string();
    PYMUPDF4LLM_PYTHON.set(None).ok();
    app.emit("pymupdf:failed", err).ok();
    Ok(())
}

pub fn get_pymupdf4llm_python() -> Result<PathBuf, String> {
    use crate::converter::document::PYMUPDF4LLM_PYTHON;
    match PYMUPDF4LLM_PYTHON.get() {
        Some(Some(p)) => Ok(p.clone()),
        _ => find_pymupdf4llm_python().ok_or_else(|| {
            "pymupdf4llm not found — install with: pip install pymupdf4llm".to_string()
        }),
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test --manifest-path swift-shifter/Cargo.toml test_find_pymupdf4llm_python_does_not_panic -- --nocapture
```

Expected: `test tests::test_find_pymupdf4llm_python_does_not_panic ... ok`

- [ ] **Step 6: Cargo check**

```bash
cargo check --manifest-path swift-shifter/Cargo.toml
```

Expected: no errors

- [ ] **Step 7: Commit**

```bash
git add swift-shifter/src/converter/document/mod.rs swift-shifter/src/converter/document/binaries.rs swift-shifter/src/converter/tests.rs
git commit -m "feat: add pymupdf4llm detection and install infrastructure"
```

---

## Task 2: PDF→MD via pymupdf4llm

**Files:**
- Modify: `swift-shifter/src/converter/document/conversion.rs`

- [ ] **Step 1: Add `convert_pdf_to_md_via_pymupdf4llm` in conversion.rs**

Add this function immediately before the existing `convert_pdf_to_md` function (around line 969 in the current file). Find the line `pub async fn convert_pdf_to_md(` and insert the new function just above it:

```rust
async fn convert_pdf_to_md_via_pymupdf4llm(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
    let python = get_pymupdf4llm_python()?;
    let out = output_path(path, "md", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let tmp_dir = std::env::temp_dir().join(format!(
        "swift_shifter_pdf_md_{}",
        unique_tmp_suffix()
    ));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_md = tmp_dir.join("output.md");

    let result = tokio::process::Command::new(&python)
        .args([
            "-c",
            "import pymupdf4llm, sys; open(sys.argv[2], 'w', encoding='utf-8').write(pymupdf4llm.to_markdown(sys.argv[1]))",
            path,
            tmp_md.to_str().unwrap_or(""),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pymupdf4llm: {e}"))?;

    if !result.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "pymupdf4llm exited with code {}",
                result.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 50.0 },
    )
    .ok();

    if llm.enabled {
        if let Ok(content) = tokio::fs::read_to_string(&tmp_md).await {
            let processed =
                llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
            let _ = tokio::fs::write(&tmp_md, processed).await;
        }
    }

    std::fs::copy(&tmp_md, &out).map_err(|e| format!("Failed to copy output: {e}"))?;
    let _ = std::fs::remove_dir_all(&tmp_dir);

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}
```

- [ ] **Step 2: Update `convert_pdf_to_md` routing**

Find the function `pub async fn convert_pdf_to_md` (currently the last line calls `convert_pdf_to_md_via_pdftohtml`). Replace the body's last line:

Old:
```rust
    convert_pdf_to_md_via_pdftohtml(app, path, output_dir, llm).await
```

New:
```rust
    convert_pdf_to_md_via_pymupdf4llm(app, path, output_dir, llm).await
```

- [ ] **Step 3: Update marker fallback in `convert_pdf_with_marker_to_md`**

Find line 1032 (approximately):
```rust
        return convert_pdf_to_md_via_pdftohtml(app, path, output_dir, llm).await;
```

Replace with:
```rust
        return convert_pdf_to_md_via_pymupdf4llm(app, path, output_dir, llm).await;
```

- [ ] **Step 4: Cargo check**

```bash
cargo check --manifest-path swift-shifter/Cargo.toml
```

Expected: no errors (both pdftohtml and pymupdf4llm functions coexist for now)

- [ ] **Step 5: Commit**

```bash
git add swift-shifter/src/converter/document/conversion.rs
git commit -m "feat: add PDF→MD via pymupdf4llm, update routing"
```

---

## Task 3: Rewrite convert_pdf_to_epub

**Files:**
- Modify: `swift-shifter/src/converter/document/conversion.rs`

This function currently spans roughly lines 480–700. It will be replaced with a clean pymupdf4llm → MD → pandoc EPUB pipeline — about 60 lines instead of 220.

- [ ] **Step 1: Replace the `convert_pdf_to_epub` function body**

Find the function signature:
```rust
pub async fn convert_pdf_to_epub(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
```

Replace the entire function (from the `{` after the signature to its matching `}`) with:

```rust
pub async fn convert_pdf_to_epub(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
    let python = get_pymupdf4llm_python()?;
    let pandoc = get_pandoc()?;
    let out = output_path(path, "epub", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let tmp_dir = std::env::temp_dir().join(format!(
        "swift_shifter_epub_{}",
        unique_tmp_suffix()
    ));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_md = tmp_dir.join("output.md");

    // Step 1: PDF → Markdown via pymupdf4llm
    let result = tokio::process::Command::new(&python)
        .args([
            "-c",
            "import pymupdf4llm, sys; open(sys.argv[2], 'w', encoding='utf-8').write(pymupdf4llm.to_markdown(sys.argv[1]))",
            path,
            tmp_md.to_str().unwrap_or(""),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pymupdf4llm: {e}"))?;

    if !result.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "pymupdf4llm exited with code {}",
                result.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 40.0 },
    )
    .ok();

    // Optional LLM post-processing
    if llm.enabled {
        if let Ok(content) = tokio::fs::read_to_string(&tmp_md).await {
            let processed =
                llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
            let _ = tokio::fs::write(&tmp_md, processed).await;
        }
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 70.0 },
    )
    .ok();

    // Step 2: Markdown → EPUB via pandoc
    let file_title = std::path::Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let pandoc_result = tokio::process::Command::new(&pandoc)
        .current_dir(&tmp_dir)
        .args([
            "-f",
            "markdown",
            "-t",
            "epub",
            "--metadata",
            &format!("title={}", file_title),
            "-o",
            out.to_str().unwrap_or(""),
            tmp_md.to_str().unwrap_or(""),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !pandoc_result.status.success() {
        let stderr = String::from_utf8_lossy(&pandoc_result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "pandoc exited with code {}",
                pandoc_result.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}
```

- [ ] **Step 2: Cargo check**

```bash
cargo check --manifest-path swift-shifter/Cargo.toml
```

Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add swift-shifter/src/converter/document/conversion.rs
git commit -m "feat: rewrite PDF→EPUB to use pymupdf4llm→pandoc pipeline"
```

---

## Task 4: Rewrite convert_pdf_to_html

**Files:**
- Modify: `swift-shifter/src/converter/document/conversion.rs`

- [ ] **Step 1: Replace the `convert_pdf_to_html` function body**

Find the function signature:
```rust
pub async fn convert_pdf_to_html(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
```

Replace the entire function with:

```rust
pub async fn convert_pdf_to_html(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let python = get_pymupdf4llm_python()?;
    let pandoc = get_pandoc()?;
    let out = output_path(path, "html", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let tmp_dir = std::env::temp_dir().join(format!(
        "swift_shifter_html_{}",
        unique_tmp_suffix()
    ));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_md = tmp_dir.join("output.md");

    // Step 1: PDF → Markdown via pymupdf4llm
    let result = tokio::process::Command::new(&python)
        .args([
            "-c",
            "import pymupdf4llm, sys; open(sys.argv[2], 'w', encoding='utf-8').write(pymupdf4llm.to_markdown(sys.argv[1]))",
            path,
            tmp_md.to_str().unwrap_or(""),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pymupdf4llm: {e}"))?;

    if !result.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "pymupdf4llm exited with code {}",
                result.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 50.0 },
    )
    .ok();

    // Step 2: Markdown → HTML via pandoc
    let pandoc_result = tokio::process::Command::new(&pandoc)
        .args([
            "-f",
            "markdown",
            "-t",
            "html",
            "-o",
            out.to_str().unwrap_or(""),
            tmp_md.to_str().unwrap_or(""),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    let _ = std::fs::remove_dir_all(&tmp_dir);

    if !pandoc_result.status.success() {
        let stderr = String::from_utf8_lossy(&pandoc_result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "pandoc exited with code {}",
                pandoc_result.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}
```

- [ ] **Step 2: Cargo check**

```bash
cargo check --manifest-path swift-shifter/Cargo.toml
```

Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add swift-shifter/src/converter/document/conversion.rs
git commit -m "feat: rewrite PDF→HTML to use pymupdf4llm→pandoc pipeline"
```

---

## Task 5: Update main.rs startup

**Files:**
- Modify: `swift-shifter/src/main.rs`

- [ ] **Step 1: Replace `ensure_pdftohtml` startup block**

Find this block in `src/main.rs` (around line 134):

```rust
            // Check for pdftohtml (poppler) at startup — needed for PDF → EPUB/HTML/MD
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = converter::document::ensure_pdftohtml(&handle).await {
                    eprintln!("pdftohtml setup warning: {e}");
                    handle.emit("pdftohtml:failed", e).ok();
                }
            });
```

Replace with:

```rust
            // Check for pymupdf4llm at startup — needed for PDF → EPUB/HTML/MD
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = converter::document::ensure_pymupdf4llm(&handle).await {
                    eprintln!("pymupdf4llm setup warning: {e}");
                    handle.emit("pymupdf:failed", e).ok();
                }
            });
```

- [ ] **Step 2: Cargo check**

```bash
cargo check --manifest-path swift-shifter/Cargo.toml
```

Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add swift-shifter/src/main.rs
git commit -m "feat: replace ensure_pdftohtml with ensure_pymupdf4llm at startup"
```

---

## Task 6: Remove all pdftohtml code + regex crate

**Files:**
- Modify: `swift-shifter/src/converter/document/binaries.rs`
- Modify: `swift-shifter/src/converter/document/mod.rs`
- Modify: `swift-shifter/src/converter/document/conversion.rs`
- Modify: `swift-shifter/src/converter/document/utils.rs`
- Modify: `swift-shifter/src/converter/tests.rs`
- Modify: `swift-shifter/Cargo.toml`

- [ ] **Step 1: Remove pdftohtml functions from binaries.rs**

Delete the following four items from `binaries.rs`:
1. The `find_pdftohtml_binary()` function (starts at `pub fn find_pdftohtml_binary()`)
2. The `ensure_pdftohtml()` function (starts at `pub async fn ensure_pdftohtml(`)
3. The `get_pdftohtml()` function (starts at `pub fn get_pdftohtml()`)

Also remove `PDFTOHTML_PATH` from the `use crate::converter::document::{...}` import at the top of binaries.rs. The import line currently reads:

```rust
use crate::converter::document::{PANDOC_PATH, PDFTOHTML_PATH, EBOOK_CONVERT_PATH};
```

Change it to:

```rust
use crate::converter::document::{PANDOC_PATH, EBOOK_CONVERT_PATH, PYMUPDF4LLM_PYTHON};
```

(Note: `PYMUPDF4LLM_PYTHON` is already imported inline in `ensure_pymupdf4llm` and `get_pymupdf4llm_python` via `use crate::converter::document::PYMUPDF4LLM_PYTHON;` — remove those inline uses and replace with the consolidated import above.)

- [ ] **Step 2: Remove `PDFTOHTML_PATH` from mod.rs**

In `swift-shifter/src/converter/document/mod.rs`, remove this line:

```rust
pub static PDFTOHTML_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
```

- [ ] **Step 3: Remove `convert_pdf_to_md_via_pdftohtml` from conversion.rs**

Delete the entire `async fn convert_pdf_to_md_via_pdftohtml(...)` function from `conversion.rs`. It starts at:

```rust
async fn convert_pdf_to_md_via_pdftohtml(
    app: &tauri::AppHandle,
```

And ends at its closing `}` (currently around line 967).

- [ ] **Step 4: Remove HTML cleanup code from utils.rs**

In `swift-shifter/src/converter/document/utils.rs`, delete:
1. The `use regex::Regex;` import at the top
2. All 8 static regex declarations (lines 5–12):
   ```rust
   pub static RE_NUM: OnceLock<Regex> = OnceLock::new();
   pub static RE_MERGE: OnceLock<Regex> = OnceLock::new();
   pub static RE_STYLE_BLOCK: OnceLock<Regex> = OnceLock::new();
   pub static RE_CSS_POS: OnceLock<Regex> = OnceLock::new();
   pub static RE_CSS_TOP: OnceLock<Regex> = OnceLock::new();
   pub static RE_CSS_LEFT: OnceLock<Regex> = OnceLock::new();
   pub static RE_CSS_HEIGHT: OnceLock<Regex> = OnceLock::new();
   pub static RE_SVG_SRC: OnceLock<Regex> = OnceLock::new();
   ```
3. The entire `pub async fn convert_svg_to_png(svg_path: &Path) -> Option<PathBuf>` function

Also remove `OnceLock` from the `use std::sync::OnceLock;` import at the top of utils.rs since it's no longer used there — change it to just remove the `use std::sync::OnceLock;` line (the other imports can stay).

Wait — check whether `OnceLock` is still used in utils.rs after removing the regex statics. It is NOT (the only uses were the 8 regex statics). So remove the entire `use std::sync::OnceLock;` line from utils.rs.

The resulting utils.rs should only contain:
- `use std::path::{Path, PathBuf};`
- `output_path`
- `copy_dir_contents_except`
- `find_md_file`
- `ext_to_pandoc_format`
- `target_to_ext`

- [ ] **Step 5: Remove the regex test from tests.rs**

In `swift-shifter/src/converter/tests.rs`, delete the entire `test_regex` function (the only test in the file). The file should end up as:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_find_pymupdf4llm_python_does_not_panic() {
        // Returns Some(path) if pymupdf4llm is installed, None otherwise. Never panics.
        let _result = crate::converter::document::find_pymupdf4llm_python();
    }
}
```

- [ ] **Step 6: Remove `regex` from Cargo.toml**

In `swift-shifter/Cargo.toml`, delete the line:

```toml
regex = "1.12.3"
```

- [ ] **Step 7: Cargo check**

```bash
cargo check --manifest-path swift-shifter/Cargo.toml
```

Expected: no errors, no unused import warnings

- [ ] **Step 8: Run tests**

```bash
cargo test --manifest-path swift-shifter/Cargo.toml
```

Expected: `test tests::test_find_pymupdf4llm_python_does_not_panic ... ok`

- [ ] **Step 9: Commit**

```bash
git add swift-shifter/src/converter/document/binaries.rs \
        swift-shifter/src/converter/document/mod.rs \
        swift-shifter/src/converter/document/conversion.rs \
        swift-shifter/src/converter/document/utils.rs \
        swift-shifter/src/converter/tests.rs \
        swift-shifter/Cargo.toml
git commit -m "refactor: remove all pdftohtml code and regex dependency"
```

---

## Task 7: Frontend — update event listeners

**Files:**
- Modify: `ui/src/main.ts`

- [ ] **Step 1: Update `DEP_DISPLAY`**

Find (around line 66):
```typescript
const DEP_DISPLAY: Record<string, string> = {
  ffmpeg: "ffmpeg",
  pandoc: "pandoc",
  pdftohtml: "poppler",
  "ebook-convert": "Calibre",
};
```

Replace with:
```typescript
const DEP_DISPLAY: Record<string, string> = {
  ffmpeg: "ffmpeg",
  pandoc: "pandoc",
  pymupdf: "PyMuPDF",
  "ebook-convert": "Calibre",
};
```

- [ ] **Step 2: Update `DEP_ERROR_FRAGMENTS`**

Find (around line 74):
```typescript
const DEP_ERROR_FRAGMENTS: [string, string][] = [
  ["pandoc not found", "pandoc"],
  ["pdftohtml not found", "pdftohtml"],
  ["ebook-convert not found", "ebook-convert"],
  ["ffmpeg not found", "ffmpeg"],
];
```

Replace with:
```typescript
const DEP_ERROR_FRAGMENTS: [string, string][] = [
  ["pandoc not found", "pandoc"],
  ["pymupdf4llm not found", "pymupdf"],
  ["ebook-convert not found", "ebook-convert"],
  ["ffmpeg not found", "ffmpeg"],
];
```

- [ ] **Step 3: Update `BACKGROUND_DEPS`**

Find (around line 549):
```typescript
const BACKGROUND_DEPS: [string, string][] = [
  ["pandoc", "pandoc"],
  ["pdftohtml", "poppler"],
  ["ebook-convert", "Calibre"],
];
```

Replace with:
```typescript
const BACKGROUND_DEPS: [string, string][] = [
  ["pandoc", "pandoc"],
  ["pymupdf", "PyMuPDF"],
  ["ebook-convert", "Calibre"],
];
```

- [ ] **Step 4: Commit**

```bash
git add ui/src/main.ts
git commit -m "feat: replace pdftohtml UI events with pymupdf events"
```

---

## Task 8: Update CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Replace the pdftohtml constraint block**

Find the section:
```markdown
### pdftohtml (poppler) is a runtime dep, not compile-time
- Required for PDF → EPUB, PDF → HTML, and PDF → MD conversion paths.
- Auto-install: macOS (`brew install poppler`), Windows (`winget`), Linux (apt/dnf/pacman).
- Emits events during detection/install: `pdftohtml:missing`, `pdftohtml:installing`, `pdftohtml:installed`.
- If install fails, these PDF output formats are silently disabled.
```

Replace with:
```markdown
### pymupdf4llm is a runtime dep, not compile-time
- Required for PDF → EPUB, PDF → HTML, and PDF → MD conversion paths.
- Python library; auto-installed via pip (`pip install pymupdf4llm`) on first use.
- Detection: probes `python3`/`python` candidates for importability (`python -c "import pymupdf4llm"`).
- Emits events during detection/install: `pymupdf:missing`, `pymupdf:installing`, `pymupdf:installed`, `pymupdf:failed`.
- If install fails, PDF→EPUB/HTML/MD formats are silently disabled.
```

- [ ] **Step 2: Update the Event Channels table**

Find the rows:
```markdown
| `pdftohtml:missing` | — | pdftohtml not on PATH |
| `pdftohtml:installing` | — | pdftohtml installation running |
| `pdftohtml:installed` | — | pdftohtml successfully installed |
```

Replace with:
```markdown
| `pymupdf:missing` | — | pymupdf4llm not importable, attempting install |
| `pymupdf:installing` | — | pip install running |
| `pymupdf:installed` | — | pymupdf4llm successfully installed |
| `pymupdf:failed` | error string | pymupdf4llm install failed |
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md — pdftohtml replaced by pymupdf4llm"
```

---

## Verification

After all tasks are complete:

- [ ] **Full cargo check**

```bash
cargo check --manifest-path swift-shifter/Cargo.toml
```

Expected: no errors, no unused-import warnings

- [ ] **Run all tests**

```bash
cargo test --manifest-path swift-shifter/Cargo.toml
```

Expected: all tests pass

- [ ] **Verify pdftohtml references are gone**

```bash
grep -r "pdftohtml" swift-shifter/src/ ui/src/ CLAUDE.md
```

Expected: no output
