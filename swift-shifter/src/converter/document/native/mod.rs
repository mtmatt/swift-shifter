//! Swift Shifter's own document converter, replacing pandoc one hop at a time
//! (see docs/superpowers/specs/2026-10-15-native-document-converter-design.md).
//! Readers build an [`ast::Document`]; writers render it.

pub mod ast;
pub mod read;
pub mod typst;
pub mod write;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tauri::Emitter;

use crate::converter::document::ProgressPayload;
use crate::converter::document::utils::{output_path, target_to_ext};
use ast::{Block, Document, Inline, List};

/// Input formats the native readers handle (normalized extensions).
pub const INPUTS: &[&str] = &["md", "txt", "html", "epub"];
/// Output formats the native writers handle.
pub const OUTPUTS: &[&str] = &["md", "txt", "html", "pdf", "typst", "epub", "tex"];

/// Whether the native engine converts `from` → `to` (normalized extensions).
pub fn handles(from: &str, to: &str) -> bool {
    INPUTS.contains(&from) && OUTPUTS.contains(&to) && from != to
}

/// Convert `path` to `target` (one of [`OUTPUTS`]), writing next to the input or
/// into `output_dir`. Emits the same progress events as the other converters.
pub async fn convert_document(
    app: &tauri::AppHandle,
    path: &str,
    target: &str,
    output_dir: Option<&str>,
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
    // Typst output is written as `.typ`, like the pandoc path.
    let out = output_path(path, target_to_ext(target), output_dir)?;
    let input = PathBuf::from(path);
    let out_for_task = out.clone();
    let target = target.to_string();
    tokio::task::spawn_blocking(move || convert(&input, &target, &out_for_task))
        .await
        .map_err(|e| format!("task panicked: {e}"))??;
    progress(100.0);
    Ok(out.to_string_lossy().into_owned())
}

/// Synchronous core of [`convert_document`].
pub fn convert(input: &Path, target: &str, out: &Path) -> Result<(), String> {
    let from = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| crate::converter::graph::normalize_ext(&e.to_lowercase()).to_string())
        .unwrap_or_default();

    // An EPUB is extracted to a temp folder its images are read from; it
    // must outlive the writing.
    let mut book_dir = None;
    let mut doc = match from.as_str() {
        "epub" => {
            let book = read::epub::read(input)?;
            book_dir = Some(book.dir);
            book.doc
        }
        "md" | "txt" | "html" => {
            let source = std::fs::read(input)
                .map_err(|e| format!("Failed to read {}: {e}", input.display()))?;
            // Lossy rather than failing on a stray invalid byte; a BOM is dropped.
            let source = String::from_utf8_lossy(&source);
            let source = source.trim_start_matches('\u{feff}');
            match from.as_str() {
                "md" => read::markdown::read(source),
                "txt" => read::plain::read(source),
                _ => read::html::read(source),
            }
        }
        other => return Err(format!("The native converter can't read .{other}")),
    };
    if matches!(target, "html" | "epub") {
        fill_title(&mut doc, input);
    }
    let base_dir = match &book_dir {
        Some(dir) => dir.path().to_path_buf(),
        None => input.parent().unwrap_or(Path::new(".")).to_path_buf(),
    };
    let elsewhere = out
        .parent()
        .is_some_and(|out_dir| !same_dir(&base_dir, out_dir));
    if target == "tex" {
        copy_images_for_latex(
            &mut doc.blocks,
            &base_dir,
            out,
            elsewhere && book_dir.is_none(),
        )?;
    } else if book_dir.is_some() {
        // The book's images vanish with its temp folder: keep copies.
        if matches!(target, "md" | "html" | "typst") {
            copy_images_beside(&mut doc.blocks, &base_dir, out)?;
        }
    } else if let Some(out_dir) = out.parent()
        && !same_dir(&base_dir, out_dir)
    {
        match target {
            "typst" => copy_images_beside(&mut doc.blocks, &base_dir, out)?,
            // Images are packaged into the book from `base_dir`, and
            // `file://` links would only work on this computer.
            "epub" => {}
            _ => rebase_relative_urls(&mut doc.blocks, &base_dir),
        }
    }

    let rendered = match target {
        "md" => write::markdown::write(&doc).into_bytes(),
        "txt" => write::plain::write(&doc).into_bytes(),
        "html" => write::html::write(&doc).into_bytes(),
        "typst" => typst_file_source(&doc, &base_dir).into_bytes(),
        "tex" => write::latex::write(&doc).into_bytes(),
        "pdf" => typeset(&doc, &base_dir)?,
        "epub" => write::epub::write(
            &doc,
            &write::epub::Options {
                base_dir: base_dir.clone(),
            },
        )?,
        other => return Err(format!("The native converter can't write {other}")),
    };
    std::fs::write(out, rendered).map_err(|e| format!("Failed to write {}: {e}", out.display()))
}

/// A standalone `.typ` file for the user's own typst.
fn typst_file_source(doc: &Document, base_dir: &Path) -> String {
    let lang = doc.effective_lang();
    let fonts = typst::text_fonts(lang.as_deref());
    let options = write::typst::Options {
        target: write::typst::Target::TypstFile,
        fonts,
        paper: typst::default_paper(),
        base_dir: base_dir.to_path_buf(),
        tex_fallback: &HashSet::new(),
    };
    write::typst::write(doc, &options).source
}

/// Typeset `doc` to PDF. Formulas the compiler rejects are shown as TeX
/// instead and the document is typeset again, so one odd formula doesn't
/// fail the whole conversion.
pub fn typeset(doc: &Document, base_dir: &Path) -> Result<Vec<u8>, String> {
    typeset_with_fallback(doc, base_dir).map(|(pdf, _)| pdf)
}

/// [`typeset`], also returning the indices of the formulas shown as TeX.
fn typeset_with_fallback(
    doc: &Document,
    base_dir: &Path,
) -> Result<(Vec<u8>, HashSet<usize>), String> {
    let lang = doc.effective_lang();
    let fonts = typst::text_fonts(lang.as_deref());
    if ast::contains_cjk(&doc.plain_text()) && !typst::has_cjk_font(&fonts) {
        return Err(no_cjk_font_error());
    }
    let mut fallback: HashSet<usize> = HashSet::new();
    loop {
        let options = write::typst::Options {
            target: write::typst::Target::Pdf,
            fonts: fonts.clone(),
            paper: typst::default_paper(),
            base_dir: base_dir.to_path_buf(),
            tex_fallback: &fallback,
        };
        let output = write::typst::write(doc, &options);
        let project = typst::Project {
            main: output.source,
            main_name: None,
            root: base_dir.to_path_buf(),
            files: output.images,
            allow_packages: false,
        };
        let error = match typst::compile_pdf(project) {
            Ok(pdf) => return Ok((pdf, fallback)),
            Err(error) => error,
        };
        let failed: Vec<usize> = output
            .formulas
            .iter()
            .enumerate()
            .filter(|(i, range)| {
                !fallback.contains(i)
                    && error
                        .main_ranges
                        .iter()
                        .any(|r| r.start < range.end && range.start < r.end)
            })
            .map(|(i, _)| i)
            .collect();
        if failed.is_empty() {
            return Err(format!("Couldn't typeset the PDF: {}", error.message));
        }
        fallback.extend(failed);
    }
}

fn no_cjk_font_error() -> String {
    "This document contains Chinese, Japanese or Korean text, but no font for it is installed. \
     Install one (e.g. Noto CJK: the fonts-noto-cjk package on Debian/Ubuntu) and try again."
        .to_string()
}

/// Compile a user-written `.typ` file to PDF. Relative paths resolve from
/// its folder, and `@preview/…` packages are downloaded on first use.
pub fn typst_file_to_pdf(input: &Path) -> Result<Vec<u8>, String> {
    let main = std::fs::read_to_string(input)
        .map_err(|e| format!("Failed to read {}: {e}", input.display()))?;
    let project = typst::Project {
        main: main.trim_start_matches('\u{feff}').to_string(),
        main_name: input.file_name().map(|n| n.to_string_lossy().into_owned()),
        root: input.parent().unwrap_or(Path::new(".")).to_path_buf(),
        files: HashMap::new(),
        allow_packages: true,
    };
    typst::compile_pdf(project).map_err(|e| e.message)
}

/// The largest page side PDF viewers accept (Acrobat's limit), in points.
const MAX_PAGE_SIDE_PT: f64 = 14_400.0;

/// A one-page PDF the size of `image` (PNG, JPEG, GIF, WebP or SVG), one
/// point per pixel, scaled down to fit when that would exceed the largest
/// page PDF viewers accept.
pub fn image_to_pdf(image: &Path) -> Result<Vec<u8>, String> {
    let ext = image
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let vpath = format!("{}/img/0.{ext}", typst::VIRTUAL_DIR);
    // SVGs have no pixel size here; typst sizes them from their viewBox.
    let fit = match ::image::image_dimensions(image) {
        Ok((w, h)) if f64::from(w.max(h)) > MAX_PAGE_SIDE_PT => {
            if w >= h {
                format!(", width: {MAX_PAGE_SIDE_PT}pt")
            } else {
                format!(", height: {MAX_PAGE_SIDE_PT}pt")
            }
        }
        _ => String::new(),
    };
    let project = typst::Project {
        main: format!(
            "#set page(width: auto, height: auto, margin: 0pt)\n#image({}{fit})\n",
            write::typst::typst_str(&vpath)
        ),
        main_name: None,
        root: image.parent().unwrap_or(Path::new(".")).to_path_buf(),
        files: HashMap::from([(vpath, image.to_path_buf())]),
        allow_packages: false,
    };
    typst::compile_pdf(project).map_err(|e| e.message)
}

/// Typeset typst markup written by pandoc (tex → pdf without LaTeX) under Swift
/// Shifter's page template, with `meta` for the title block. `root` is where
/// its image paths resolve.
pub fn typeset_pandoc_typst(body: &str, root: &Path, meta: &ast::Meta) -> Result<Vec<u8>, String> {
    let body = unlink_dangling_labels(body);
    let lang = ast::detect_cjk_lang(&body);
    let fonts = typst::text_fonts(lang);
    if ast::contains_cjk(&body) && !typst::has_cjk_font(&fonts) {
        return Err(no_cjk_font_error());
    }
    let options = write::typst::Options {
        target: write::typst::Target::Pdf,
        fonts,
        paper: typst::default_paper(),
        base_dir: root.to_path_buf(),
        tex_fallback: &HashSet::new(),
    };
    let preamble = write::typst::preamble(
        meta,
        lang,
        &options,
        write::typst::PreambleExtras::default(),
    );
    let project = typst::Project {
        main: format!("{preamble}\n{body}\n"),
        main_name: None,
        root: root.to_path_buf(),
        files: HashMap::new(),
        allow_packages: false,
    };
    typst::compile_pdf(project).map_err(|e| format!("Couldn't typeset the PDF: {}", e.message))
}

/// Point links to labels the document never defines (broken anchors, common
/// in real ebooks) at a plain URL instead: typst rejects links to unknown
/// labels, which would fail the whole conversion. Pandoc writes labels as
/// `<name>`, or as `label("name")` when the name has other characters.
fn unlink_dangling_labels(body: &str) -> String {
    const LINK: &str = "link(";
    // Labels are defined anywhere except as a link target.
    let mut defined = HashSet::new();
    for (at, _) in body.match_indices(['<', 'l']) {
        if let Some((name, _)) = label_at(body, at)
            && !body[..at].ends_with(LINK)
        {
            defined.insert(name);
        }
    }
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(i) = rest.find(LINK) {
        let target = i + LINK.len();
        out.push_str(&rest[..target]);
        match label_at(rest, target) {
            Some((name, len)) if !defined.contains(name) => {
                // A `label("…")` name is already escaped for a string.
                out.push_str(&format!("\"#{name}\""));
                rest = &rest[target + len..];
            }
            _ => rest = &rest[target..],
        }
    }
    out.push_str(rest);
    out
}

/// The label written at `body[at..]`, `<name>` or `label("name")`, and its
/// length in bytes. A `\<` is escaped text, not a label.
fn label_at(body: &str, at: usize) -> Option<(&str, usize)> {
    let text = &body[at..];
    if let Some(after) = text.strip_prefix('<') {
        let is_label_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':');
        let len = after
            .find(|c: char| !is_label_char(c))
            .unwrap_or(after.len());
        let closed = len > 0 && after[len..].starts_with('>');
        return (closed && !body[..at].ends_with('\\')).then(|| (&after[..len], len + 2));
    }
    let after = text.strip_prefix("label(\"")?;
    if body[..at].ends_with(|c: char| c.is_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let mut escaped = false;
    let end = after.char_indices().find_map(|(i, c)| match c {
        '\\' if !escaped => {
            escaped = true;
            None
        }
        '"' if !escaped => Some(i),
        _ => {
            escaped = false;
            None
        }
    })?;
    after[end + 1..]
        .starts_with(')')
        .then(|| (&after[..end], "label(\"".len() + end + 2))
}

/// Markdown → standalone HTML, for converters that produce Markdown first
/// (PDF → HTML via pymupdf4llm).
pub fn markdown_to_html(markdown: &str, title: &str) -> String {
    let mut doc = read::markdown::read(markdown);
    if doc.meta.title.is_none() {
        doc.meta.title = Some(title.to_string());
    }
    write::html::write(&doc)
}

/// Markdown → EPUB, for converters that produce Markdown first (PDF → EPUB
/// via pymupdf4llm or marker). Images resolve from `base_dir`.
pub fn markdown_to_epub(markdown: &str, title: &str, base_dir: &Path) -> Result<Vec<u8>, String> {
    let mut doc = read::markdown::read(markdown);
    if doc.meta.title.is_none() {
        doc.meta.title = Some(title.to_string());
    }
    write::epub::write(
        &doc,
        &write::epub::Options {
            base_dir: base_dir.to_path_buf(),
        },
    )
}

/// HTML pages need a `<title>`: use the document's own, else its first
/// heading, else the file name. (Markdown and plain output only show a title
/// the document declared, so they don't get this fallback.)
fn fill_title(doc: &mut Document, input: &Path) {
    if doc.meta.title.is_some() {
        return;
    }
    let first_heading = doc.blocks.iter().find_map(|b| match b {
        Block::Heading { content, .. } => Some(ast::inline_text(content)),
        _ => None,
    });
    doc.meta.title =
        first_heading.or_else(|| input.file_stem().map(|s| s.to_string_lossy().into_owned()));
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Which kind of reference a URL is.
#[derive(Clone, Copy, PartialEq)]
enum UrlKind {
    Image,
    Link,
}

/// Calls `f` with every image and link URL in `blocks`.
fn for_each_url(blocks: &mut [Block], f: &mut dyn FnMut(UrlKind, &mut String)) {
    for block in blocks {
        match block {
            Block::Heading { content, .. } | Block::Paragraph(content) => {
                for_each_inline_url(content, f)
            }
            Block::Quote(children)
            | Block::FootnoteDefinition {
                content: children, ..
            } => for_each_url(children, f),
            Block::List(List { items, .. }) => items
                .iter_mut()
                .for_each(|item| for_each_url(&mut item.content, f)),
            Block::Table(table) => table
                .header
                .iter_mut()
                .chain(table.rows.iter_mut().flatten())
                .for_each(|cell| for_each_inline_url(cell, f)),
            _ => {}
        }
    }
}

fn for_each_inline_url(inlines: &mut [Inline], f: &mut dyn FnMut(UrlKind, &mut String)) {
    for inline in inlines {
        match inline {
            Inline::Image { url, .. } => f(UrlKind::Image, url),
            Inline::Link { url, content, .. } => {
                f(UrlKind::Link, url);
                for_each_inline_url(content, f);
            }
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strikethrough(c)
            | Inline::Superscript(c)
            | Inline::Subscript(c) => for_each_inline_url(c, f),
            _ => {}
        }
    }
}

/// Make relative image and link targets absolute against `base`, so they
/// still resolve when the output is written to a different folder (an
/// output directory, or a temp dir between conversion hops).
fn rebase_relative_urls(blocks: &mut [Block], base: &Path) {
    for_each_url(blocks, &mut |_, url| rebase_url(url, base));
}

/// For `.typ` output in another folder: copy local images into a
/// `<name>-images` folder beside it and point at the copies (typst can't
/// read files above the folder it compiles from). Links are rebased as for
/// the other formats.
fn copy_images_beside(blocks: &mut [Block], src_dir: &Path, out: &Path) -> Result<(), String> {
    let stem = out.file_stem().unwrap_or_default().to_string_lossy();
    let dir_name = format!("{stem}-images");
    let dir = out.with_file_name(&dir_name);
    let mut copied = 0usize;
    let mut error = None;
    for_each_url(blocks, &mut |kind, url| match kind {
        UrlKind::Link => rebase_url(url, src_dir),
        UrlKind::Image => {
            let Some(path) = local_path(url, src_dir) else {
                return;
            };
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let file_name = format!("{copied}-{name}");
            let copy = std::fs::create_dir_all(&dir)
                .and_then(|()| std::fs::copy(&path, dir.join(&file_name)));
            match copy {
                Ok(_) => {
                    *url = format!("{dir_name}/{file_name}");
                    copied += 1;
                }
                Err(e) => error = Some(format!("Failed to copy {}: {e}", path.display())),
            }
        }
    });
    error.map_or(Ok(()), Err)
}

/// For `.tex` output: copy local images into an ASCII-named
/// `<name>-images` folder beside it as `N.ext`, paths no LaTeX engine
/// stumbles on. PNG, JPEG and PDF are copied as they are; other raster
/// formats become PNG, and an image the `image` crate can't decode (SVG)
/// is replaced by its alt text (an empty URL). Links are rebased only when
/// `rebase_links` (the output is in another folder).
fn copy_images_for_latex(
    blocks: &mut [Block],
    src_dir: &Path,
    out: &Path,
    rebase_links: bool,
) -> Result<(), String> {
    let stem = out.file_stem().unwrap_or_default().to_string_lossy();
    let dir_name = format!("{}-images", ascii_name(&stem));
    let dir = out.with_file_name(&dir_name);
    let mut copied = 0usize;
    let mut error = None;
    for_each_url(blocks, &mut |kind, url| match kind {
        UrlKind::Link if rebase_links => rebase_url(url, src_dir),
        UrlKind::Link => {}
        UrlKind::Image => {
            let Some(path) = local_path(url, src_dir) else {
                return;
            };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            let as_is = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "pdf");
            let file_name = format!("{}.{}", copied + 1, if as_is { &ext } else { "png" });
            let target = dir.join(&file_name);
            if let Err(e) = std::fs::create_dir_all(&dir) {
                error = Some(format!("Failed to create {}: {e}", dir.display()));
                return;
            }
            let written = if as_is {
                std::fs::copy(&path, &target)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            } else {
                match ::image::open(&path) {
                    Ok(image) => image.save(&target).map_err(|e| e.to_string()),
                    // Not a raster format we read (SVG): alt text instead.
                    Err(::image::ImageError::Decoding(_) | ::image::ImageError::Unsupported(_)) => {
                        url.clear();
                        return;
                    }
                    Err(e) => Err(e.to_string()),
                }
            };
            match written {
                Ok(()) => {
                    *url = format!("{dir_name}/{file_name}");
                    copied += 1;
                }
                Err(e) => error = Some(format!("Failed to copy {}: {e}", path.display())),
            }
        }
    });
    error.map_or(Ok(()), Err)
}

/// `name` as ASCII letters, digits and `-`, for paths LaTeX reads. When
/// characters had to go, a short hash of the original name is appended,
/// so `報告` and `提案` don't share (and overwrite) one image folder.
fn ascii_name(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut kept = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            kept.push(c);
        } else if !kept.ends_with('-') {
            kept.push('-');
        }
    }
    let lossy = kept != name;
    let mut out = kept.trim_matches('-').to_string();
    if out.is_empty() {
        out = "document".to_string();
    }
    if lossy {
        let hash = Sha256::digest(name.as_bytes());
        out.push_str(&format!("-{:02x}{:02x}{:02x}", hash[0], hash[1], hash[2]));
    }
    out
}

/// The local file a URL points at, if it exists: a `file://` URL, an
/// absolute path, or a path relative to `base` (percent-decoded, without
/// any `?query` or `#fragment`). `None` for remote URLs, data URLs, in-page
/// anchors and missing files.
pub(crate) fn local_path(url: &str, base: &Path) -> Option<PathBuf> {
    let path = if url.starts_with("file://") {
        url::Url::parse(url).ok()?.to_file_path().ok()?
    } else if url.is_empty()
        || url.starts_with('#')
        || url.contains("://")
        || url.starts_with("data:")
    {
        return None;
    } else {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        base.join(percent_decode(path))
    };
    path.is_file().then_some(path)
}

/// Point a relative target that exists next to the input at it by absolute
/// `file://` URL (percent-encoded, so spaces, `#` and Windows paths work).
fn rebase_url(url: &mut String, base: &Path) {
    let is_relative_path = !url.is_empty()
        && !url.starts_with('#')
        && !url.starts_with('/')
        && !url.starts_with('\\')
        && !url.contains(':');
    if !is_relative_path {
        return;
    }
    let (path, rest) = match url.find(['#', '?']) {
        Some(i) => url.split_at(i),
        None => (url.as_str(), ""),
    };
    let absolute = base.join(percent_decode(path));
    if !absolute.exists() {
        return;
    }
    if let Ok(file_url) = url::Url::from_file_path(&absolute) {
        *url = format!("{file_url}{rest}");
    }
}

/// Decode `%XX` escapes (relative URLs in HTML are usually encoded); invalid
/// sequences are kept as written.
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_only_native_pairs() {
        assert!(handles("md", "html"));
        assert!(handles("html", "md"));
        assert!(handles("txt", "md"));
        assert!(!handles("md", "md"));
        assert!(handles("md", "pdf"));
        assert!(handles("html", "typst"));
        assert!(handles("md", "tex"));
        assert!(handles("epub", "tex"));
        assert!(!handles("tex", "html"));
    }

    #[test]
    fn converts_files_and_falls_back_to_heading_title() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("notes.md");
        std::fs::write(&input, "\u{feff}# Notes\n\nBody\n").unwrap();
        let out = dir.path().join("notes.html");
        convert(&input, "html", &out).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains("<title>Notes</title>"), "{html}");
        assert!(html.contains("<p>Body</p>"));
    }

    #[test]
    fn title_falls_back_to_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("plain notes.txt");
        std::fs::write(&input, "just text").unwrap();
        let out = dir.path().join("plain notes.html");
        convert(&input, "html", &out).unwrap();
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .contains("<title>plain notes</title>")
        );
    }

    #[test]
    fn relative_images_are_rebased_when_output_moves() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("img")).unwrap();
        std::fs::write(src.path().join("img/a b.png"), b"png").unwrap();
        let input = src.path().join("doc.md");
        std::fs::write(
            &input,
            "![pic](<img/a b.png>) [missing](nope.md) [web](https://x.org)\n",
        )
        .unwrap();
        let out = dst.path().join("doc.html");
        convert(&input, "html", &out).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        let expected = url::Url::from_file_path(src.path().join("img/a b.png")).unwrap();
        assert!(html.contains(&format!("src=\"{expected}\"")), "{html}");
        assert!(expected.as_str().contains("a%20b.png"));
        assert!(
            html.contains("href=\"nope.md\""),
            "missing targets are left alone"
        );
        assert!(html.contains("href=\"https://x.org\""));
    }

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/docs")
            .join(name);
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn markdown_output_is_a_fixed_point() {
        for name in ["kitchen-sink.md", "cjk.md", "pymupdf-output.md"] {
            let once = write::markdown::write(&read::markdown::read(&fixture(name)));
            let twice = write::markdown::write(&read::markdown::read(&once));
            assert_eq!(once, twice, "{name}: writing what we wrote changed it");
            assert_eq!(
                read::markdown::read(&once),
                read::markdown::read(&twice),
                "{name}: AST drifted"
            );
        }
    }

    #[test]
    fn kitchen_sink_keeps_its_structure_through_markdown() {
        let doc = read::markdown::read(&fixture("kitchen-sink.md"));
        let again = read::markdown::read(&write::markdown::write(&doc));
        assert_eq!(again.meta, doc.meta);
        assert_eq!(again.blocks.len(), doc.blocks.len());
        let md = write::markdown::write(&doc);
        assert!(md.contains("Snake_case_identifiers and 3 \\* 4"), "{md}");
        assert!(md.contains("| :-- | --: | :-: |"), "{md}");
        assert!(md.contains("[^1]: A footnote with **bold**."), "{md}");
    }

    #[test]
    fn messy_html_page_becomes_clean_markdown() {
        let md = write::markdown::write(&read::html::read(&fixture("messy-page.html")));
        for chrome in ["Logo", "Sidebar", "track", "© Site", "color: red"] {
            assert!(!md.contains(chrome), "page chrome {chrome:?} leaked: {md}");
        }
        for kept in [
            "title: A Messy Page",
            "author:\n- Web Author",
            "# The Real *Title*",
            "Loose intro text with **bold** and a [relative link](docs/guide.html).",
            "Paragraph one.\\\nSecond line.",
            "![A chart](img/chart.png)",
            "*Figure 1: growth*",
            "| Year | Value |",
            "| 2025 | 10<br>approx |",
            "| Total pending |  |",
            "```python\ndef f():\n    return 1\n```",
            "Math: $x^2$",
            "- [x] shipped\n- [ ] pending",
            "Notes: written in 2026.",
        ] {
            assert!(md.contains(kept), "missing {kept:?} in:\n{md}");
        }
    }

    #[test]
    fn txt_input_stays_literal_in_html() {
        let html = write::html::write(&read::plain::read(&fixture("notes.txt")));
        assert!(
            html.contains("<p>Shopping list<br>\n* eggs<br>\n* milk</p>"),
            "{html}"
        );
        assert!(html.contains("<p># not a heading, 2 * 3 = 6</p>"), "{html}");
    }

    #[test]
    fn cjk_documents_declare_their_language() {
        let html = write::html::write(&read::markdown::read(&fixture("cjk.md")));
        assert!(
            html.contains("<html lang=\"zh-Hant\">"),
            "mostly Traditional Chinese: {html}"
        );
        assert!(html.contains("<h1 id=\"中文標題\">中文標題</h1>"), "{html}");
    }

    #[test]
    fn percent_decoding_handles_valid_and_invalid_escapes() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%e4%b8%ad"), "%zz中");
    }

    /// Markdown written from any reader's output must read back unchanged.
    #[test]
    fn markdown_round_trips_from_plain_and_html_sources() {
        let sources = [
            read::plain::read(&fixture("notes.txt")),
            read::plain::read("    indented\n  - x\n  # y\n  1. z\nAT&amp;T &copy;\n| pipe |"),
            read::html::read(&fixture("messy-page.html")),
            read::html::read(
                "<h1>Main<br>Sub</h1><ul><li>a</li></ul><ul><li>b</li></ul>\
                 <table><tr><th>h</th></tr><tr><td><code>a|b</code></td></tr></table>",
            ),
        ];
        for doc in sources {
            let md = write::markdown::write(&doc);
            let again = read::markdown::read(&md);
            // Headings flatten their line breaks, by design.
            let flatten = |blocks: &[Block]| -> Vec<Block> {
                blocks
                    .iter()
                    .map(|b| match b {
                        Block::Heading { level, content } => Block::Heading {
                            level: *level,
                            content: vec![Inline::Text(ast::inline_text(content))],
                        },
                        other => other.clone(),
                    })
                    .collect()
            };
            assert_eq!(
                flatten(&again.blocks),
                flatten(&doc.blocks),
                "changed through:\n{md}"
            );
        }
    }

    fn page_count(pdf: &[u8]) -> usize {
        lopdf::Document::load_mem(pdf)
            .expect("valid PDF")
            .get_pages()
            .len()
    }

    #[test]
    fn every_fixture_typesets_to_pdf() {
        let docs = [
            (
                "kitchen-sink.md",
                read::markdown::read(&fixture("kitchen-sink.md")),
                "Kitchen Sink",
            ),
            ("cjk.md", read::markdown::read(&fixture("cjk.md")), "Lists"),
            (
                "messy-page.html",
                read::html::read(&fixture("messy-page.html")),
                "Paragraph one",
            ),
            (
                "notes.txt",
                read::plain::read(&fixture("notes.txt")),
                "Shopping list",
            ),
            (
                "pymupdf-output.md",
                read::markdown::read(&fixture("pymupdf-output.md")),
                "Revenue",
            ),
        ];
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs");
        for (name, doc, _) in docs {
            let pdf = match typeset(&doc, &base) {
                Ok(pdf) => pdf,
                // A machine without CJK fonts can't typeset the CJK fixture,
                // and says so.
                Err(e) if name == "cjk.md" && e.contains("no font for it is installed") => continue,
                Err(e) => panic!("{name}: {e}"),
            };
            assert!(pdf.starts_with(b"%PDF") && page_count(&pdf) >= 1, "{name}");
        }
    }

    #[test]
    fn formulas_the_compiler_rejects_fall_back_to_tex() {
        let doc = read::markdown::read("Good $x^2$, odd $\\frac{a}$, good $y_1$ and fine text.\n");
        let (pdf, fallback) = typeset_with_fallback(&doc, &std::env::temp_dir()).unwrap();
        assert_eq!(page_count(&pdf), 1);
        // mitex turns `\frac{a}` into `frac(a)`, which typst rejects.
        assert_eq!(fallback, HashSet::from([1]), "only the odd formula");
        let doc = read::markdown::read("All good: $x^2$ and $$\\frac{a}{b}$$\n");
        let (_, fallback) = typeset_with_fallback(&doc, &std::env::temp_dir()).unwrap();
        assert!(fallback.is_empty());
    }

    fn media_box_size(pdf: &[u8]) -> (f32, f32) {
        let doc = lopdf::Document::load_mem(pdf).unwrap();
        let (_, page_id) = doc.get_pages().into_iter().next().unwrap();
        let page = doc.get_dictionary(page_id).unwrap();
        let media_box = page.get(b"MediaBox").unwrap().as_array().unwrap();
        let num = |o: &lopdf::Object| {
            o.as_float()
                .or_else(|_| o.as_i64().map(|i| i as f32))
                .unwrap()
        };
        (
            num(&media_box[2]) - num(&media_box[0]),
            num(&media_box[3]) - num(&media_box[1]),
        )
    }

    #[test]
    fn images_become_pages_of_their_size() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("wide.png");
        image::RgbImage::new(200, 100).save(&png).unwrap();
        let (w, h) = media_box_size(&image_to_pdf(&png).unwrap());
        assert!((w / h - 2.0).abs() < 0.01, "aspect ratio kept: {w}x{h}");
    }

    #[test]
    fn huge_images_are_scaled_to_the_largest_page() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("banner.png");
        image::GrayImage::new(16_000, 50).save(&png).unwrap();
        let (w, h) = media_box_size(&image_to_pdf(&png).unwrap());
        assert!(w <= MAX_PAGE_SIDE_PT as f32 + 0.5, "{w}x{h}");
        assert!((w / h - 320.0).abs() < 1.0, "aspect ratio kept: {w}x{h}");
    }

    #[test]
    fn typst_files_compile_with_their_own_relative_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chapter.typ"), "Included chapter text.").unwrap();
        let main = dir.path().join("main.typ");
        std::fs::write(&main, "= Book\n#include \"chapter.typ\"\n").unwrap();
        let pdf = typst_file_to_pdf(&main).unwrap();
        assert_eq!(page_count(&pdf), 1);
        // A missing include fails, so the include above was really read.
        std::fs::remove_file(dir.path().join("chapter.typ")).unwrap();
        assert!(
            typst_file_to_pdf(&main)
                .unwrap_err()
                .contains("chapter.typ")
        );
    }

    #[test]
    fn typst_errors_name_the_users_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.typ"), "#let greeting = [Hello]").unwrap();
        let report = dir.path().join("report.typ");
        std::fs::write(&report, "#import \"main.typ\": greeting\n#greeting\n").unwrap();
        // A sibling `main.typ` is the user's, not the compiled file.
        assert_eq!(page_count(&typst_file_to_pdf(&report).unwrap()), 1);
        std::fs::write(&report, "Fine.\n\n#undefined-function()\n").unwrap();
        let error = typst_file_to_pdf(&report).unwrap_err();
        assert!(error.contains("report.typ:3:"), "{error}");
    }

    #[test]
    fn dangling_label_links_become_plain_urls() {
        let body = "#link(<intro>)[Intro] #link(<gone>)[Gone] #link(<a.b-c>)[X]\n\
                    = Intro <intro>\n\\<not-a-label> #link(<not-a-label>)[Y]\n";
        assert_eq!(
            unlink_dangling_labels(body),
            "#link(<intro>)[Intro] #link(\"#gone\")[Gone] #link(\"#a.b-c\")[X]\n\
             = Intro <intro>\n\\<not-a-label> #link(\"#not-a-label\")[Y]\n"
        );
        // Not a label link: left alone.
        assert_eq!(unlink_dangling_labels("#link(<x y>)"), "#link(<x y>)");
        // Pandoc's form for names with other characters.
        assert_eq!(
            unlink_dangling_labels(
                "#link(label(\"weird id!\"))[x] #link(label(\"a \\\" b\"))[y]\n#label(\"a \\\" b\")"
            ),
            "#link(\"#weird id!\")[x] #link(label(\"a \\\" b\"))[y]\n#label(\"a \\\" b\")"
        );
        assert_eq!(
            unlink_dangling_labels("#mylabel(\"x\") #link(label(\"x\"))"),
            "#mylabel(\"x\") #link(\"#x\")"
        );
    }

    #[test]
    fn pandoc_typst_with_broken_links_still_typesets() {
        let meta = ast::Meta {
            title: Some("Book".into()),
            authors: vec!["A. Author".into()],
            ..Default::default()
        };
        let body = "= Chapter <ch1>\nSee #link(<ch1>)[above] and #link(<missing>)[nowhere].\n";
        let pdf = typeset_pandoc_typst(body, &std::env::temp_dir(), &meta).unwrap();
        assert_eq!(page_count(&pdf), 1);
    }

    #[test]
    fn local_paths_decode_and_drop_queries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("my pic.png"), b"x").unwrap();
        let expected = Some(dir.path().join("my pic.png"));
        assert_eq!(local_path("my%20pic.png?v=2#top", dir.path()), expected);
        let file_url = url::Url::from_file_path(dir.path().join("my pic.png")).unwrap();
        assert_eq!(local_path(file_url.as_str(), Path::new("/")), expected);
        for url in [
            "missing.png",
            "#top",
            "https://x.test/a.png",
            "data:image/png;base64,AA",
            "",
        ] {
            assert_eq!(local_path(url, dir.path()), None, "{url}");
        }
    }

    #[test]
    fn typst_output_elsewhere_copies_images_beside_it() {
        let src = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();
        image::RgbImage::new(4, 4)
            .save(src.path().join("a b.png"))
            .unwrap();
        std::fs::write(src.path().join("other.md"), "x").unwrap();
        let mut doc =
            read::markdown::read("![one](a%20b.png) ![gone](missing.png) [link](other.md)\n");
        let out = out_dir.path().join("notes.typ");
        copy_images_beside(&mut doc.blocks, src.path(), &out).unwrap();
        let mut urls = Vec::new();
        for_each_url(&mut doc.blocks, &mut |_, url| urls.push(url.clone()));
        assert_eq!(urls[0], "notes-images/0-a b.png");
        assert!(out_dir.path().join("notes-images/0-a b.png").is_file());
        assert_eq!(urls[1], "missing.png", "missing images are left as written");
        assert!(
            urls[2].starts_with("file://"),
            "links are rebased: {}",
            urls[2]
        );
    }

    #[test]
    fn epub_input_keeps_its_images_beside_the_output() {
        let src = tempfile::tempdir().unwrap();
        image::RgbImage::new(2, 2)
            .save(src.path().join("pic.png"))
            .unwrap();
        let md = src.path().join("book.md");
        std::fs::write(&md, "# Book\n\n![pic](pic.png)\n").unwrap();
        let epub = src.path().join("book.epub");
        convert(&md, "epub", &epub).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let out = out_dir.path().join("book.md");
        convert(&epub, "md", &out).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("![pic](book-images/0-1.png)"), "{text}");
        assert!(out_dir.path().join("book-images/0-1.png").is_file());
        // PDF reads the images from the extracted book directly.
        convert(&epub, "pdf", &out_dir.path().join("book.pdf")).unwrap();
    }

    #[test]
    fn ascii_names_stay_distinct() {
        assert_eq!(ascii_name("report-2024"), "report-2024");
        assert_ne!(ascii_name("報告"), ascii_name("提案"));
        assert!(ascii_name("報告").starts_with("document-"));
        assert_ne!(ascii_name("résumé"), ascii_name("résumà"));
    }

    #[test]
    fn latex_output_gets_safe_image_copies() {
        let src = tempfile::tempdir().unwrap();
        image::RgbImage::new(2, 2)
            .save(src.path().join("my pic.png"))
            .unwrap();
        image::RgbImage::new(2, 2)
            .save(src.path().join("b.bmp"))
            .unwrap();
        std::fs::write(
            src.path().join("c.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        )
        .unwrap();
        std::fs::write(src.path().join("other.md"), "x").unwrap();
        let md = src.path().join("doc.md");
        std::fs::write(
            &md,
            "![a](my%20pic.png) ![b](b.bmp) ![c](c.svg) [o](other.md)\n",
        )
        .unwrap();
        // Same folder as the input: images are still copied, into a folder
        // with an ASCII name; links stay relative.
        let out = src.path().join("my #1 ~résumé.tex");
        convert(&md, "tex", &out).unwrap();
        let tex = std::fs::read_to_string(&out).unwrap();
        let dir = format!("{}-images", ascii_name("my #1 ~résumé"));
        assert!(dir.starts_with("my-1-r-sum-") && dir.is_ascii(), "{dir}");
        let dir = dir.as_str();
        assert!(
            tex.contains(&format!("\\includegraphics[height=1.2em]{{{dir}/1.png}}")),
            "{tex}"
        );
        assert!(
            tex.contains(&format!("{{{dir}/2.png}}")) && tex.contains("{[}c{]}"),
            "{tex}"
        );
        assert!(
            image::open(src.path().join(dir).join("2.png")).is_ok(),
            "bmp became png"
        );
        assert!(!src.path().join(dir).join("3.png").exists());
        assert!(tex.contains("\\href{other.md}{o}"), "{tex}");
    }

    /// Our LaTeX, read back by pandoc's LaTeX reader, has the source's
    /// structure. Skipped where pandoc isn't installed.
    #[test]
    fn pandoc_reads_our_latex_back_with_the_same_structure() {
        let Some(pandoc) = crate::converter::document::binaries::find_pandoc_binary() else {
            eprintln!("skipping: pandoc not found");
            return;
        };
        /// Headings, then counts of tables, code blocks, display math and
        /// notes, allowing for how pandoc's GFM writer represents them.
        fn outline(doc: &Document) -> (Vec<(u8, String)>, [usize; 4]) {
            let headings = doc
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Heading { level, content } => Some((*level, ast::inline_text(content))),
                    _ => None,
                })
                .collect();
            let is_math = |b: &Block| {
                matches!(b, Block::Math(_))
                    || matches!(b, Block::Code { lang: Some(l), .. } if l == "math")
            };
            let count = |f: &dyn Fn(&Block) -> bool| doc.blocks.iter().filter(|b| f(b)).count();
            (
                headings,
                [
                    // A table pandoc can't write as GFM comes out as HTML.
                    count(&|b| {
                        matches!(b, Block::Table(_))
                            || matches!(b, Block::Raw { text, .. } if text.trim_start().starts_with("<table"))
                    }),
                    count(&|b| matches!(b, Block::Code { .. }) && !is_math(b)),
                    count(&is_math),
                    count(&|b| matches!(b, Block::FootnoteDefinition { .. })),
                ],
            )
        }
        for name in ["kitchen-sink.md", "pymupdf-output.md"] {
            let doc = read::markdown::read(&fixture(name));
            let tex = write::latex::write(&doc);
            let output = std::process::Command::new(&pandoc)
                .args(["-f", "latex", "-t", "gfm", "--wrap=none"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(tex.as_bytes())?;
                    child.wait_with_output()
                })
                .unwrap();
            assert!(output.status.success(), "{name}: pandoc failed on\n{tex}");
            let back = read::markdown::read(&String::from_utf8_lossy(&output.stdout));
            assert_eq!(outline(&back), outline(&doc), "{name}");
        }
    }

    #[test]
    fn markdown_to_html_uses_given_title() {
        let html = markdown_to_html("a<br>b\n\n| x |\n|---|\n| 1<br>2 |\n", "report");
        assert!(html.contains("<title>report</title>"));
        assert!(html.contains("<td>1<br>\n2</td>"), "{html}");
    }
}
