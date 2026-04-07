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
    _llm: LlmCfg,
) -> Result<String, String> {
    let pdftohtml = get_pdftohtml()?;
    let pandoc = get_pandoc()?;

    let out = output_path(path, "epub", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_dir = std::env::temp_dir().join(format!("swift_shifter_{}", unique_tmp_suffix()));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp directory: {e}"))?;

    let tmp_pdf = tmp_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF to temp dir: {e}"))?;

    let tmp_html = tmp_dir.join("doc.html");

    let pdftohtml_out = tokio::process::Command::new(&pdftohtml)
        .current_dir(&tmp_dir)
        .args(["-noframes", "-nodrm", "-fmt", "png", "input.pdf", "doc"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pdftohtml: {e}"))?;

    if !pdftohtml_out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&pdftohtml_out.stderr);
        let msg = if stderr.trim().is_empty() {
            format!(
                "pdftohtml exited with code {}",
                pdftohtml_out.status.code().unwrap_or(-1)
            )
        } else {
            stderr.trim().to_string()
        };
        return Err(msg);
    }

    if let Ok(mut html_content) = tokio::fs::read_to_string(&tmp_html).await {
        let re_style_block = RE_STYLE_BLOCK.get_or_init(|| {
            regex::Regex::new(r"(?s)<style\b[^>]*>.*?</style>").expect("valid static regex")
        });
        html_content = re_style_block.replace_all(&html_content, "").to_string();

        let re_pos = RE_CSS_POS.get_or_init(|| {
            regex::Regex::new(r"position\s*:\s*(?:absolute|relative)\s*;?\s*").expect("valid static regex")
        });
        html_content = re_pos.replace_all(&html_content, "").to_string();

        let re_top = RE_CSS_TOP.get_or_init(|| {
            regex::Regex::new(r"top\s*:\s*-?\d+(?:\.\d+)?px\s*;?\s*").expect("valid static regex")
        });
        html_content = re_top.replace_all(&html_content, "").to_string();

        let re_left = RE_CSS_LEFT.get_or_init(|| {
            regex::Regex::new(r"left\s*:\s*-?\d+(?:\.\d+)?px\s*;?\s*").expect("valid static regex")
        });
        html_content = re_left.replace_all(&html_content, "").to_string();

        let re_height = RE_CSS_HEIGHT.get_or_init(|| {
            regex::Regex::new(r"height\s*:\s*\d+(?:\.\d+)?px\s*;?\s*").expect("valid static regex")
        });
        html_content = re_height.replace_all(&html_content, "").to_string();

        let re_num = RE_NUM.get_or_init(|| {
            regex::Regex::new(r"(?m)^\s*(?:<a name=\d+></a>)?\s*\d+\s*<br/>\s*\r?\n?").expect("valid static regex")
        });
        html_content = re_num.replace_all(&html_content, "").to_string();

        let re_merge = RE_MERGE.get_or_init(|| {
            regex::Regex::new(r"([a-zA-Z,\-]|&#160;)\s*<br/>\s*\r?\n?\s*(?:&#160;)*([a-z])")
                .expect("valid static regex")
        });
        html_content = re_merge.replace_all(&html_content, "${1} ${2}").to_string();

        let re_svg = RE_SVG_SRC.get_or_init(|| {
            regex::Regex::new(r#"(?i)src="([^"]+\.svg)""#).expect("valid static regex")
        });
        let svg_names: Vec<String> = re_svg
            .captures_iter(&html_content)
            .map(|c| c[1].to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for svg_name in svg_names {
            let svg_path = tmp_dir.join(&svg_name);
            if let Some(png_path) = convert_svg_to_png(&svg_path).await {
                let png_name = png_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                html_content = html_content
                    .replace(&format!(r#"src="{}""#, svg_name), &format!(r#"src="{}""#, png_name));
            } else {
                if let Ok(re_img) = regex::Regex::new(&format!(
                    r#"(?i)<img\b[^>]*\bsrc="{}"[^>]*/?>[ \t]*"#,
                    regex::escape(&svg_name)
                )) {
                    html_content = re_img.replace_all(&html_content, "").to_string();
                }
            }
        }

        let _ = tokio::fs::write(&tmp_html, html_content).await;
    }

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 50.0,
        },
    )
    .ok();

    let file_title = Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let mut cmd = tokio::process::Command::new(&pandoc);
    cmd.current_dir(&tmp_dir);
    cmd.args([
        "-f",
        "html",
        "-t",
        "epub",
        "--metadata",
        &format!("title={}", file_title),
        "-o",
        out.to_str().unwrap_or(""),
        "doc.html",
    ]);
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

    let _ = std::fs::remove_dir_all(&tmp_dir);

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
    let pdftohtml = get_pdftohtml()?;
    let out = output_path(path, "html", output_dir)?;
    let out_stem = out.with_extension("");

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let result = tokio::process::Command::new(&pdftohtml)
        .args(["-noframes", "-nodrm", "-fmt", "png", path, out_stem.to_str().unwrap_or("")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pdftohtml: {e}"))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("pdftohtml exited with code {}", result.status.code().unwrap_or(-1))
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

async fn convert_pdf_to_md_via_pdftohtml(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
    llm: LlmCfg,
) -> Result<String, String> {
    let pdftohtml = get_pdftohtml()?;
    let pandoc = get_pandoc()?;
    let out = output_path(path, "md", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 0.0 },
    )
    .ok();

    let tmp_dir = std::env::temp_dir().join(format!("swift_shifter_pdf_md_{}", unique_tmp_suffix()));
    std::fs::create_dir_all(&tmp_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let tmp_pdf = tmp_dir.join("input.pdf");
    std::fs::copy(path, &tmp_pdf)
        .map_err(|e| format!("Failed to copy PDF: {e}"))?;

    let html_result = tokio::process::Command::new(&pdftohtml)
        .current_dir(&tmp_dir)
        .args(["-noframes", "-nodrm", "-fmt", "png", "input.pdf", "doc"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pdftohtml: {e}"))?;

    if !html_result.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&html_result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("pdftohtml exited with code {}", html_result.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        });
    }

    app.emit(
        "convert:progress",
        ProgressPayload { path: path.to_string(), percent: 50.0 },
    )
    .ok();

    let tmp_md = tmp_dir.join("doc.md");
    let md_result = tokio::process::Command::new(&pandoc)
        .current_dir(&tmp_dir)
        .args(["-f", "html", "-t", "markdown", "-o", "doc.md", "doc.html"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("Failed to spawn pandoc: {e}"))?;

    if !md_result.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        let stderr = String::from_utf8_lossy(&md_result.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("pandoc exited with code {}", md_result.status.code().unwrap_or(-1))
        } else {
            stderr.trim().to_string()
        });
    }

    if llm.enabled {
        if let Ok(content) = tokio::fs::read_to_string(&tmp_md).await {
            let processed = llm_postprocess_markdown(app, content, path, &llm.url, &llm.model).await;
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
    convert_pdf_to_md_via_pdftohtml(app, path, output_dir, llm).await
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
        return convert_pdf_to_md_via_pdftohtml(app, path, output_dir, llm).await;
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
