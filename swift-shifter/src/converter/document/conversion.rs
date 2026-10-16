use crate::converter::document::binaries::*;
use crate::converter::document::llm::*;
use crate::converter::document::pdf;
use crate::converter::document::types::*;
use crate::converter::document::utils::*;
use crate::converter::graph::normalize_ext;
use std::path::{Path, PathBuf};
use tauri::Emitter;

fn unique_tmp_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let tid = std::thread::current().id();
    format!("{ms}_{tid:?}").replace(['(', ')', ' '], "_")
}

/// Convert a PDF to EPUB using the marker-pdf ML pipeline:
///   1. `marker input.pdf --output_dir tmp` — produces Markdown + images
///   2. the native EPUB writer packages them
pub async fn convert_pdf_with_marker(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
    let marker = find_marker_binary()
        .ok_or_else(|| "marker not found — install with: pip install marker-pdf".to_string())?;

    let user_output_dir = output_dir;
    let out = output_path(path, "epub", user_output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_base =
        std::env::temp_dir().join(format!("swift_shifter_marker_{}", unique_tmp_suffix()));
    let input_dir = tmp_base.join("input");
    let output_dir = tmp_base.join("output");

    std::fs::create_dir_all(&input_dir)
        .map_err(|e| format!("Failed to create input temp directory: {e}"))?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output temp directory: {e}"))?;

    let tmp_pdf = input_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf).map_err(|e| format!("Failed to copy PDF: {e}"))?;

    let marker_name = marker
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut cmd = crate::process::async_command(&marker);
    if marker_name.contains("marker_single") {
        // v1.x+ CLI: marker_single FPATH --output_dir DIR
        cmd.args([
            tmp_pdf.to_str().unwrap_or(""),
            "--output_dir",
            output_dir.to_str().unwrap_or(""),
        ]);
    } else {
        cmd.args([
            input_dir.to_str().unwrap_or(""),
            "--output_dir",
            output_dir.to_str().unwrap_or(""),
        ]);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn marker: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "marker: stdout not piped".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "marker: stderr not piped".to_string())?;
    let app_handle = app.clone();
    let path_str = path.to_string();

    let progress_task = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stdout).lines();
        let mut err_reader = BufReader::new(stderr).lines();

        let stages: &[(&str, f32)] = &[
            ("load model", 5.0),
            ("detection model", 8.0),
            ("texify", 12.0),
            ("recognition model", 12.0),
            ("surya", 14.0),
            ("reading pdf", 18.0),
            ("pdf loaded", 20.0),
            ("running layout", 28.0),
            ("layout detection", 28.0),
            ("running ocr", 38.0),
            ("text extraction", 40.0),
            ("running line", 44.0),
            ("post-processing", 60.0),
            ("ordering blocks", 65.0),
            ("merging lines", 68.0),
            ("cleaning text", 72.0),
            ("formatting", 75.0),
            ("saving output", 88.0),
            ("writing markdown", 88.0),
            ("saved to", 90.0),
        ];

        let mut current_pct: f32 = 2.0;
        let mut stdout_done = false;
        let mut stderr_done = false;

        app_handle
            .emit(
                "convert:progress",
                ProgressPayload {
                    path: path_str.clone(),
                    percent: current_pct,
                },
            )
            .ok();

        fn advance_from_line(line: &str, stages: &[(&str, f32)], current: f32) -> f32 {
            let lower = line.to_lowercase();
            let mut best = current;
            for (kw, pct) in stages {
                if *pct > current && lower.contains(kw) && *pct > best {
                    best = *pct;
                }
            }
            best
        }

        fn tqdm_percent(line: &str) -> Option<f32> {
            let pos = line.find('%')?;
            let start = pos.saturating_sub(4);
            let num_str = line[start..pos].trim_start_matches(|c: char| !c.is_ascii_digit());
            num_str.parse::<f32>().ok()
        }

        loop {
            if stdout_done && stderr_done {
                break;
            }

            tokio::select! {
                line = reader.next_line(), if !stdout_done => {
                    match line {
                        Ok(Some(l)) => {
                            if let Some(p) = tqdm_percent(&l) {
                                let mapped = 20.0 + (p / 100.0) * 68.0;
                                if mapped > current_pct { current_pct = mapped; }
                            } else {
                                let new_pct = advance_from_line(&l, stages, current_pct);
                                if new_pct > current_pct { current_pct = new_pct; }
                            }
                            app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();
                        }
                        _ => { stdout_done = true; }
                    }
                }
                line = err_reader.next_line(), if !stderr_done => {
                    match line {
                        Ok(Some(l)) => {
                            if let Some(p) = tqdm_percent(&l) {
                                let mapped = 20.0 + (p / 100.0) * 68.0;
                                if mapped > current_pct { current_pct = mapped; }
                            } else {
                                let new_pct = advance_from_line(&l, stages, current_pct);
                                if new_pct > current_pct { current_pct = new_pct; }
                            }
                            app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();
                        }
                        _ => { stderr_done = true; }
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(800)) => {
                    if current_pct < 85.0 {
                        current_pct += 0.3;
                        app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();
                    }
                }
            }
        }
    });

    let status = child
        .wait()
        .await
        .map_err(|e| format!("marker wait error: {e}"))?;
    let _ = progress_task.await;

    if !status.success() || find_md_file(&output_dir).is_none() {
        let _ = std::fs::remove_dir_all(&tmp_base);
        return convert_pdf_to_epub(app, path, user_output_dir, llm).await;
    }

    let md_file =
        find_md_file(&output_dir).ok_or_else(|| "marker produced no Markdown file".to_string())?;

    let mut content = tokio::fs::read_to_string(&md_file)
        .await
        .map_err(|e| format!("Failed to read marker's output: {e}"))?;
    if llm.enabled {
        content = llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 97.0,
        },
    )
    .ok();

    // Images marker extracted sit next to its Markdown.
    let md_dir = md_file.parent().unwrap_or(&output_dir).to_path_buf();
    let title = file_stem(path);
    let epub = tokio::task::spawn_blocking(move || {
        super::native::markdown_to_epub(&content, &title, &md_dir)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"));
    let _ = std::fs::remove_dir_all(&tmp_base);
    let epub = epub??;
    tokio::fs::write(&out, epub)
        .await
        .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();

    Ok(out.to_string_lossy().to_string())
}

/// The file name without its extension, as a fallback document title.
fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

pub async fn convert_document(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let input_ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // typst → pdf compiles the file with the embedded typst; pandoc's typst
    // reader would lose most of the document.
    if normalize_ext(&input_ext) == "typst" && target_format == "pdf" {
        return convert_typst_to_pdf(app, path, output_dir).await;
    }

    let pandoc = get_pandoc()?;

    let out = output_path(path, target_to_ext(target_format), output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    // Plain text is read literally by the native reader and handed to pandoc
    // as Markdown, so .txt keeps the same meaning whatever the target. The
    // temp dir lives until pandoc is done.
    let staged_txt = if normalize_ext(&input_ext) == "txt" {
        let text = tokio::fs::read(path)
            .await
            .map_err(|e| format!("Failed to read {path}: {e}"))?;
        let dir = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {e}"))?;
        let staged = dir.path().join("input.md");
        let markdown = super::native::plain_text_to_markdown(&String::from_utf8_lossy(&text));
        tokio::fs::write(&staged, markdown)
            .await
            .map_err(|e| format!("Failed to stage {path}: {e}"))?;
        Some((dir, staged))
    } else {
        None
    };
    let (source, from_fmt) = match &staged_txt {
        // GFM is the dialect the native writer targets; pandoc's own
        // Markdown would still read citations, smart quotes, definition
        // lists and line blocks into the text.
        Some((_, staged)) => (staged.as_path(), "gfm"),
        None => (Path::new(path), ext_to_pandoc_input_format(&input_ext)),
    };
    let to_fmt = ext_to_pandoc_format(target_format);

    // PDF (only `.tex` reaches here): LaTeX when an engine is installed,
    // otherwise pandoc → typst → embedded typst.
    let latex = if target_format == "pdf" {
        let setup = (normalize_ext(&input_ext) == "tex")
            .then(|| pdf::latex_setup(source))
            .flatten();
        if setup.is_none() {
            return convert_via_pandoc_typst(app, path, &pandoc, source, from_fmt, &out).await;
        }
        setup
    } else {
        None
    };

    let mut cmd = crate::process::async_command(&pandoc);
    cmd.args([
        "-f",
        from_fmt,
        "-t",
        to_fmt,
        "-o",
        out.to_str().unwrap_or(""),
    ]);

    if let Some(latex) = latex {
        cmd.args(&latex.args);
        if let Some(path) = latex.path {
            cmd.env("PATH", path);
        }
    }

    cmd.arg(source);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    let stderr_out = if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncReadExt, BufReader};
        let mut buf = String::new();
        BufReader::new(stderr).read_to_string(&mut buf).await.ok();
        buf
    } else {
        String::new()
    };

    let status = child
        .wait()
        .await
        .map_err(|e| format!("pandoc wait error: {e}"))?;

    if !status.success() {
        let msg = if stderr_out.trim().is_empty() {
            format!("pandoc exited with code {}", status.code().unwrap_or(-1))
        } else {
            stderr_out.trim().to_string()
        };
        return Err(msg);
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();

    Ok(out.to_string_lossy().to_string())
}

/// PDF via pandoc's typst writer and the embedded typst compiler, for tex
/// without a LaTeX engine.
async fn convert_via_pandoc_typst(
    app: &tauri::AppHandle,
    path: &str,
    pandoc: &Path,
    source: &Path,
    from_fmt: &str,
    out: &Path,
) -> Result<String, String> {
    let progress = |percent: f32| {
        app.emit(
            "convert:progress",
            ProgressPayload {
                path: path.to_string(),
                percent,
            },
        )
        .ok();
    };
    progress(0.0);
    let stem = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let pdf = pandoc_typst_pdf(pandoc, source, from_fmt, &stem).await?;
    tokio::fs::write(out, pdf)
        .await
        .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;
    progress(100.0);
    Ok(out.to_string_lossy().to_string())
}

/// Pandoc turns `source` into typst markup (extracting images next to it),
/// and Swift Shifter typesets it under its own page template with the
/// document's own title and authors (`fallback_title` when it has none).
pub(crate) async fn pandoc_typst_pdf(
    pandoc: &Path,
    source: &Path,
    from_fmt: &str,
    fallback_title: &str,
) -> Result<Vec<u8>, String> {
    let stage = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let source = std::path::absolute(source)
        .map_err(|e| format!("Failed to resolve {}: {e}", source.display()))?;
    let source_dir = source.parent().unwrap_or(Path::new(".")).to_path_buf();
    // Pandoc runs from the stage so extracted media get relative paths that
    // the typst project rooted there can read on every platform; images,
    // `\input` and `\include` still resolve from the source's folder (the
    // trailing empty entry keeps TeX's default search path).
    let texinputs = std::env::join_paths([source_dir.clone(), PathBuf::new()])
        .map_err(|e| format!("Invalid source folder: {e}"))?;
    let output = crate::process::async_command(pandoc)
        .current_dir(stage.path())
        .env("TEXINPUTS", texinputs)
        .args([
            "-f",
            from_fmt,
            "-t",
            "typst",
            "--extract-media=media",
            "-o",
            "body.typ",
        ])
        .arg(format!("--resource-path={}", source_dir.display()))
        .arg(&source)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "pandoc exited with code {}",
                output.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }
    let body = tokio::fs::read_to_string(stage.path().join("body.typ"))
        .await
        .map_err(|e| format!("Failed to read pandoc's output: {e}"))?;
    let mut meta = pandoc_metadata(pandoc, &source, from_fmt, stage.path()).await;
    if meta.title.is_none() {
        meta.title = Some(fallback_title.to_string());
    }
    let root = stage.path().to_path_buf();
    let pdf = tokio::task::spawn_blocking(move || {
        super::native::typeset_pandoc_typst(&body, &root, &meta)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))??;
    drop(stage);
    Ok(pdf)
}

/// The document's title and authors, as pandoc reads them. Empty when pandoc
/// can't tell (metadata is a nicety; its absence never fails a conversion).
async fn pandoc_metadata(
    pandoc: &Path,
    source: &Path,
    from_fmt: &str,
    stage: &Path,
) -> super::native::ast::Meta {
    let template = stage.join("meta.txt");
    let mut meta = super::native::ast::Meta::default();
    if tokio::fs::write(
        &template,
        "$title$\n$for(author)$$author$$sep$\u{1f}$endfor$\n",
    )
    .await
    .is_err()
    {
        return meta;
    }
    let Ok(output) = crate::process::async_command(pandoc)
        .args(["-f", from_fmt, "-t", "plain", "--wrap=none"])
        .arg(format!("--template={}", template.display()))
        .arg(source)
        .stderr(std::process::Stdio::null())
        .output()
        .await
    else {
        return meta;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    meta.title = lines
        .next()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    meta.authors = lines
        .next()
        .unwrap_or("")
        .split('\u{1f}')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect();
    meta
}

/// Compile a Typst document straight to PDF with the embedded typst
/// compiler. `@preview/…` packages it imports are downloaded on first use.
pub async fn convert_typst_to_pdf(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let out = output_path(path, "pdf", output_dir)?;
    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();
    let input = PathBuf::from(path);
    let pdf = tokio::task::spawn_blocking(move || super::native::typst_file_to_pdf(&input))
        .await
        .map_err(|e| format!("task panicked: {e}"))??;
    tokio::fs::write(&out, pdf)
        .await
        .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;
    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

pub async fn convert_image_to_pdf(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let out = output_path(path, "pdf", output_dir)?;
    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let input = path.to_string();
    let pdf = tokio::task::spawn_blocking(move || image_pdf_bytes(&input))
        .await
        .map_err(|e| format!("task panicked: {e}"))??;
    tokio::fs::write(&out, pdf)
        .await
        .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

/// A PDF page the size of the image. typst reads PNG, JPEG, GIF, WebP and
/// SVG; other formats are converted to PNG first.
fn image_pdf_bytes(path: &str) -> Result<Vec<u8>, String> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg"
    ) {
        return super::native::image_to_pdf(Path::new(path));
    }
    let stage = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let stage_dir = stage.path().to_string_lossy().into_owned();
    let png = if matches!(ext.as_str(), "heic" | "heif") {
        crate::converter::image::convert_heic(path, "png", Some(&stage_dir))?
    } else {
        crate::converter::image::convert_image(path, "png", 90, Some(&stage_dir))?
    };
    super::native::image_to_pdf(Path::new(&png))
}

pub async fn convert_pdf_to_epub(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
    let python = get_pymupdf4llm_python()?;
    let out = output_path(path, "epub", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_dir = std::env::temp_dir().join(format!("swift_shifter_epub_{}", unique_tmp_suffix()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_md = tmp_dir.join("output.md");

    let tmp_md_str = tmp_md
        .to_str()
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())?;

    // Step 1: PDF → Markdown via pymupdf4llm
    let result = crate::process::async_command(&python)
        .args([
            "-c",
            "import pymupdf4llm, sys; open(sys.argv[2], 'w', encoding='utf-8').write(pymupdf4llm.to_markdown(sys.argv[1]))",
            path,
            tmp_md_str,
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

    if !tmp_md.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("pymupdf4llm succeeded but produced no output file".to_string());
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 40.0,
        },
    )
    .ok();

    let read = tokio::fs::read_to_string(&tmp_md).await;
    let mut content = match read {
        Ok(content) => content,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(format!("Failed to read pymupdf4llm's output: {e}"));
        }
    };
    if llm.enabled {
        content = llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 70.0,
        },
    )
    .ok();

    // Step 2: Markdown → EPUB with the native writer
    let title = file_stem(path);
    let base_dir = tmp_dir.clone();
    let epub = tokio::task::spawn_blocking(move || {
        super::native::markdown_to_epub(&content, &title, &base_dir)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    tokio::fs::write(&out, epub??)
        .await
        .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

async fn run_ebook_convert(
    app: &tauri::AppHandle,
    input: &str,
    output: &Path,
) -> Result<(), String> {
    let ec = get_ebook_convert()?;
    app.emit(
        "convert:progress",
        ProgressPayload {
            path: input.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let result = crate::process::async_command(&ec)
        .args([
            input,
            output.to_str().unwrap_or(""),
            "--filter-css",
            "background-color,background,color",
            "--extra-css",
            "body, html { background-color: white !important; color: black !important; }",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn ebook-convert: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!(
                "ebook-convert exited with code {}",
                result.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: input.to_string(),
            percent: 100.0,
        },
    )
    .ok();
    Ok(())
}

pub async fn convert_mobi(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    match target_format {
        "pdf" | "epub" | "html" => {
            let out = output_path(path, target_format, output_dir)?;
            run_ebook_convert(app, path, &out).await?;
            Ok(out.to_string_lossy().to_string())
        }
        "md" => {
            let out = output_path(path, "md", output_dir)?;
            let tmp_epub = std::env::temp_dir()
                .join(format!("swift_shifter_mobi_{}.epub", unique_tmp_suffix()));
            run_ebook_convert(app, path, &tmp_epub).await?;

            app.emit(
                "convert:progress",
                ProgressPayload {
                    path: path.to_string(),
                    percent: 50.0,
                },
            )
            .ok();

            // Calibre's EPUB → Markdown with the native reader; its images
            // are copied next to the output.
            let epub = tmp_epub.clone();
            let md = out.clone();
            let result =
                tokio::task::spawn_blocking(move || super::native::convert(&epub, "md", &md))
                    .await
                    .map_err(|e| format!("task panicked: {e}"));
            let _ = std::fs::remove_file(&tmp_epub);
            result??;

            app.emit(
                "convert:progress",
                ProgressPayload {
                    path: path.to_string(),
                    percent: 100.0,
                },
            )
            .ok();
            Ok(out.to_string_lossy().to_string())
        }
        _ => Err(format!("Unsupported MOBI target format: {target_format}")),
    }
}

pub async fn convert_epub_to_mobi(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let out = output_path(path, "mobi", output_dir)?;
    run_ebook_convert(app, path, &out).await?;
    Ok(out.to_string_lossy().to_string())
}

pub async fn convert_pdf_to_mobi(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    use_marker: bool,
    llm: LlmCfg,
) -> Result<String, String> {
    let out = output_path(path, "mobi", output_dir)?;

    if use_marker && marker_available() {
        let tmp_epub = std::env::temp_dir().join(format!(
            "swift_shifter_marker_mobi_{}.epub",
            unique_tmp_suffix()
        ));
        let tmp_epub_dir = tmp_epub.parent().map(|p| p.to_string_lossy().to_string());
        let epub_path = convert_pdf_with_marker(app, path, tmp_epub_dir.as_deref(), llm).await?;
        if std::path::Path::new(&epub_path) != tmp_epub {
            let _ = std::fs::rename(&epub_path, &tmp_epub);
        }
        run_ebook_convert(app, tmp_epub.to_str().unwrap_or(path), &out).await?;
        let _ = std::fs::remove_file(&tmp_epub);
    } else {
        run_ebook_convert(app, path, &out).await?;
    }

    Ok(out.to_string_lossy().to_string())
}

pub async fn convert_pdf_to_html(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let python = get_pymupdf4llm_python()?;
    let out = output_path(path, "html", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_dir = std::env::temp_dir().join(format!("swift_shifter_html_{}", unique_tmp_suffix()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_md = tmp_dir.join("output.md");

    let tmp_md_str = tmp_md
        .to_str()
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())?;

    // Step 1: PDF → Markdown via pymupdf4llm
    let result = crate::process::async_command(&python)
        .args([
            "-c",
            "import pymupdf4llm, sys; open(sys.argv[2], 'w', encoding='utf-8').write(pymupdf4llm.to_markdown(sys.argv[1]))",
            path,
            tmp_md_str,
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

    if !tmp_md.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("pymupdf4llm succeeded but produced no output file".to_string());
    }

    // Clean up the temp dir before propagating a read error.
    let markdown = tokio::fs::read_to_string(&tmp_md)
        .await
        .map_err(|e| format!("Failed to read extracted markdown: {e}"));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    let markdown = markdown?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 50.0,
        },
    )
    .ok();

    // Step 2: Markdown → standalone HTML with the native writer. It reads the
    // <br> tags pymupdf4llm puts in table cells as line breaks.
    let title = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    tokio::fs::write(&out, super::native::markdown_to_html(&markdown, &title))
        .await
        .map_err(|e| format!("Failed to write {}: {e}", out.display()))?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

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
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_dir =
        std::env::temp_dir().join(format!("swift_shifter_pdf_md_{}", unique_tmp_suffix()));
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let tmp_md = tmp_dir.join("output.md");

    let tmp_md_str = tmp_md
        .to_str()
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())?;

    let result = crate::process::async_command(&python)
        .args([
            "-c",
            "import pymupdf4llm, sys; open(sys.argv[2], 'w', encoding='utf-8').write(pymupdf4llm.to_markdown(sys.argv[1]))",
            path,
            tmp_md_str,
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

    if !tmp_md.exists() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("pymupdf4llm succeeded but produced no output file".to_string());
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 50.0,
        },
    )
    .ok();

    if llm.enabled
        && let Ok(content) = tokio::fs::read_to_string(&tmp_md).await
    {
        let processed = llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
        let _ = tokio::fs::write(&tmp_md, processed).await;
    }

    let copy_result = std::fs::copy(&tmp_md, &out);
    let _ = std::fs::remove_dir_all(&tmp_dir);
    copy_result.map_err(|e| format!("Failed to copy output: {e}"))?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

pub async fn convert_pdf_to_md(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    use_marker: bool,
    llm: LlmCfg,
) -> Result<String, String> {
    if use_marker && marker_available() {
        return convert_pdf_with_marker_to_md(app, path, output_dir, llm).await;
    }
    convert_pdf_to_md_via_pymupdf4llm(app, path, output_dir, llm).await
}

pub(crate) async fn convert_pdf_with_marker_to_md(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
    let marker = find_marker_binary()
        .ok_or_else(|| "marker not found — install with: pip install marker-pdf".to_string())?;
    let out = output_path(path, "md", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_base =
        std::env::temp_dir().join(format!("swift_shifter_marker_md_{}", unique_tmp_suffix()));
    let input_dir = tmp_base.join("input");
    let output_dir_tmp = tmp_base.join("output");

    std::fs::create_dir_all(&input_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;
    std::fs::create_dir_all(&output_dir_tmp)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let tmp_pdf = input_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf).map_err(|e| format!("Failed to copy PDF: {e}"))?;

    let marker_name = marker
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut cmd = crate::process::async_command(&marker);
    if marker_name.contains("marker_single") {
        cmd.args([
            tmp_pdf.to_str().unwrap_or(""),
            output_dir_tmp.to_str().unwrap_or(""),
        ]);
    } else {
        cmd.args([
            input_dir.to_str().unwrap_or(""),
            "--output_dir",
            output_dir_tmp.to_str().unwrap_or(""),
        ]);
    }
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let status = cmd
        .status()
        .await
        .map_err(|e| format!("Failed to spawn marker: {e}"))?;

    if !status.success() || find_md_file(&output_dir_tmp).is_none() {
        let _ = std::fs::remove_dir_all(&tmp_base);
        return convert_pdf_to_md_via_pymupdf4llm(app, path, output_dir, llm).await;
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 90.0,
        },
    )
    .ok();

    let md_file = find_md_file(&output_dir_tmp)
        .ok_or_else(|| "marker produced no Markdown file".to_string())?;

    if llm.enabled
        && let Ok(content) = tokio::fs::read_to_string(&md_file).await
    {
        let processed = llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
        let _ = tokio::fs::write(&md_file, processed).await;
    }

    std::fs::copy(&md_file, &out).map_err(|e| format!("Failed to copy marker output: {e}"))?;

    let out_dir = out.parent().unwrap_or_else(|| std::path::Path::new("."));
    if let Some(md_parent) = md_file.parent() {
        copy_dir_contents_except(md_parent, out_dir, &md_file);
    }

    let _ = std::fs::remove_dir_all(&tmp_base);

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 100.0,
        },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf_text(pdf: &[u8]) -> Option<String> {
        let dir = tempfile::tempdir().ok()?;
        let path = dir.path().join("out.pdf");
        std::fs::write(&path, pdf).ok()?;
        let output = std::process::Command::new("pdftotext")
            .arg(&path)
            .arg("-")
            .output()
            .ok()?;
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    #[test]
    fn non_native_images_are_converted_before_typesetting() {
        let dir = tempfile::tempdir().unwrap();
        let bmp = dir.path().join("scan.bmp");
        image::RgbImage::new(30, 20).save(&bmp).unwrap();
        let pdf = image_pdf_bytes(&bmp.to_string_lossy()).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }

    #[test]
    fn tex_typesets_via_pandoc_with_its_own_files_and_metadata() {
        let Some(pandoc) = find_pandoc_binary() else {
            eprintln!("skipping: pandoc not found");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        image::RgbImage::new(8, 8)
            .save(dir.path().join("fig.png"))
            .unwrap();
        std::fs::write(dir.path().join("part.tex"), "Included part text.\n").unwrap();
        let tex = dir.path().join("paper.tex");
        std::fs::write(
            &tex,
            "\\documentclass{article}\n\\title{Real Title}\n\\author{Ada \\and Grace}\n\
             \\begin{document}\n\\maketitle\n\\section{Intro}\\label{intro}\n\
             See \\hyperref[nowhere]{a broken link}.\n\\input{part}\n\
             \\includegraphics{fig.png}\n\\end{document}\n",
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let meta = runtime.block_on(pandoc_metadata(&pandoc, &tex, "latex", dir.path()));
        assert_eq!(meta.title.as_deref(), Some("Real Title"));
        assert_eq!(meta.authors, ["Ada", "Grace"]);
        let pdf = runtime
            .block_on(pandoc_typst_pdf(&pandoc, &tex, "latex", "paper"))
            .unwrap();
        // Pandoc swaps an image it can't find for its description, so check
        // that the figure really was embedded.
        let doc = lopdf::Document::load_mem(&pdf).unwrap();
        let images = doc
            .objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .filter(|s| {
                s.dict.get(b"Subtype").and_then(|t| t.as_name()).ok() == Some(b"Image".as_slice())
            })
            .count();
        assert_eq!(images, 1);
        if let Some(text) = pdf_text(&pdf) {
            assert!(text.contains("Real Title"), "{text}");
            assert!(text.contains("Included part text."), "{text}");
        }
    }
}
