use std::path::{Path, PathBuf};
use tauri::Emitter;
use crate::converter::document::types::*;
use crate::converter::document::utils::*;
use crate::converter::document::binaries::*;
use crate::converter::document::llm::*;

const EPUB_CSS: &str = concat!(
    "code, pre, kbd, samp {\n",
    "  white-space: pre;\n",
    "  font-family: monospace;\n",
    "}\n",
    "pre {\n",
    "  overflow-x: auto;\n",
    "  padding: 0.5em;\n",
    "}\n",
    ".math, .MathML, math {\n",
    "  white-space: pre;\n",
    "}\n",
);

/// Replace `<br>` variants with a space so pandoc produces valid XHTML (needed for EPUB)
/// and browsers don't encounter unclosed void elements in HTML output.
fn sanitize_br_tags(content: &str) -> String {
    content
        .replace("<br/>", " ")
        .replace("<br />", " ")
        .replace("<BR/>", " ")
        .replace("<BR />", " ")
        .replace("<br>", " ")
        .replace("<BR>", " ")
}

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
///   2. `pandoc *.md -t epub` — packages them into EPUB
pub async fn convert_pdf_with_marker(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
    let marker = find_marker_binary()
        .ok_or_else(|| "marker not found — install with: pip install marker-pdf".to_string())?;
    let pandoc = get_pandoc()?;

    let user_output_dir = output_dir;
    let out = output_path(path, "epub", user_output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    ).ok();

    let tmp_base = std::env::temp_dir().join(format!("swift_shifter_marker_{}", unique_tmp_suffix()));
    let input_dir = tmp_base.join("input");
    let output_dir = tmp_base.join("output");

    std::fs::create_dir_all(&input_dir)
        .map_err(|e| format!("Failed to create input temp directory: {e}"))?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output temp directory: {e}"))?;

    let tmp_pdf = input_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF: {e}"))?;

    let marker_name = marker.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut cmd = tokio::process::Command::new(&marker);
    if marker_name.contains("marker_single") {
        cmd.args([tmp_pdf.to_str().unwrap_or(""), output_dir.to_str().unwrap_or("")]);
    } else {
        cmd.args([
            input_dir.to_str().unwrap_or(""),
            "--output_dir",
            output_dir.to_str().unwrap_or(""),
        ]);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to spawn marker: {e}"))?;

    let stdout = child.stdout.take()
        .ok_or_else(|| "marker: stdout not piped".to_string())?;
    let stderr = child.stderr.take()
        .ok_or_else(|| "marker: stderr not piped".to_string())?;
    let app_handle = app.clone();
    let path_str = path.to_string();

    let progress_task = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut reader = BufReader::new(stdout).lines();
        let mut err_reader = BufReader::new(stderr).lines();

        let stages: &[(&str, f32)] = &[
            ("load model",          5.0),
            ("detection model",     8.0),
            ("texify",             12.0),
            ("recognition model",  12.0),
            ("surya",              14.0),
            ("reading pdf",        18.0),
            ("pdf loaded",         20.0),
            ("running layout",     28.0),
            ("layout detection",   28.0),
            ("running ocr",        38.0),
            ("text extraction",    40.0),
            ("running line",       44.0),
            ("post-processing",    60.0),
            ("ordering blocks",    65.0),
            ("merging lines",      68.0),
            ("cleaning text",      72.0),
            ("formatting",         75.0),
            ("saving output",      88.0),
            ("writing markdown",   88.0),
            ("saved to",           90.0),
        ];

        let mut current_pct: f32 = 2.0;
        let mut stdout_done = false;
        let mut stderr_done = false;

        app_handle.emit("convert:progress", ProgressPayload { path: path_str.clone(), percent: current_pct }).ok();

        fn advance_from_line(line: &str, stages: &[(&str, f32)], current: f32) -> f32 {
            let lower = line.to_lowercase();
            let mut best = current;
            for (kw, pct) in stages {
                if *pct > current && lower.contains(kw) {
                    if *pct > best { best = *pct; }
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
            if stdout_done && stderr_done { break; }

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

    let status = child.wait().await
        .map_err(|e| format!("marker wait error: {e}"))?;
    let _ = progress_task.await;

    if !status.success() || find_md_file(&output_dir).is_none() {
        let _ = std::fs::remove_dir_all(&tmp_base);
        return convert_pdf_to_epub(app, path, user_output_dir, llm).await;
    }

    let md_file = find_md_file(&output_dir)
        .ok_or_else(|| "marker produced no Markdown file".to_string())?;

    if let Ok(mut content) = tokio::fs::read_to_string(&md_file).await {
        let mut changed = false;
        let tags = ["sup", "sub"];
        for tag in tags {
            let open = format!("<{}>", tag);
            let close = format!("</{}>", tag);
            let open_count = content.matches(&open).count();
            let close_count = content.matches(&close).count();
            
            if open_count > close_count {
                for _ in 0..(open_count - close_count) {
                    content.push_str(&close);
                }
                changed = true;
            }
        }

        if llm.enabled {
            content = llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
            changed = true;
        }

        if changed {
            let _ = tokio::fs::write(&md_file, content).await;
        }
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 97.0 },
    ).ok();

    let md_dir = md_file.parent().unwrap_or(&output_dir);

    let file_title = Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let epub_css_path: Option<String> = {
        let p = md_dir.join("_swift_shifter_epub.css");
        if std::fs::write(&p, EPUB_CSS).is_ok() {
            p.to_str().map(|s| s.to_string())
        } else {
            None
        }
    };

    let mut pandoc_cmd = tokio::process::Command::new(&pandoc);
    pandoc_cmd.current_dir(md_dir);
    pandoc_cmd.args([
        "-f", "markdown+footnotes+superscript+subscript+tex_math_dollars+tex_math_single_backslash",
        "-t", "epub3",
        "--mathml",
    ]);
    if let Some(ref css_str) = epub_css_path {
        pandoc_cmd.args(["--css", css_str]);
    }
    pandoc_cmd.args([
        "--metadata", &format!("title={}", file_title),
        "-o", out.to_str().unwrap_or(""),
        md_file.file_name().unwrap_or_default().to_str().unwrap_or(""),
    ]);
    pandoc_cmd.stdout(std::process::Stdio::null());
    pandoc_cmd.stderr(std::process::Stdio::piped());

    let mut child = pandoc_cmd.spawn()
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    let stderr_out = if let Some(stderr) = child.stderr.take() {
        use tokio::io::{AsyncReadExt, BufReader};
        let mut buf = String::new();
        BufReader::new(stderr).read_to_string(&mut buf).await.ok();
        buf
    } else {
        String::new()
    };

    let status = child.wait().await
        .map_err(|e| format!("pandoc wait error: {e}"))?;

    let _ = std::fs::remove_dir_all(&tmp_base);

    if !status.success() {
        return Err(if stderr_out.trim().is_empty() {
            format!("pandoc exited with code {}", status.code().unwrap_or(-1))
        } else {
            stderr_out.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    ).ok();

    Ok(out.to_string_lossy().to_string())
}

pub async fn convert_document(
    app: &tauri::AppHandle,
    path: &str,
    target_format: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let pandoc = get_pandoc()?;

    let input_ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let out = output_path(path, target_to_ext(target_format), output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let from_fmt = ext_to_pandoc_format(&input_ext);
    let to_fmt = ext_to_pandoc_format(target_format);

    let mut cmd = tokio::process::Command::new(&pandoc);
    cmd.args([
        "-f",
        from_fmt,
        "-t",
        to_fmt,
        "-o",
        out.to_str().unwrap_or(""),
    ]);

    if target_format == "pdf" {
        if let Some(engine) = detect_pdf_engine() {
            cmd.args(["--pdf-engine", engine]);
        }
    }

    cmd.arg(path);
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

pub async fn convert_image_to_pdf(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let pandoc = get_pandoc()?;

    let input = Path::new(path);
    let filename = input
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let image_dir = input
        .parent()
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .to_string();

    let out = output_path(path, "pdf", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_md = std::env::temp_dir().join(format!("swift_shifter_{}.md", unique_tmp_suffix()));
    std::fs::write(&tmp_md, format!("![]({})", filename))
        .map_err(|e| format!("Failed to create temp file: {e}"))?;

    let mut cmd = tokio::process::Command::new(&pandoc);
    cmd.args([
        "-f",
        "markdown",
        "-t",
        "pdf",
        "--resource-path",
        &image_dir,
        "-o",
        out.to_str().unwrap_or(""),
    ]);

    if let Some(engine) = detect_pdf_engine() {
        cmd.args(["--pdf-engine", engine]);
    }

    cmd.arg(tmp_md.to_str().unwrap_or(""));
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

    let _ = std::fs::remove_file(&tmp_md);

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

    let tmp_md_str = tmp_md
        .to_str()
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())?;

    // Step 1: PDF → Markdown via pymupdf4llm
    let result = tokio::process::Command::new(&python)
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
        ProgressPayload { path: path.to_string(), percent: 40.0 },
    )
    .ok();

    // Sanitize <br> tags — EPUB content is XHTML, bare <br> is invalid XML
    if let Ok(content) = tokio::fs::read_to_string(&tmp_md).await {
        let _ = tokio::fs::write(&tmp_md, sanitize_br_tags(&content)).await;
    }

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
            tmp_md_str,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            format!("Failed to spawn pandoc: {e}")
        })?;

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

async fn run_ebook_convert(
    app: &tauri::AppHandle,
    input: &str,
    output: &PathBuf,
) -> Result<(), String> {
    let ec = get_ebook_convert()?;
    app.emit(
        "convert:progress",
        ProgressPayload { path: input.to_string(), percent: 0.0 },
    )
    .ok();

    let result = tokio::process::Command::new(&ec)
        .args([
            input,
            output.to_str().unwrap_or(""),
            "--filter-css", "background-color,background,color",
            "--extra-css", "body, html { background-color: white !important; color: black !important; }",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn ebook-convert: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("ebook-convert exited with code {}", result.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: input.to_string(), percent: 100.0 },
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
            let pandoc = get_pandoc()?;
            let out = output_path(path, "md", output_dir)?;
            let tmp_epub =
                std::env::temp_dir().join(format!("swift_shifter_mobi_{}.epub", unique_tmp_suffix()));
            run_ebook_convert(app, path, &tmp_epub).await?;

            app.emit(
                "convert:progress",
                ProgressPayload { path: path.to_string(), percent: 50.0 },
            )
            .ok();

            let status = tokio::process::Command::new(&pandoc)
                .args([
                    "-f", "epub",
                    "-t", "markdown",
                    "-o", out.to_str().unwrap_or(""),
                    tmp_epub.to_str().unwrap_or(""),
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

            let _ = std::fs::remove_file(&tmp_epub);

            if !status.success() {
                return Err(format!("pandoc exited with code {}", status.code().unwrap_or(-1)));
            }

            app.emit(
                "convert:progress",
                ProgressPayload { path: path.to_string(), percent: 100.0 },
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
        let epub_path = convert_pdf_with_marker(
            app,
            path,
            tmp_epub_dir.as_deref(),
            llm,
        ).await?;
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

    let tmp_md_str = tmp_md
        .to_str()
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())?;

    // Step 1: PDF → Markdown via pymupdf4llm
    let result = tokio::process::Command::new(&python)
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

    // Sanitize <br> tags before pandoc — HTML output uses --standalone but EPUB is strict XHTML
    if let Ok(content) = tokio::fs::read_to_string(&tmp_md).await {
        let _ = tokio::fs::write(&tmp_md, sanitize_br_tags(&content)).await;
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 50.0 },
    )
    .ok();

    // Step 2: Markdown → HTML via pandoc
    // --standalone adds <!DOCTYPE html> so browsers use HTML5 parsing (fixes <br> in tables)
    let pandoc_result = tokio::process::Command::new(&pandoc)
        .args([
            "-f",
            "markdown",
            "-t",
            "html",
            "--standalone",
            "-o",
            out.to_str().unwrap_or(""),
            tmp_md_str,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            format!("Failed to spawn pandoc: {e}")
        })?;

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

    let tmp_md_str = tmp_md
        .to_str()
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())?;

    let result = tokio::process::Command::new(&python)
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

    let copy_result = std::fs::copy(&tmp_md, &out);
    let _ = std::fs::remove_dir_all(&tmp_dir);
    copy_result.map_err(|e| format!("Failed to copy output: {e}"))?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
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
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let tmp_base = std::env::temp_dir().join(format!("swift_shifter_marker_md_{}", unique_tmp_suffix()));
    let input_dir = tmp_base.join("input");
    let output_dir_tmp = tmp_base.join("output");

    std::fs::create_dir_all(&input_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    std::fs::create_dir_all(&output_dir_tmp)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let tmp_pdf = input_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF: {e}"))?;

    let marker_name = marker.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut cmd = tokio::process::Command::new(&marker);
    if marker_name.contains("marker_single") {
        cmd.args([tmp_pdf.to_str().unwrap_or(""), output_dir_tmp.to_str().unwrap_or("")]);
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
        ProgressPayload { path: path.to_string(), percent: 90.0 },
    )
    .ok();

    let md_file = find_md_file(&output_dir_tmp)
        .ok_or_else(|| "marker produced no Markdown file".to_string())?;

    if llm.enabled {
        if let Ok(content) = tokio::fs::read_to_string(&md_file).await {
            let processed = llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
            let _ = tokio::fs::write(&md_file, processed).await;
        }
    }

    std::fs::copy(&md_file, &out)
        .map_err(|e| format!("Failed to copy marker output: {e}"))?;

    let out_dir = out.parent().unwrap_or_else(|| std::path::Path::new("."));
    if let Some(md_parent) = md_file.parent() {
        copy_dir_contents_except(md_parent, out_dir, &md_file);
    }

    let _ = std::fs::remove_dir_all(&tmp_base);

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 100.0 },
    )
    .ok();
    Ok(out.to_string_lossy().to_string())
}
