use crate::converter::document::binaries::*;
use crate::converter::document::types::*;
use crate::converter::document::utils::*;
use std::path::Path;
use tauri::Emitter;

/// A new folder for a `kind` of conversion's working files, its own however
/// many others run, in this process or another (#190), removed when dropped.
fn work_dir(kind: &str) -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix(&format!("swift_shifter_{kind}_"))
        .tempdir()
        .map_err(|e| format!("Failed to create temp dir: {e}"))
}

/// Convert a PDF to EPUB using the marker-pdf ML pipeline:
///   1. `marker input.pdf --output_dir tmp` — produces Markdown + images
///   2. the native EPUB writer packages them
pub async fn convert_pdf_with_marker(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let marker = get_marker()?;

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

    let tmp_base = work_dir("marker")?;
    let input_dir = tmp_base.path().join("input");
    let output_dir = tmp_base.path().join("output");

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
        drop(tmp_base);
        return convert_pdf_to_epub(app, path, user_output_dir).await;
    }

    let md_file =
        find_md_file(&output_dir).ok_or_else(|| "marker produced no Markdown file".to_string())?;

    let content = tokio::fs::read_to_string(&md_file)
        .await
        .map_err(|e| format!("Failed to read marker's output: {e}"))?;

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
    .map_err(|e| format!("task panicked: {e}"))??;
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

    let tmp_dir = work_dir("epub")?;
    let tmp_md = tmp_dir.path().join("output.md");

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

    let content = tokio::fs::read_to_string(&tmp_md)
        .await
        .map_err(|e| format!("Failed to read pymupdf4llm's output: {e}"))?;

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
    let base_dir = tmp_dir.path().to_path_buf();
    let epub = tokio::task::spawn_blocking(move || {
        super::native::markdown_to_epub(&content, &title, &base_dir)
    })
    .await
    .map_err(|e| format!("task panicked: {e}"))??;
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

    let tmp_dir = work_dir("html")?;
    let tmp_md = tmp_dir.path().join("output.md");

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
        return Err("pymupdf4llm succeeded but produced no output file".to_string());
    }

    let markdown = tokio::fs::read_to_string(&tmp_md)
        .await
        .map_err(|e| format!("Failed to read extracted markdown: {e}"))?;

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

    let tmp_dir = work_dir("pdf_md")?;
    let tmp_md = tmp_dir.path().join("output.md");

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

    std::fs::copy(&tmp_md, &out).map_err(|e| format!("Failed to copy output: {e}"))?;

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
) -> Result<String, String> {
    if use_marker && marker_available() {
        return convert_pdf_with_marker_to_md(app, path, output_dir).await;
    }
    convert_pdf_to_md_via_pymupdf4llm(app, path, output_dir).await
}

pub(crate) async fn convert_pdf_with_marker_to_md(
    app: &tauri::AppHandle,
    path: &str,
    output_dir: Option<&str>,
) -> Result<String, String> {
    let marker = get_marker()?;
    let out = output_path(path, "md", output_dir)?;

    app.emit(
        "convert:progress",
        ProgressPayload {
            path: path.to_string(),
            percent: 0.0,
        },
    )
    .ok();

    let tmp_base = work_dir("marker_md")?;
    let input_dir = tmp_base.path().join("input");
    let output_dir_tmp = tmp_base.path().join("output");

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
        drop(tmp_base);
        return convert_pdf_to_md_via_pymupdf4llm(app, path, output_dir).await;
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

    std::fs::copy(&md_file, &out).map_err(|e| format!("Failed to copy marker output: {e}"))?;

    let out_dir = out.parent().unwrap_or_else(|| std::path::Path::new("."));
    if let Some(md_parent) = md_file.parent() {
        copy_dir_contents_except(md_parent, out_dir, &md_file);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Regression for #190: names made of the time and a thread's id
    /// collided, and one conversion removed another's output.
    #[test]
    fn work_dirs_started_together_are_each_their_own_until_dropped() {
        const CONVERSIONS: usize = 16;
        let start = std::sync::Barrier::new(CONVERSIONS);
        let dirs: Vec<_> = std::thread::scope(|scope| {
            let started: Vec<_> = (0..CONVERSIONS)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        work_dir("epub").unwrap()
                    })
                })
                .collect();
            started.into_iter().map(|t| t.join().unwrap()).collect()
        });
        let paths: std::collections::HashSet<PathBuf> =
            dirs.iter().map(|dir| dir.path().to_path_buf()).collect();
        assert_eq!(paths.len(), CONVERSIONS);
        assert!(paths.iter().all(|path| path.is_dir()));
        drop(dirs);
        assert!(paths.iter().all(|path| !path.exists()));
    }

    #[test]
    fn non_native_images_are_converted_before_typesetting() {
        let dir = tempfile::tempdir().unwrap();
        let bmp = dir.path().join("scan.bmp");
        image::RgbImage::new(30, 20).save(&bmp).unwrap();
        let pdf = image_pdf_bytes(&bmp.to_string_lossy()).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
    }
}
