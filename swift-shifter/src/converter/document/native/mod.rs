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

use crate::converter::Converted;
use crate::converter::document::ProgressPayload;
use crate::converter::document::utils::{output_path, target_to_ext};
use ast::{Block, Document, Inline, List};

/// Input formats the native readers handle (normalized extensions).
pub const INPUTS: &[&str] = &["md", "txt", "html", "epub", "typst"];
/// Output formats the native writers handle.
pub const OUTPUTS: &[&str] = &["md", "txt", "html", "pdf", "typst", "epub", "tex"];

/// Something of the input a conversion left out of its output. The
/// conversion still succeeds; this says what it lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// What was lost, said of one: "SVG image became its alt text".
    pub one: &'static str,
    /// The same, said of several: "SVG images became their alt text".
    pub many: &'static str,
    pub count: usize,
    /// The input line, where the reader knows it.
    pub line: Option<usize>,
}

impl Warning {
    /// "2 SVG images became their alt text", "1 … at line 12".
    pub fn message(&self) -> String {
        let what = if self.count == 1 { self.one } else { self.many };
        match self.line {
            Some(line) => format!("{} {what} at line {line}", self.count),
            None => format!("{} {what}", self.count),
        }
    }
}

/// For the UI: the message, with the count and line it's made of.
impl serde::Serialize for Warning {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(serde::Serialize)]
        struct Shown {
            message: String,
            count: usize,
            line: Option<usize>,
        }
        Shown {
            message: self.message(),
            count: self.count,
            line: self.line,
        }
        .serialize(serializer)
    }
}

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
) -> Result<Converted, String> {
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
    let warnings = tokio::task::spawn_blocking(move || convert(&input, &target, &out_for_task))
        .await
        .map_err(|e| format!("task panicked: {e}"))??;
    progress(100.0);
    Ok(Converted {
        output: out.to_string_lossy().into_owned(),
        warnings,
    })
}

/// Synchronous core of [`convert_document`]: also returns what the
/// conversion left out.
pub fn convert(input: &Path, target: &str, out: &Path) -> Result<Vec<Warning>, String> {
    let from = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| crate::converter::graph::normalize_ext(&e.to_lowercase()).to_string())
        .unwrap_or_default();

    // A typst file is compiled as written; reading it into the AST would
    // only lose what typst can lay out.
    if from == "typst" && target == "pdf" {
        let pdf = typst_file_to_pdf(input)?;
        std::fs::write(out, pdf).map_err(|e| format!("Failed to write {}: {e}", out.display()))?;
        return Ok(Vec::new());
    }

    // An EPUB is extracted to a temp folder its images are read from; it
    // must outlive the writing.
    let mut book_dir = None;
    let mut doc = match from.as_str() {
        "epub" => {
            let book = read::epub::read(input)?;
            book_dir = Some(book.dir);
            book.doc
        }
        "md" | "txt" | "html" | "typst" => {
            let source = std::fs::read(input)
                .map_err(|e| format!("Failed to read {}: {e}", input.display()))?;
            // Lossy rather than failing on a stray invalid byte; a BOM is dropped.
            let source = String::from_utf8_lossy(&source);
            let source = source.trim_start_matches('\u{feff}');
            match from.as_str() {
                "md" => read::markdown::read(source),
                "txt" => read::plain::read(source),
                "typst" => read::typst::read_in(source, input.parent().unwrap_or(Path::new("."))),
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
        None => input_dir(input),
    };
    let elsewhere = out
        .parent()
        .is_some_and(|out_dir| !same_dir(&base_dir, out_dir));
    // Raw HTML elements shown as text alone.
    let mut raw_html_lost = 0;
    let mut svgs_as_alt_text = 0;
    if target == "tex" {
        raw_html_lost += read_raw_inline_html(&mut doc.blocks);
        svgs_as_alt_text = copy_images_for_latex(
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

    // Markdown, HTML and EPUB keep raw HTML; the other writers show the
    // text of what they can't read.
    if !matches!(target, "md" | "html" | "epub") {
        raw_html_lost += write::raw_html_elements(&doc);
    }
    let mut warnings = Vec::new();
    if raw_html_lost > 0 {
        warnings.push(Warning {
            one: "raw HTML element kept only its text",
            many: "raw HTML elements kept only their text",
            count: raw_html_lost,
            line: None,
        });
    }
    if svgs_as_alt_text > 0 {
        warnings.push(Warning {
            one: "SVG image became its alt text",
            many: "SVG images became their alt text",
            count: svgs_as_alt_text,
            line: None,
        });
    }

    let rendered = match target {
        "md" => write::markdown::write(&doc).into_bytes(),
        "txt" => write::plain::write(&doc).into_bytes(),
        "html" => write::html::write(&doc).into_bytes(),
        "typst" => typst_file_source(&doc, &base_dir).into_bytes(),
        "tex" => write::latex::write(&doc).into_bytes(),
        "pdf" => {
            let (pdf, shown_as_tex) = typeset(&doc, &base_dir)?;
            if shown_as_tex > 0 {
                warnings.push(Warning {
                    one: "formula couldn't be typeset and is shown as TeX",
                    many: "formulas couldn't be typeset and are shown as TeX",
                    count: shown_as_tex,
                    line: None,
                });
            }
            pdf
        }
        "epub" => {
            let options = write::epub::Options {
                base_dir: base_dir.clone(),
            };
            let (epub, dropped) = write::epub::write_reporting(&doc, &options)?;
            let lost = [
                (
                    dropped.elements,
                    "script or embed was left out of the book",
                    "scripts and embeds were left out of the book",
                ),
                (
                    dropped.images,
                    "image couldn't be packaged and became its alt text",
                    "images couldn't be packaged and became their alt text",
                ),
                (
                    dropped.links,
                    "link that can't work in a book kept only its text",
                    "links that can't work in a book kept only their text",
                ),
            ];
            for (count, one, many) in lost {
                if count > 0 {
                    warnings.push(Warning {
                        one,
                        many,
                        count,
                        line: None,
                    });
                }
            }
            epub
        }
        other => return Err(format!("The native converter can't write {other}")),
    };
    std::fs::write(out, rendered).map_err(|e| format!("Failed to write {}: {e}", out.display()))?;
    Ok(warnings)
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
/// fail the whole conversion. Also returns how many formulas it shows as
/// TeX: those, and those mitex can't convert.
pub fn typeset(doc: &Document, base_dir: &Path) -> Result<(Vec<u8>, usize), String> {
    typeset_with_fallback(doc, base_dir).map(|(pdf, _, shown_as_tex)| (pdf, shown_as_tex))
}

/// [`typeset`], also returning the indices of the formulas the compiler
/// rejected.
fn typeset_with_fallback(
    doc: &Document,
    base_dir: &Path,
) -> Result<(Vec<u8>, HashSet<usize>, usize), String> {
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
        // The writer shows a formula as TeX in `#raw(…)`, whether it's
        // one of `fallback` or mitex couldn't convert it.
        let shown_as_tex = output
            .formulas
            .iter()
            .filter(|range| output.source[(*range).clone()].starts_with("#raw("))
            .count();
        let project = typst::Project {
            main: output.source,
            main_name: None,
            root: base_dir.to_path_buf(),
            files: output.images,
            allow_packages: false,
        };
        let error = match typst::compile_pdf(project) {
            Ok(pdf) => return Ok((pdf, fallback, shown_as_tex)),
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

/// The folder `input` is in, absolute, as the `file://` URLs that rebased
/// targets become must be.
fn input_dir(input: &Path) -> PathBuf {
    let dir = input
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    // A bare file name's parent is empty: the current folder.
    let dir = |p: &Path| {
        if p.as_os_str().is_empty() {
            Path::new(".").canonicalize()
        } else {
            p.canonicalize()
        }
    };
    match (dir(a), dir(b)) {
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
/// output directory, or a temp dir between conversion hops). Raw HTML is
/// included.
fn rebase_relative_urls(blocks: &mut [Block], base: &Path) {
    for_each_url(blocks, &mut |_, url| rebase_url(url, base));

    // The raw HTML is rewritten as one piece, joined in document order, so
    // a `<textarea>` or `<script>` opened in one raw block or tag (the
    // Markdown reader gives inline HTML tag by tag) keeps what follows it as
    // text. The separator, a private-use character the HTML doesn't hold,
    // is text wherever it falls.
    let mut pieces = Vec::new();
    for_each_raw(blocks, &mut |html| pieces.push(html.clone()));
    let Some(sep) = ('\u{E000}'..='\u{F8FF}').find(|&c| !pieces.iter().any(|p| p.contains(c)))
    else {
        return;
    };
    let joined = pieces.join(&sep.to_string());
    let rewritten = rebase_raw_html_urls(&joined, base);
    let rewritten: Vec<&str> = rewritten.split(sep).collect();
    // Never short of pieces, unless a rebased URL held the separator.
    if rewritten.len() != pieces.len() {
        return;
    }
    let mut rewritten = rewritten.into_iter();
    for_each_raw(blocks, &mut |html| {
        if let Some(new) = rewritten.next() {
            *html = new.to_string();
        }
    });
}

/// Calls `f` with every raw HTML block and raw inline tag in `blocks`, in
/// document order.
fn for_each_raw(blocks: &mut [Block], f: &mut dyn FnMut(&mut String)) {
    fn walk(inlines: &mut [Inline], f: &mut dyn FnMut(&mut String)) {
        for inline in inlines {
            match inline {
                Inline::Raw { text, .. } => f(text),
                Inline::Emph(c)
                | Inline::Strong(c)
                | Inline::Strikethrough(c)
                | Inline::Superscript(c)
                | Inline::Subscript(c)
                | Inline::Link { content: c, .. } => walk(c, f),
                _ => {}
            }
        }
    }
    for block in blocks {
        match block {
            Block::Raw { text, .. } => f(text),
            Block::Heading { content, .. } | Block::Paragraph(content) => walk(content, f),
            Block::Quote(children)
            | Block::FootnoteDefinition {
                content: children, ..
            } => for_each_raw(children, f),
            Block::List(List { items, .. }) => items
                .iter_mut()
                .for_each(|item| for_each_raw(&mut item.content, f)),
            Block::Table(table) => table
                .header
                .iter_mut()
                .chain(table.rows.iter_mut().flatten())
                .for_each(|cell| walk(cell, f)),
            _ => {}
        }
    }
}

/// `html` with the `src` and `href` values of its start tags rebased (see
/// [`rebase_url`]). lol_html tokenizes it as a browser would (comments,
/// scripts and `<textarea>` text are left alone) and writes back every byte
/// it isn't asked to change; parsing and serializing the HTML instead would
/// close a tag that a later block ends (`<details>` around Markdown) and
/// drop comments. A rebased value is written double-quoted and its tag
/// loses any space before the `>`. As in browsers, only the first of a
/// repeated attribute counts. `srcset` and `poster` aren't rebased.
fn rebase_raw_html_urls(html: &str, base: &Path) -> String {
    let rebase = lol_html::element!("[src], [href]", |el| {
        for name in ["src", "href"] {
            let Some(written) = el.get_attribute(name) else {
                continue;
            };
            let value = decode_attr_value(&written);
            let mut url = value.clone();
            rebase_url(&mut url, base);
            if url != value {
                // lol_html escapes only the `"`.
                el.set_attribute(name, &url.replace('&', "&amp;"))?;
            }
        }
        Ok(())
    });
    let settings = lol_html::RewriteStrSettings::new().append_element_content_handler(rebase);
    lol_html::rewrite_str(html, settings).unwrap_or_else(|_| html.to_string())
}

/// An attribute value as lol_html gives it (character references still in
/// it), decoded as HTML reads it: `&amp;`, `&copy;`, `&#38;`, and the
/// attribute rules for a reference without its `;`.
fn decode_attr_value(written: &str) -> String {
    if !written.contains('&') {
        return written.to_string();
    }
    let attr = written.replace('"', "&quot;");
    let html = scraper::Html::parse_fragment(&format!("<a href=\"{attr}\">"));
    html.root_element()
        .descendants()
        .filter_map(scraper::ElementRef::wrap)
        .find_map(|a| a.value().attr("href"))
        .unwrap_or(written)
        .to_string()
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
/// `<name>-images` folder beside it as `N.ext` (see [`LatexImages`]), and
/// point the URLs there. Links are rebased only when `rebase_links` (the
/// output is in another folder). The LaTeX writer reads raw HTML blocks
/// itself, so headings in them take no id of the main text's (see
/// [`DocumentIds`](write::DocumentIds)): their images and links are
/// rewritten in the HTML. Returns how many SVG images became their alt
/// text.
fn copy_images_for_latex(
    blocks: &mut [Block],
    src_dir: &Path,
    out: &Path,
    rebase_links: bool,
) -> Result<usize, String> {
    let mut images = LatexImages::new(src_dir, out);
    for_each_url(blocks, &mut |kind, url| match kind {
        UrlKind::Link if rebase_links => rebase_url(url, src_dir),
        UrlKind::Link => {}
        UrlKind::Image => match images.copy(url) {
            LatexImage::Copied(path) => *url = path,
            LatexImage::AltText => url.clear(),
            LatexImage::Unchanged => {}
        },
    });
    for_each_raw_html(blocks, &mut |html| {
        // An image with no source shows its alt text, as the AST's do.
        let mut image = |src: &str| {
            Some(match images.copy(src) {
                LatexImage::Copied(path) => path,
                LatexImage::AltText => String::new(),
                LatexImage::Unchanged => src.to_string(),
            })
        };
        let link = |href: &str| {
            let mut href = href.to_string();
            if rebase_links {
                rebase_url(&mut href, src_dir);
            }
            Some(href)
        };
        let mut hooks = write::xhtml::Hooks {
            image: &mut image,
            link: &link,
        };
        *html = write::xhtml::rewrite_urls(html, &mut hooks);
    });
    images.error.map_or(Ok(images.svgs_as_alt_text), Err)
}

/// The images `.tex` output shows, copied beside it into one folder with
/// ASCII paths no LaTeX engine stumbles on: PNG, JPEG and PDF as they are,
/// other raster formats as PNG.
struct LatexImages<'a> {
    src_dir: &'a Path,
    /// The folder, `<name>-images` beside the output.
    dir: PathBuf,
    /// The folder's name, which copied images' paths start with.
    dir_name: String,
    copied: usize,
    /// SVG images shown as their alt text, which LaTeX can't include.
    svgs_as_alt_text: usize,
    /// Why a copy failed, which fails the conversion.
    error: Option<String>,
}

/// What an image becomes in `.tex` output (see [`LatexImages::copy`]).
enum LatexImage {
    /// A copy, at this path from the output's folder.
    Copied(String),
    /// Its alt text: a missing local image, which would stop the LaTeX
    /// run, or one the `image` crate can't decode (SVG).
    AltText,
    /// As it is: a remote image, left for the writer, or one whose copy
    /// failed.
    Unchanged,
}

impl<'a> LatexImages<'a> {
    fn new(src_dir: &'a Path, out: &Path) -> Self {
        let stem = out.file_stem().unwrap_or_default().to_string_lossy();
        let dir_name = format!("{}-images", ascii_name(&stem));
        LatexImages {
            src_dir,
            dir: out.with_file_name(&dir_name),
            dir_name,
            copied: 0,
            svgs_as_alt_text: 0,
            error: None,
        }
    }

    /// Copies the image at `url`, as `N.ext` for the next `N`.
    fn copy(&mut self, url: &str) -> LatexImage {
        let Some(path) = local_path(url, self.src_dir) else {
            return if write::latex::is_local(url) {
                LatexImage::AltText
            } else {
                LatexImage::Unchanged
            };
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let as_is = matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "pdf");
        let file_name = format!("{}.{}", self.copied + 1, if as_is { &ext } else { "png" });
        let target = self.dir.join(&file_name);
        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            self.error = Some(format!("Failed to create {}: {e}", self.dir.display()));
            return LatexImage::Unchanged;
        }
        let written = if as_is {
            std::fs::copy(&path, &target)
                .map(|_| ())
                .map_err(|e| e.to_string())
        } else {
            match ::image::open(&path) {
                Ok(image) => image.save(&target).map_err(|e| e.to_string()),
                Err(::image::ImageError::Decoding(_) | ::image::ImageError::Unsupported(_)) => {
                    if ext == "svg" {
                        self.svgs_as_alt_text += 1;
                    }
                    return LatexImage::AltText;
                }
                Err(e) => Err(e.to_string()),
            }
        };
        match written {
            Ok(()) => {
                self.copied += 1;
                LatexImage::Copied(format!("{}/{file_name}", self.dir_name))
            }
            Err(e) => {
                self.error = Some(format!("Failed to copy {}: {e}", path.display()));
                LatexImage::Unchanged
            }
        }
    }
}

/// Calls `f` with the HTML of every raw HTML block in `blocks`, those in
/// quotes, list items and notes included. Every kind of block is listed, so
/// a new kind of container doesn't compile until it's entered here too.
fn for_each_raw_html(blocks: &mut [Block], f: &mut dyn FnMut(&mut String)) {
    for block in blocks {
        match block {
            Block::Raw { text, .. } => f(text),
            Block::Quote(children)
            | Block::FootnoteDefinition {
                content: children, ..
            } => for_each_raw_html(children, f),
            Block::List(List { items, .. }) => items
                .iter_mut()
                .for_each(|item| for_each_raw_html(&mut item.content, f)),
            Block::Heading { .. }
            | Block::Paragraph(_)
            | Block::Code { .. }
            | Block::Math(_)
            | Block::Table(_)
            | Block::ThematicBreak => {}
        }
    }
}

/// For LaTeX, which can't keep markup: raw HTML tags in text (an inline
/// `<img>`) read into the AST, so their images are copied and converted
/// like any other. What the HTML reader keeps raw (MathML without TeX)
/// stays raw, and so do raw HTML blocks, which the writer reads (see
/// [`copy_images_for_latex`]). Returns how many tags read as no more than
/// text: what they marked up shows as plain text.
fn read_raw_inline_html(blocks: &mut [Block]) -> usize {
    let mut lost = 0;
    for block in blocks {
        lost += match block {
            Block::Heading { content, .. } | Block::Paragraph(content) => read_raw_inlines(content),
            Block::Quote(children)
            | Block::FootnoteDefinition {
                content: children, ..
            } => read_raw_inline_html(children),
            Block::List(List { items, .. }) => items
                .iter_mut()
                .map(|item| read_raw_inline_html(&mut item.content))
                .sum(),
            Block::Table(table) => table
                .header
                .iter_mut()
                .chain(table.rows.iter_mut().flatten())
                .map(read_raw_inlines)
                .sum(),
            _ => 0,
        };
    }
    lost
}

/// [`read_raw_inline_html`] for a run of inlines: each raw tag becomes the inlines
/// it reads as (an `<img>` an image; a lone `<span>` nothing).
fn read_raw_inlines(inlines: &mut Vec<Inline>) -> usize {
    let raw = std::mem::take(inlines);
    let mut lost = 0;
    for (i, inline) in raw.iter().enumerate() {
        let mut inline = inline.clone();
        match &mut inline {
            Inline::Raw { text, .. } => {
                let blocks = read::html::read_fragment(text);
                // Anything that isn't inline (MathML kept raw): leave the tag
                // to the writer.
                if !blocks.iter().all(|b| matches!(b, Block::Paragraph(_))) {
                    inlines.push(inline);
                    continue;
                }
                let content: Vec<Inline> = blocks
                    .into_iter()
                    .flat_map(|block| match block {
                        Block::Paragraph(content) => content,
                        _ => Vec::new(),
                    })
                    .collect();
                // Closing tags and empty anchors mark nothing up.
                if content.iter().all(|c| matches!(c, Inline::Text(_)))
                    && !text.trim_start().starts_with("</")
                    && write::inline_anchor(&raw[i..]).is_none()
                {
                    lost += 1;
                }
                inlines.extend(content);
                continue;
            }
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strikethrough(c)
            | Inline::Superscript(c)
            | Inline::Subscript(c)
            | Inline::Link { content: c, .. } => lost += read_raw_inlines(c),
            _ => {}
        }
        inlines.push(inline);
    }
    lost
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
        assert!(handles("typst", "md") && handles("typst", "pdf"));
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

    #[test]
    fn raw_html_urls_are_rebased_when_output_moves() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pic.png"), b"png").unwrap();
        let input = src.path().join("raw.md");
        std::fs::write(
            &input,
            "<figure><img src=\"pic.png\" alt=\"raw\"></figure>\n\n![md](pic.png)\n\n\
             <details>\n<!-- keep <img src=\"pic.png\"> -->\n<summary>S</summary>\n\n\
             *md* and <a href='pic.png#p'>raw</a>\n\n</details>\n\n\
             <p><a href=\"https://x.org/pic.png\">web</a> <a href=\"#top\">top</a></p>\n",
        )
        .unwrap();
        let pic = url::Url::from_file_path(src.path().join("pic.png")).unwrap();
        for target in ["html", "md"] {
            let out = dst.path().join(format!("raw.{target}"));
            convert(&input, target, &out).unwrap();
            let written = std::fs::read_to_string(&out).unwrap();
            let markdown_image = match target {
                "md" => format!("![md]({pic})"),
                _ => format!("<img src=\"{pic}\" alt=\"md\""),
            };
            for expected in [
                format!("<img src=\"{pic}\" alt=\"raw\">"),
                markdown_image,
                // Inline raw HTML.
                format!("<a href=\"{pic}#p\">"),
                // Split across blocks, with a comment: kept as written.
                "<details>\n<!-- keep <img src=\"pic.png\"> -->\n<summary>S</summary>".into(),
                "</details>".into(),
                "<a href=\"https://x.org/pic.png\">web</a> <a href=\"#top\">top</a>".into(),
            ] {
                assert!(
                    written.contains(&expected),
                    "{expected} in {target}:\n{written}"
                );
            }
        }
    }

    #[test]
    fn raw_text_elements_keep_their_text_across_raw_pieces() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pic.png"), b"png").unwrap();
        let pic = url::Url::from_file_path(src.path().join("pic.png")).unwrap();
        let input = src.path().join("raw.md");
        let out = dst.path().join("raw.html");
        let convert_md = |md: &str| {
            std::fs::write(&input, md).unwrap();
            convert(&input, "html", &out).unwrap();
            std::fs::read_to_string(&out).unwrap()
        };

        // Inline HTML comes tag by tag: the text of a textarea or a script
        // runs to its own end tag, out of an emphasis and into later blocks.
        let kept = "<textarea title=\"</textarea>\"><img src=\"pic.png\"></textarea-x>\
                    <img src=\"pic.png\"></TEXTAREA> \
                    <script>s = '<img src=\"pic.png\">'</script>";
        let html = convert_md(&format!(
            "a {kept} *b <textarea>* <img src=\"pic.png\"></textarea> <img src=\"pic.png\">\n\n\
             c <textarea>\n\n<img src=\"pic.png\">\n\n</textarea>\n\n<img src=\"pic.png\">\n"
        ));
        for expected in [
            kept.to_string(),
            "<textarea></em> <img src=\"pic.png\"></textarea>".into(),
            format!("</textarea> <img src=\"{pic}\">"),
            "<textarea></p>\n<img src=\"pic.png\">\n</textarea>".into(),
            format!("</textarea>\n<img src=\"{pic}\">"),
        ] {
            assert!(html.contains(&expected), "{expected} in:\n{html}");
        }

        // With the first separator taken by the document, another is used.
        let html = convert_md(
            "a <textarea><img src=\"pic.png\"></textarea>\n\n\
             <p title=\"\u{E000}\">marker</p>\n\n<img src=\"pic.png\">\n",
        );
        for expected in [
            "<textarea><img src=\"pic.png\"></textarea>".to_string(),
            "<p title=\"\u{E000}\">marker</p>".into(),
            format!("<img src=\"{pic}\">"),
        ] {
            assert!(html.contains(&expected), "{expected} in:\n{html}");
        }
    }

    #[test]
    fn raw_html_urls_are_decoded_and_escaped() {
        let src = tempfile::tempdir().unwrap();
        for name in ["pic.png", "a&b.png", "\u{a9}.png"] {
            std::fs::write(src.path().join(name), b"png").unwrap();
        }
        let url = |name: &str| url::Url::from_file_path(src.path().join(name)).unwrap();
        let (pic, amp, copy) = (url("pic.png"), url("a&b.png"), url("\u{a9}.png"));
        let amp = amp.as_str().replace('&', "&amp;");
        assert_eq!(
            rebase_raw_html_urls(
                "<IMG SRC=pic.png alt=x ><img src='a&amp;b.png'><img src=\"&copy;.png\">",
                src.path()
            ),
            format!("<IMG SRC=\"{pic}\" alt=x><img src=\"{amp}\"><img src=\"{copy}\">")
        );
        // `&copy;.png` names `©.png`, never a file of that literal name.
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("&copy;.png"), b"png").unwrap();
        let html = "<img src=\"&copy;.png\">";
        assert_eq!(rebase_raw_html_urls(html, other.path()), html);
    }

    #[test]
    fn raw_html_text_and_end_tags_are_left_alone() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pic.png"), b"png").unwrap();
        for html in [
            "<textarea><img src=\"pic.png\"></textarea-x><img src=\"pic.png\"></textarea>",
            "<script><!--<script></script><img src=\"pic.png\">--></script>",
            "<plaintext><img src=\"pic.png\">",
            "<?x <img src=\"pic.png\"> ?>",
            "</a title=\"<img src=pic.png>\">",
            "<img src=\"pic.png\"",
        ] {
            assert_eq!(rebase_raw_html_urls(html, src.path()), html);
        }
        // A custom element named like `<title>` is markup.
        let pic = url::Url::from_file_path(src.path().join("pic.png")).unwrap();
        assert_eq!(
            rebase_raw_html_urls("<title-card><img src=\"pic.png\"></title-card>", src.path()),
            format!("<title-card><img src=\"{pic}\"></title-card>")
        );
    }

    #[test]
    fn relative_input_paths_are_rebased() {
        // Tests run from the crate's folder, so a path below it is relative.
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&target).unwrap();
        let src = tempfile::tempdir_in(&target).unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("pic.png"), b"png").unwrap();
        std::fs::write(src.path().join("doc.md"), "![pic](pic.png)\n").unwrap();
        let relative = Path::new("target")
            .join(src.path().file_name().unwrap())
            .join("doc.md");
        let out = dst.path().join("doc.html");
        convert(&relative, "html", &out).unwrap();
        let pic = url::Url::from_file_path(src.path().join("pic.png")).unwrap();
        let html = std::fs::read_to_string(&out).unwrap();
        assert!(html.contains(&format!("src=\"{pic}\"")), "{html}");

        // A bare file name is in the current folder.
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(input_dir(Path::new("doc.md")), cwd);
        assert!(same_dir(Path::new(""), &cwd));
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
                Ok((pdf, _)) => pdf,
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
        let (pdf, fallback, _) = typeset_with_fallback(&doc, &std::env::temp_dir()).unwrap();
        assert_eq!(page_count(&pdf), 1);
        // mitex turns `\frac{a}` into `frac(a)`, which typst rejects.
        assert_eq!(fallback, HashSet::from([1]), "only the odd formula");
        let doc = read::markdown::read("All good: $x^2$ and $$\\frac{a}{b}$$\n");
        let (_, fallback, _) = typeset_with_fallback(&doc, &std::env::temp_dir()).unwrap();
        assert!(fallback.is_empty());
    }

    /// Formulas the compiler rejects and those mitex can't convert both
    /// show as TeX in the PDF; each counts.
    #[test]
    fn formulas_shown_as_tex_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("math.md");
        let pdf = |markdown: &str| {
            std::fs::write(&input, markdown).unwrap();
            let warnings = convert(&input, "pdf", &input.with_extension("pdf")).unwrap();
            warnings.iter().map(Warning::message).collect::<Vec<_>>()
        };
        // `\frac{a}` the compiler rejects; mitex can't convert `\sideset`
        // once the `\mathclap` its macros use is redefined.
        assert_eq!(
            pdf("Good $x^2$, rejected $\\frac{a}$, and \
                 $\\providecommand{\\mathclap}[1]{#1} \\sideset{_a}{_b}\\sum$.\n"),
            ["2 formulas couldn't be typeset and are shown as TeX"]
        );
        assert_eq!(
            pdf("Rejected $\\frac{a}$.\n"),
            ["1 formula couldn't be typeset and is shown as TeX"]
        );
        assert_eq!(
            pdf("All good: $x^2$ and $$\\frac{a}{b}$$\n"),
            [] as [String; 0]
        );
    }

    /// The typst source for `markdown`, after checking that each of its
    /// formulas is typeset rather than shown as TeX: none fails mitex's
    /// conversion (the writer puts those in `#raw(…)`) and none is rejected
    /// by the compiler (those fall back).
    fn typeset_not_tex(markdown: &str) -> String {
        let doc = read::markdown::read(markdown);
        let options = write::typst::Options {
            target: write::typst::Target::Pdf,
            fonts: typst::text_fonts(None),
            paper: typst::default_paper(),
            base_dir: std::env::temp_dir(),
            tex_fallback: &HashSet::new(),
        };
        let source = write::typst::write(&doc, &options).source;
        assert!(!source.contains("#raw("), "{source}");
        let (_, fallback, _) = typeset_with_fallback(&doc, &std::env::temp_dir()).unwrap();
        assert!(fallback.is_empty(), "{fallback:?}");
        source
    }

    #[test]
    fn mathscr_is_typeset_as_scr_not_tex() {
        let source = typeset_not_tex("A sheaf $\\mathscr{F}$ on $X$.\n");
        assert!(source.contains("scr(F"), "{source}");
    }

    #[test]
    fn every_delimited_matrix_is_typeset_not_tex() {
        typeset_not_tex(
            "$$\\begin{Bmatrix}a&b\\\\c&d\\end{Bmatrix}$$\n\n\
             $$\\begin{Vmatrix}a&b\\\\c&d\\end{Vmatrix}$$\n\n\
             $$\\begin{pmatrix}a\\end{pmatrix} \\begin{bmatrix}a\\end{bmatrix} \
             \\begin{vmatrix}a\\end{vmatrix}$$\n",
        );
    }

    #[test]
    fn laps_are_typeset_not_tex() {
        typeset_not_tex(
            "$a\\mathrlap{x}b$ $a\\mathllap{x}b$ $a\\mathclap{x}b$ \
             $\\rlap{y}$ $\\llap{y}$ $\\clap{y}$\n\n\
             $$\\sum_{\\mathclap{1 \\le i \\le n}} i$$\n",
        );
    }

    #[test]
    fn sideset_is_typeset_not_tex() {
        let source = typeset_not_tex(
            "$\\sideset{_a^b}{_c^d}\\sum$\n\n\
             $$\\sideset{}{'}\\sum_{n<k,\\,n\\text{ odd}} n e_n$$\n",
        );
        assert_eq!(source.matches("sideset(").count(), 2, "{source}");
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

    /// The `/URI` of each link annotation in `pdf`, `None` for a link
    /// within the document.
    fn pdf_links(pdf: &[u8]) -> Vec<Option<String>> {
        let doc = lopdf::Document::load_mem(pdf).unwrap();
        doc.objects
            .values()
            .filter_map(|o| o.as_dict().ok())
            .filter(|d| d.get(b"Subtype").and_then(|s| s.as_name()).ok() == Some(&b"Link"[..]))
            .map(|d| {
                let action = d.get(b"A").and_then(|a| doc.dereference(a)).ok()?.1;
                let uri = action.as_dict().ok()?.get(b"URI").ok()?.as_str().ok()?;
                Some(String::from_utf8_lossy(uri).into_owned())
            })
            .collect()
    }

    #[test]
    fn markdown_anchors_stay_link_destinations_in_typst() {
        // Anchors in text, one quoted otherwise and with an entity, then as
        // an HTML block named like the writer's label for a note cited twice.
        let doc = read::markdown::read(
            "See [the note](#note-1), [again](#note%2D1), [this](#a%26b), \
             [the end](#fn-1) and [elsewhere](#gone).\n\n\
             <a id=\"note-1\"></a>The note.\n\n<a id=\"note-1\"></a>\n\n\
             <a id='a&amp;b'></a>This.\n\nCited[^n] twice[^n].\n\n\
             [^n]: A note.\n\n<a id=\"fn-1\">\n</a>\n",
        );
        let dir = tempfile::tempdir().unwrap();
        let source = typst_file_source(&doc, dir.path());
        for link in [
            "(\"anchor:note-1\"))[the note]",
            "(\"anchor:note-1\"))[again]",
            "(\"anchor:a&b\"))[this]",
            "(\"anchor:fn-1\"))[the end]",
        ] {
            assert!(source.contains(&format!("#link(label{link}")), "{source}");
        }
        // Each id is labelled once, since typst won't link to a label used
        // twice; a link to no anchor keeps its URL.
        assert_eq!(
            source.matches("#metadata(none)#label(").count(),
            3,
            "{source}"
        );
        assert!(source.contains("#link(\"#gone\")"), "{source}");
        // Typst rejects a link to a label that isn't in the document.
        let main = dir.path().join("main.typ");
        std::fs::write(&main, &source).unwrap();
        let typst_file_pdf = typst_file_to_pdf(&main).unwrap();
        let (pdf, _, _) = typeset_with_fallback(&doc, dir.path()).unwrap();
        for pdf in [typst_file_pdf, pdf] {
            let mut links = pdf_links(&pdf);
            links.sort();
            // The four links, the note's two marks and its way back.
            let mut expected = vec![None; 7];
            expected.push(Some("#gone".to_string()));
            assert_eq!(links, expected);
        }
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

    #[test]
    fn raw_html_in_latex_output_keeps_the_main_texts_ids_and_copies_its_images() {
        // The writer reads raw HTML blocks itself, so a heading in one takes
        // no id of the main text's, and their images are copied like the
        // rest, or shown as their alt text.
        let src = tempfile::tempdir().unwrap();
        image::RgbImage::new(2, 2)
            .save(src.path().join("pic.png"))
            .unwrap();
        let md = src.path().join("raw.md");
        std::fs::write(
            &md,
            "<h2>Same</h2>\n\n<p align=\"center\"><img src=\"pic.png\" alt=\"Logo\"></p>\n\n\
             # Same\n\nSee [up](#same).\n\n<img src=\"missing.png\" alt=\"Gone\">\n\n\
             <video src=\"clip.mp4\">\nThe video's fallback text.\n</video>\n",
        )
        .unwrap();
        let out = src.path().join("raw.tex");
        convert(&md, "tex", &out).unwrap();
        let tex = std::fs::read_to_string(&out).unwrap();
        assert!(
            tex.contains("\\label{same-1}")
                && tex.contains("\\section[Same]{\\texorpdfstring{Same}{Same}}\\label{same}")
                && tex.contains("\\hyperref[same]{up}"),
            "{tex}"
        );
        assert!(tex.contains("{raw-images/1.png}"), "{tex}");
        assert!(src.path().join("raw-images/1.png").is_file());
        assert!(tex.contains("{[}Gone{]}"), "{tex}");
        // What the HTML reader keeps of other elements stays.
        assert!(tex.contains("The video's fallback text."), "{tex}");
        assert_latex_compiles(&out);
    }

    /// Our LaTeX compiles, tricky input included: brackets after `\item`
    /// and rows, notes in headings and table cells, code, images and tables
    /// inside notes, deep and mixed nesting (quotes past LaTeX's limit
    /// too), display environments, CJK and a missing image.
    #[test]
    fn our_latex_compiles() {
        let dir = tempfile::tempdir().unwrap();
        image::RgbImage::new(4, 4)
            .save(dir.path().join("pic.png"))
            .unwrap();
        std::fs::write(
            dir.path().join("logo.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        )
        .unwrap();
        let md = dir.path().join("tricky #1.md");
        std::fs::write(&md, TRICKY_MARKDOWN).unwrap();
        let tex = dir.path().join("out").join("tricky #1.tex");
        std::fs::create_dir_all(tex.parent().unwrap()).unwrap();
        convert(&md, "tex", &tex).unwrap();
        // Four local raster images (the SVG, missing and remote ones fall
        // back to alt text), every one a copy, raw HTML ones included.
        let written = std::fs::read_to_string(&tex).unwrap();
        assert_eq!(written.matches("\\includegraphics").count(), 4, "{written}");
        assert!(
            !written.contains("{pic.png}") && !written.contains("logo.svg"),
            "{written}"
        );
        assert_latex_compiles(&tex);
    }

    /// Images a book can't package and links that can't work in it count;
    /// those it keeps don't.
    #[test]
    fn images_and_links_a_book_shows_as_text_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        image::RgbImage::new(4, 4)
            .save(dir.path().join("ok.png"))
            .unwrap();
        let input = dir.path().join("links.md");
        std::fs::write(
            &input,
            "# Start\n\n\
             ![remote](https://e.org/r.png) ![missing](missing.png) ![ok](ok.png)\n\n\
             [js](javascript:x()) [gone](#nope) [other](other.md) \
             [web](https://e.org) [mail](mailto:a@e.org) [start](#start)\n",
        )
        .unwrap();
        let warnings = convert(&input, "epub", &input.with_extension("epub")).unwrap();
        let messages: Vec<String> = warnings.iter().map(Warning::message).collect();
        assert_eq!(
            messages,
            [
                "2 images couldn't be packaged and became their alt text",
                "3 links that can't work in a book kept only their text",
            ]
        );
    }

    /// Scripts and embeds a book leaves out count, one each; what only
    /// styles or stands in for a script doesn't.
    #[test]
    fn content_left_out_of_books_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("embeds.md");
        std::fs::write(
            &input,
            "Text.\n\n<script>alert(1)</script>\n\n\
             <iframe src=\"https://e.org/x\"></iframe>\n\n\
             <p><video src=\"a.mp4\"><source src=\"a.webm\"></video> \
             <audio src=\"a.mp3\"></audio> <object data=\"a.swf\"></object> \
             <embed src=\"a.swf\"></p>\n\n\
             <style>p { color: red }</style>\n\n<template><p>T</p></template>\n\n\
             <noscript>No scripts.</noscript>\n\n<link rel=\"stylesheet\" href=\"a.css\">\n",
        )
        .unwrap();
        let warnings = convert(&input, "epub", &input.with_extension("epub")).unwrap();
        let messages: Vec<String> = warnings.iter().map(Warning::message).collect();
        assert_eq!(messages, ["6 scripts and embeds were left out of the book"]);
        // HTML keeps them all.
        let html = convert(&input, "html", &input.with_extension("html")).unwrap();
        assert_eq!(html, []);
    }

    #[test]
    fn svgs_shown_as_alt_text_in_latex_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\"/>";
        std::fs::write(dir.path().join("a.svg"), svg).unwrap();
        std::fs::write(dir.path().join("b.svg"), svg).unwrap();
        image::RgbImage::new(4, 4)
            .save(dir.path().join("p.png"))
            .unwrap();
        image::RgbImage::new(4, 4)
            .save(dir.path().join("j.jpg"))
            .unwrap();
        let input = dir.path().join("pics.md");
        // One in Markdown, one in raw HTML; PNG and JPEG are included.
        std::fs::write(
            &input,
            "![a](a.svg) ![p](p.png) ![j](j.jpg)\n\n<p><img src=\"b.svg\" alt=\"b\"></p>\n",
        )
        .unwrap();
        let messages = |target: &str| {
            let output = dir.path().join("out").join(format!("pics.{target}"));
            std::fs::create_dir_all(output.parent().unwrap()).unwrap();
            let warnings = convert(&input, target, &output).unwrap();
            warnings.iter().map(Warning::message).collect::<Vec<_>>()
        };
        assert_eq!(messages("tex"), ["2 SVG images became their alt text"]);
        for target in ["md", "html", "txt", "typst", "pdf", "epub"] {
            let messages = messages(target);
            assert!(
                !messages.iter().any(|m| m.contains("SVG")),
                "{target}: {messages:?}"
            );
        }
    }

    /// Regression for #67: quotes nested past LaTeX's six lists compile,
    /// whatever they open with (text starting with `plus` or `minus`,
    /// which TeX would read on into a length before it, included) and
    /// whatever they hold, in notes and list items too, and so do lists
    /// of alternating kinds nested more than six deep.
    #[test]
    fn quotes_nested_past_the_latex_list_limit_compile() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("deep.md");
        std::fs::write(
            &md,
            "> > > > > > plus six\n> > > > > >\n\
             > > > > > > > Plus seven, really\n> > > > > > >\n\
             > > > > > > > ```\n> > > > > > > code\n> > > > > > > ```\n> > > > > > >\n\
             > > > > > > > ```\n> > > > > > > \\end{verbatim}\n> > > > > > > ```\n> > > > > > >\n\
             > > > > > > > ---\n> > > > > > >\n> > > > > > > - item\n> > > > > > >\n\
             > > > > > > > > > > minus ten[^n]\n> > > > > > >\n> > > > > > > back to seven\n\n\
             - > > > > > > > minus first in an item\n- next\n\n\
             - a\n  1. b\n     - c\n       1. d\n          - e\n            1. f\n\
             \x20              - g\n                 1. h\n                    > plus in lists\n\n\
             [^n]: > > > > > > > plus in a note\n",
        )
        .unwrap();
        let tex = dir.path().join("deep.tex");
        convert(&md, "tex", &tex).unwrap();
        assert_latex_compiles(&tex);
    }

    /// Regression for #67: however deep quotes nest, the line keeps room
    /// for text, rules and images. Thirty levels once left a line of
    /// -340pt, which set text a word at a time past the margin, dropped
    /// rules and drew images mirrored. Levels past LaTeX's six still
    /// indent one by one, as typst's do, through the tenth, which leaves
    /// a line just under 10em. A probe at each level's text stops the run
    /// if its line is narrower than 9em, or if it isn't indented past its
    /// parent.
    #[test]
    fn quotes_nested_thirty_deep_keep_a_readable_line() {
        const DEEPEST: usize = 30;
        let dir = tempfile::tempdir().unwrap();
        image::RgbImage::new(40, 10)
            .save(dir.path().join("pic.png"))
            .unwrap();
        let mut md: String = (1..=DEEPEST)
            .map(|depth| format!("{}Probe{depth} at depth {depth}.\n", "> ".repeat(depth)))
            .collect();
        let deepest = "> ".repeat(DEEPEST);
        md.push_str(&format!(
            "{deepest}\n{deepest}---\n{deepest}\n{deepest}A ![](pic.png) B\n{deepest}\n\
             {deepest}- Probe{} in a list\n",
            DEEPEST + 1
        ));
        let input = dir.path().join("deep.md");
        std::fs::write(&input, md).unwrap();
        let tex = dir.path().join("deep.tex");
        convert(&input, "tex", &tex).unwrap();

        let probe = r"\makeatletter
\newdimen\probeindent
\def\probe#1{%
  \ifdim\linewidth<9em \errmessage{depth #1 leaves a \the\linewidth\space line}\fi
  \ifnum#1<11 \ifdim\@totalleftmargin>\probeindent\else
    \errmessage{depth #1 isn't indented past its parent}\fi\fi
  \global\probeindent\@totalleftmargin}
\makeatother
\begin{document}";
        let written = std::fs::read_to_string(&tex).unwrap();
        let mut written = written.replacen("\\begin{document}", probe, 1);
        for depth in 1..=DEEPEST + 1 {
            written = written.replace(&format!("Probe{depth} "), &format!("\\probe{{{depth}}}"));
        }
        assert_eq!(
            written.matches("\\probe{").count(),
            DEEPEST + 1,
            "{written}"
        );
        std::fs::write(&tex, written).unwrap();
        assert_latex_compiles(&tex);
    }

    /// The LaTeX of typst math compiles: `:=`, symbols typed as Unicode
    /// (next to letters, and degrees and primes after a script), brace
    /// annotations, alignment points in a single row (nested ones too),
    /// inline rows, rows and columns between delimiters and in operands,
    /// and empty equations.
    #[test]
    fn typst_math_latex_compiles() {
        let dir = tempfile::tempdir().unwrap();
        let typ = dir.path().join("math.typ");
        std::fs::write(
            &typ,
            "Colon $a := b$, $y =: g$, $a ::= b$ and $a :=^\"def\" b$, \
             symbols $α + β → γ$, $∀ x ∈ ℝ$, $∀中$ and $→é$, \
             $x^2°$ and $x_1^2′$, \
             $underbrace(x + y, \"sum\")$, $overbrace(a, b)$, \
             $underbrace(x, a & b)$ and $underbrace(x, a \\ b)$.\n\n\
             Aligned $a &= b$, rows $x &= y \\ z &= w$ and $[| a &= b |]$.\n\n\
             Nested $(a &= b)$, $lr((a &= b))$, $sqrt(a &= b)$, $x_(a &= b)$, \
             $frac(a &= b, c)$, $bold(a &= b)$, $abs(a &= b)$ and $sin(a &= b)$.\n\n\
             $ a &= b $\n\n$ [| a \\ b |] $\n\n\
             $ lr((a &= b \\ c &= d)) $\n\n$ cases((x & y), z) $\n\n\
             $ mat((a & b), c) $\n\n$ (a \\ b) + [| c \\ d |] $\n\n\
             $ x_(a \\ b) + [| c \\ d |] $\n\n$ x &= sqrt(a &= b) \\ y &= 2 $\n\n\
             $ [| a \\ b |]^2 + ⌈ c \\ d ⌉ $\n\n$ (a \\ b)/c + [| d \\ e |] $\n\n\
             Empty $&$ and $display()$.\n\n$ & $\n\n\
             Brackets $[0, 1[ \\ x$, $[a \\ b) + c$, $sqrt([0, 1[ \\ x)$, $a \\ [b \\ c]$, \
             $mat(a; [0, 1[ \\ b)$, $cases(a, [b \\ c])$ and \
             $x_(a \\ [0, 1[)$.#footnote[Noted $[0, 1[ \\ x$ and [1].]\n\n\
             $ [0, 1[ \\ x $\n\n$ a &= b \\ [c &= d] $\n\n\
             Operators $op(a \\ b)$, $op([| a \\ b |])$, $op(hat(x))$, $op(x_1)$ and \
             $op(\"sin\")$.\n\n\
             Columns $cases(x &= (a & b))$, $cases(a & b & c, d)$ and \
             $mat(a & b & c & d & e & f, g & h & i & j & k & l)$.\n\n\
             Escapes $a \\^ b \\~ c \\\\ d$, $a \\\\ [b]$, $( a \"x\\\\y^\" b )$ and \
             $op(\"x\\\\y^\")$.\n",
        )
        .unwrap();
        let tex = dir.path().join("math.tex");
        convert(&typ, "tex", &tex).unwrap();
        assert_latex_compiles(&tex);
    }

    /// Math symbols typed as Unicode get TeX names, which LaTeX's math
    /// fonts have glyphs for, rather than passing through as characters
    /// they don't.
    #[test]
    fn typst_math_symbols_have_latex_glyphs() {
        let dir = tempfile::tempdir().unwrap();
        let typ = dir.path().join("symbols.typ");
        std::fs::write(
            &typ,
            "Greek $α + β → γ$ and $µ + ∆x$, sets $∀ x ∈ ℝ$, $A ⊄ B ⊈ C$ and \
             $ℙ(𝔼) ⊑ ℒ$, relations $a ≢ b ≰ c ∤ d ∦ e$ and $a ⊲ b ≜ c ⊣ d$, \
             arrows $a ↛ b ⇏ c ⇀ d$, and $a ::= b$. \
             CJK input $a ≦ b ≧ c ≒ d$, sets $a ∊ B ∍ c$ and $𝕂 ⊂ 𝒪 ∪ 𝔄 ∪ 𝐅$, \
             letters $ħ + ı + ð + ϝ + ℶ$, $Α Β Μ ο$, scripts $x₁₂ + y² + z^2²$, \
             arrows $a ⇋ b ↷ c ⇢ d ↺ e$, operators $a ⊘ b ⊞ c ∙ d ≀ e$, \
             shapes $▷ ★ ✓ ■ ○$ and $⟪ a ⟫$.\n",
        )
        .unwrap();
        let tex = dir.path().join("symbols.tex");
        convert(&typ, "tex", &tex).unwrap();
        // Every character has a name, so the math is ASCII, as pdfLaTeX
        // needs it.
        let written = std::fs::read_to_string(&tex).unwrap();
        let body = &written[written.find("\\begin{document}").unwrap()..];
        assert!(body.is_ascii(), "{body}");
        if let Some(log) = assert_latex_compiles(&tex) {
            assert!(!log.contains("Missing character"), "{log}");
        }
    }

    /// `:=` from typst is MathML's one `≔`, not a `:` and an `=` drawn
    /// apart, and the LaTeX preamble defines the commands for it that
    /// amsmath lacks, written in Markdown too.
    #[test]
    fn colon_equals_is_one_symbol_in_html_and_compiles_in_latex() {
        let dir = tempfile::tempdir().unwrap();
        let typ = dir.path().join("colon.typ");
        std::fs::write(&typ, "Colon $a := b$ and $y =: g$.\n").unwrap();
        let html = dir.path().join("colon.html");
        convert(&typ, "html", &html).unwrap();
        let html = std::fs::read_to_string(html).unwrap();
        assert!(
            html.contains("<mo>≔</mo>") && html.contains("<mo>≕</mo>"),
            "{html}"
        );
        assert!(!html.contains("<code class=\"math\">"), "{html}");

        let md = dir.path().join("colon.md");
        std::fs::write(&md, "$a \\coloneqq b \\eqqcolon c \\Coloneqq d$\n").unwrap();
        let tex = dir.path().join("colon.tex");
        convert(&md, "tex", &tex).unwrap();
        assert_latex_compiles(&tex);
    }

    /// An equation with nothing in it (only an alignment point) writes no
    /// empty `$$`, which Markdown would read back as display math around
    /// the text after it, or as two literal `$$` paragraphs.
    #[test]
    fn empty_typst_equations_write_no_empty_markdown_math() {
        let dir = tempfile::tempdir().unwrap();
        let typ = dir.path().join("empty.typ");
        std::fs::write(&typ, "Inline lone $&$ and $&&$ end.\n\n$ & $\n\nAfter.\n").unwrap();
        let md = dir.path().join("empty.md");
        convert(&typ, "md", &md).unwrap();
        let md = std::fs::read_to_string(md).unwrap();
        assert!(!md.contains('$'), "{md}");
        assert!(md.contains("Inline lone") && md.contains("After."), "{md}");
    }

    /// Regression for #58: typst's `:=` is `\coloneqq` in LaTeX, which the
    /// preamble defines without mathtools for a document that uses it
    /// (written in Markdown too). A formula that defines it itself keeps
    /// its own definition, and the others the preamble's.
    #[test]
    fn colon_equals_is_defined_for_latex_beside_the_documents_own() {
        let dir = tempfile::tempdir().unwrap();
        let definition = COLONEQQ_FALLBACK;
        let (tex, text) = converted(dir.path(), "colon.typ", "Colon $a := b$.\n", "tex");
        assert!(text.contains("Colon \\(a \\coloneqq b\\)."), "{text}");
        assert!(text.contains(definition), "{text}");
        assert_latex_compiles(&tex);

        let source = "Define $\\newcommand{\\coloneqq}{\\mathrel{:=}} a \\coloneqq b$, \
                      then $c \\coloneqq d$ and $e ≔ f$.\n";
        let (tex, text) = converted(dir.path(), "own.md", source, "tex");
        assert!(text.contains(definition), "{text}");
        assert!(text.contains("\\(\\renewcommand{\\coloneqq}"), "{text}");
        assert_latex_compiles(&tex);

        let (_, text) = converted(dir.path(), "none.md", "No colon $a = b$.\n", "tex");
        assert!(!text.contains("coloneqq"), "{text}");
    }

    /// Regression for #65: math characters typed as Unicode keep their
    /// glyphs wherever they're shown as typed. HTML draws `μ` (not the
    /// micro sign pulldown-latex draws `\mu` as), `≇` (not `\ncong`'s `≆`),
    /// a bold `Α`, a baseline `…` and a centred `∗`, and plain text keeps
    /// them. LaTeX, whose fonts lack them, gets their names, for Markdown
    /// input too.
    #[test]
    fn unicode_math_characters_keep_their_glyphs_and_get_latex_names() {
        let dir = tempfile::tempdir().unwrap();
        let source = "Typed $α + β → γ$, $μ ≇ ν$, $bold(Α)$, $x_1 … x_n$ and $a ∗ b$.\n";
        let (_, html) = converted(dir.path(), "chars.typ", source, "html");
        for drawn in [
            "<mi>μ</mi>",
            "<mo>≇</mo>",
            "<mi>𝚨</mi>",
            "<mi>…</mi>",
            "<mo>∗</mo>",
        ] {
            assert!(html.contains(drawn), "{drawn} in {html}");
        }
        let (_, txt) = converted(dir.path(), "chars.typ", source, "txt");
        assert!(txt.contains("α + β → γ"), "{txt}");
        let (_, tex) = converted(dir.path(), "chars.typ", source, "tex");
        for named in [
            "\\(\\alpha + \\beta \\to \\gamma\\)",
            "\\(\\mu \\ncong \\nu\\)",
            "\\(\\mathbf{\\Alpha}\\)",
            "\\(x_{1} \\ldots x_{n}\\)",
            "\\(a \\ast b\\)",
        ] {
            assert!(tex.contains(named), "{named} in {tex}");
        }
        let (_, tex) = converted(dir.path(), "chars.md", "Typed $α → ℝ$.\n", "tex");
        assert!(tex.contains("\\(\\alpha \\to \\mathbb{R}\\)"), "{tex}");
    }

    /// Regression for #48 and #62: rows that typst stacks render as MathML
    /// in HTML, not as the TeX code shown when pulldown-latex rejects it,
    /// whatever the rows start with (a bracket, which `aligned` and `\\`
    /// would read as an optional argument) and wherever they are (between
    /// delimiters, or in an operator name).
    #[test]
    fn typst_math_rows_render_as_mathml() {
        let dir = tempfile::tempdir().unwrap();
        let source = "$ [| a \\ b |] $\n\nInterval $[0, 1[ \\ x$, $a \\ [b \\ c]$ and \
                      $op(a \\ b)$.\n";
        let (_, html) = converted(dir.path(), "rows.typ", source, "html");
        assert!(!html.contains("<code class=\"math\">"), "{html}");
        // Rows in text that end on the baseline have a second table, of
        // those above their last.
        let tables = html.matches("<mtable").count();
        let on_baseline = html.matches("<mover accent=\"true\" class=\"last-row-on-baseline\">");
        assert_eq!(tables - on_baseline.count(), 4, "{html}");
    }

    /// typst sets the rows of an equation in text with the last on the
    /// baseline, as LaTeX does with `aligned`'s `[b]`, which pulldown-latex
    /// and mitex don't take: the HTML and the PDF leave it out, not show it.
    #[test]
    fn typst_rows_in_text_take_aligned_b_where_tex_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let source = "Rows $a \\ b$ in text.\n";
        let rows = "\\begin{aligned}[b]&{a } \\\\ &{ b}\\end{aligned}";
        let (_, tex) = converted(dir.path(), "rows.typ", source, "tex");
        assert!(tex.contains(rows), "{tex}");
        let (_, markdown) = converted(dir.path(), "rows.typ", source, "md");
        assert!(markdown.contains(rows), "{markdown}");
        let (_, html) = converted(dir.path(), "rows.typ", source, "html");
        assert!(html.contains("<mtable"), "{html}");
        assert!(!html.contains(">[</mo>"), "{html}");
        // The annotation keeps it for the way back.
        assert!(html.contains("aligned}[b]&amp;"), "{html}");
        let typst_source = typeset_not_tex(&markdown);
        assert!(typst_source.contains("aligned("), "{typst_source}");
        assert!(!typst_source.contains("[b"), "{typst_source}");
        let (_, typ) = converted(dir.path(), "rows.md", &markdown, "typst");
        assert!(typ.contains("#mi(") && !typ.contains("[b"), "{typ}");
    }

    /// Regression for #75: typst numbers a note inside a note right after
    /// it, so B, C and E are notes 1, 2 and 3, in every output.
    #[test]
    fn typst_notes_inside_notes_number_as_typst_numbers_them() {
        let dir = tempfile::tempdir().unwrap();
        let source = "A#footnote[B#footnote[C]] D#footnote[E]\n";
        let (_, markdown) = converted(dir.path(), "notes.typ", source, "md");
        for written in ["A[^1] D[^3]", "[^1]: B[^2]", "[^2]: C", "[^3]: E"] {
            assert!(markdown.contains(written), "{written} in {markdown}");
        }
        // Note `n`'s text in HTML or XHTML, up to its first tag.
        let note = |page: &str, n: usize| -> String {
            let start = page
                .find(&format!(" id=\"fn-{n}\""))
                .unwrap_or_else(|| panic!("no note {n} in {page}"));
            let text = page[start..].split_once("<p>").unwrap().1;
            text[..text.find('<').unwrap()].to_string()
        };
        let (_, html) = converted(dir.path(), "notes.typ", source, "html");
        let input = dir.path().join("notes.typ");
        let epub = input.with_extension("epub");
        convert(&input, "epub", &epub).unwrap();
        let mut book = zip::ZipArchive::new(std::fs::File::open(&epub).unwrap()).unwrap();
        let mut chapter = String::new();
        std::io::Read::read_to_string(
            &mut book.by_name("OEBPS/text/ch001.xhtml").unwrap(),
            &mut chapter,
        )
        .unwrap();
        for page in [html, chapter] {
            let notes: Vec<_> = (1..=3).map(|n| note(&page, n)).collect();
            assert_eq!(notes, ["B", "C", "E"], "{page}");
            // D's reference, the second in the text, is to note 3.
            let after_d = &page[page.find(" D").unwrap()..];
            assert_eq!(after_d.find("#fn-"), after_d.find("#fn-3"), "{page}");
        }
    }

    /// Regression for #65 and #49: typed large operators take typst's
    /// limits in HTML and their names in LaTeX, and a string keeps the
    /// space typst sets beside it in both.
    #[test]
    fn typst_operators_and_strings_are_spaced_as_typst_sets_them() {
        let dir = tempfile::tempdir().unwrap();
        let source = "$ ∑_(i=1)^n i + ∫_0^1 f + ⋃_(k ∈ ℕ) A_k $\n\n\
                      $ f(x) = cases(1 & \"if\" x > 0, 0 & \"otherwise\") $\n\n\
                      $ a quad \"so\" b & \"if\" c \\ bold(\"or\") d & e $\n";
        let (_, html) = converted(dir.path(), "ops.typ", source, "html");
        for drawn in [
            "<munderover><mo movablelimits=\"false\">∑</mo>",
            "<msubsup><mo movablelimits=\"false\">∫</mo>",
            "<munder><mo movablelimits=\"false\">⋃</mo>",
            // The space before an alignment point is the item's before it.
            "<mn>1</mn><mspace width=\"0.333em\" /></mtd><mtd><mtext>if&nbsp;</mtext>",
            "<mtext>so&nbsp;</mtext>",
        ] {
            assert!(html.contains(drawn), "{drawn} in {html}");
        }
        let (tex, text) = converted(dir.path(), "ops.typ", source, "tex");
        for named in [
            "\\sum_{i=1}^{n}",
            "\\int_{0}^{1}",
            "\\bigcup_{k \\in \\mathbb{N}}",
            "1\\hspace{0.333em} & \\text{if } x",
            "a \\quad \\text{so } b\\hspace{0.333em} & \\text{if } c",
            "\\mathbf{\\text{or }} d",
        ] {
            assert!(text.contains(named), "{named} in {text}");
        }
        assert_latex_compiles(&tex);
    }

    /// Regression for #65: degrees, primes and script characters with a
    /// script after them compile in LaTeX, from typst and Markdown alike.
    #[test]
    fn script_characters_with_scripts_compile_in_latex() {
        let dir = tempfile::tempdir().unwrap();
        let source = "Raised $x°^2$, $a²^3$, $x°_1$ and $y′^2$.\n";
        let (tex, text) = converted(dir.path(), "deg.typ", source, "tex");
        assert!(text.contains("\\(x{{}^{\\circ}}^{2}\\)"), "{text}");
        assert_latex_compiles(&tex);
        let source = "Raised $x°^2$ $a²^3$ $b³^2$ $c¹^2$ $d⁰^2$ $e⁴^2$ $f⁹^2$ $g⁺^2$ \
                      $h⁻^2$ $i⁼^2$ $j⁽^2$ $k⁾^2$ $l ⁱ^2$ $m°'$ $n₁^2$.\n";
        let (tex, _) = converted(dir.path(), "deg-md.md", source, "tex");
        assert_latex_compiles(&tex);
    }

    /// Regression for #65: a text argument without braces and `\verb`'s
    /// are text, kept as they are, and a name of several tokens is a whole
    /// argument, which compiles.
    #[test]
    fn unbraced_and_verbatim_text_in_math_compiles_in_latex() {
        let dir = tempfile::tempdir().unwrap();
        let source = "Text $\\text é$, $\\mbox é$, $\\text*é$, $\\text\u{A0}é$ and \
                      $\\verb|α|$; $\\hat é + \\mathrm é + \\vec ℝ + \\frac 1 ℝ$.\n\n\
                      $$x = 1 \\tag é$$\n";
        let (tex, text) = converted(dir.path(), "text.md", source, "tex");
        for written in [
            "Text \\(\\text{é}\\), \\(\\mbox{é}\\), \\(\\text*\\text{é}\\)",
            "\\(\\verb|α|\\)",
            "\\hat {\\text{é}} + \\mathrm {\\text{é}} + \\vec {\\mathbb{R}} + \\frac 1 {\\mathbb{R}}",
            "\\tag{é}",
        ] {
            assert!(text.contains(written), "{written} in {text}");
        }
        assert_latex_compiles(&tex);
    }

    /// Regression for #65: amssymb's negated relations, and symbols and
    /// characters with a variation selector, get LaTeX names, from typst
    /// and Markdown alike.
    #[test]
    fn negated_relations_and_variation_selectors_get_latex_names() {
        let dir = tempfile::tempdir().unwrap();
        let negated = "Negated $≨ ⪇ ⋦ ⪉ ⪷ ⪹ ⪵ ⋨ ⋠ ≭ ⪕ ≂ ≴ ⋢ ≄$ and $a ↔\u{FE0E} b$";
        for (name, source) in [
            (
                "negated.typ",
                format!("{negated}, $arrow.r.hook arrow.tr union.serif$.\n"),
            ),
            ("negated.md", format!("{negated}, $↪\u{FE0E} ∪\u{FE00}$.\n")),
        ] {
            let (tex, text) = converted(dir.path(), name, &source, "tex");
            let body = &text[text.find("\\begin{document}").unwrap()..];
            assert!(body.is_ascii(), "{body}");
            assert!(
                body.contains("\\(a \\leftrightarrow b\\)")
                    && body.contains("\\hookrightarrow")
                    && !body.contains("\\text{")
                    && !body.contains("\\operatorname"),
                "{body}"
            );
            if let Some(log) = assert_latex_compiles(&tex) {
                assert!(!log.contains("Missing character"), "{log}");
            }
        }
    }

    /// Regression for #58: the math inside a text argument gets the
    /// fallbacks' definitions too, a formula's own `\providecommand` holds
    /// in its formula, and pandoc, reading the LaTeX back, keeps the name.
    #[test]
    fn colon_equals_is_defined_for_nested_math_and_provided_commands() {
        let dir = tempfile::tempdir().unwrap();
        let source = "$$a = b \\quad \\text{where $c \\coloneqq d$}$$\n\n\
                      $$\\begin{align} a &= b \\intertext{where $c ≔ d$} e &= f \\end{align}$$\n";
        let (tex, text) = converted(dir.path(), "nested.md", source, "tex");
        assert!(text.contains(COLONEQQ_FALLBACK), "{text}");
        assert!(
            text.contains("\\intertext{where $c \\coloneqq d$}"),
            "{text}"
        );
        assert_latex_compiles(&tex);

        // The formula's own, and one it has made before, hold in it.
        let source = "$\\providecommand{\\coloneqq}{\\mathrel{\\equiv}} a \\coloneqq b$, \
                      $c \\coloneqq d$ and \
                      $\\def\\coloneqq{\\mathrel{\\sim}}\\providecommand{\\coloneqq}{=} e \\coloneqq f$\n";
        let (tex, text) = converted(dir.path(), "provided.md", source, "tex");
        assert!(text.contains(COLONEQQ_FALLBACK), "{text}");
        assert!(
            text.contains("\\(\\renewcommand{\\coloneqq}{\\mathrel{\\equiv}}")
                && text.contains("\\providecommand{\\coloneqq}{=}"),
            "{text}"
        );
        assert_latex_compiles(&tex);
        if let Some(shown) = pdf_text(&tex.with_extension("pdf")) {
            let shown: String = shown.split_whitespace().collect();
            assert!(shown.contains("a≡b,c:=dande∼f"), "{shown}");
        }
        if let Some(back) = pandoc_read_back(&tex, &["-t", "markdown", "--wrap=none"]) {
            assert!(
                back.contains("$\\renewcommand{\\coloneqq}{\\mathrel{\\equiv}} a \\coloneqq b$")
                    && back.contains("$c \\coloneqq d$"),
                "{back}"
            );
        }

        // In a heading, whose short form LaTeX writes to the table of
        // contents, the definition is written out as it is.
        let source = "# New $\\newcommand{\\coloneqq}{\\mathrel{\\equiv}} a \\coloneqq b$\n\n\
                      Body.\n\n# Plain $c \\coloneqq d$\n";
        let (tex, text) = converted(dir.path(), "heading.md", source, "tex");
        assert!(
            text.contains(
                "\\section[New \\texorpdfstring{\\(\\protect\\renewcommand\
                 {\\protect\\coloneqq}{\\protect\\mathrel{\\protect\\equiv}} \
                 a \\protect\\coloneqq b\\)}"
            ) && text.contains("\\section[Plain \\texorpdfstring{\\(c \\coloneqq d\\)}"),
            "{text}"
        );
        assert_latex_compiles(&tex);
    }

    /// The text of `pdf`, as pdftotext reads it. `None` where pdftotext
    /// isn't installed, or there's no PDF (tectonic wasn't run).
    fn pdf_text(pdf: &Path) -> Option<String> {
        let pdftotext = which::which("pdftotext").ok()?;
        if !pdf.is_file() {
            return None;
        }
        let output = std::process::Command::new(pdftotext)
            .arg(pdf)
            .arg("-")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// What pandoc writes when it reads `tex`, given the writer's
    /// `options` (`-t markdown`). `None` where pandoc isn't installed.
    fn pandoc_read_back(tex: &Path, options: &[&str]) -> Option<String> {
        let pandoc = crate::converter::document::binaries::find_pandoc_binary()?;
        let output = std::process::Command::new(pandoc)
            .args(["-f", "latex"])
            .args(options)
            .arg(tex)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// How the LaTeX preamble defines `\coloneqq` where no package has.
    const COLONEQQ_FALLBACK: &str = "\\ifdefined\\coloneqq\\else\\def\\coloneqq{\\mathrel{:=}}\\fi";

    /// Converts `source`, written to `dir/name`, to `target` next to it:
    /// the output's path and text.
    fn converted(dir: &Path, name: &str, source: &str, target: &str) -> (PathBuf, String) {
        let input = dir.join(name);
        std::fs::write(&input, source).unwrap();
        let ext = if target == "typst" { "typ" } else { target };
        let output = input.with_extension(ext);
        convert(&input, target, &output).unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        (output, text)
    }

    #[test]
    fn clean_conversions_warn_of_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("clean.md");
        std::fs::write(&input, "# Title\n\nPlain *text* and $x^2$.\n").unwrap();
        for target in ["md", "txt", "html", "typst", "tex", "epub", "pdf"] {
            let output = input.with_extension(target);
            assert_eq!(convert(&input, target, &output).unwrap(), [], "{target}");
        }
    }

    #[test]
    fn raw_html_shown_as_text_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("raw.md");
        // In text, in bold, a block read as MathML kept raw, in a note; not
        // an anchor or a note nothing cites (not written).
        std::fs::write(
            &input,
            "Some <span style=\"color:red\">red</span> and **<kbd>Ctrl</kbd>**.[^n]\n\n\
             <math>\n<mi>x</mi>\n</math>\n\n<a id=\"here\"></a>Here.\n\n\
             [^n]: Press <kbd>N</kbd>.\n\n[^u]: <kbd>U</kbd>\n",
        )
        .unwrap();
        for target in ["txt", "typst", "pdf", "tex"] {
            let output = input.with_extension(target);
            let warnings = convert(&input, target, &output).unwrap();
            let messages: Vec<String> = warnings.iter().map(Warning::message).collect();
            assert_eq!(
                messages,
                ["4 raw HTML elements kept only their text"],
                "{target}"
            );
        }
        for target in ["md", "html", "epub"] {
            let output = dir.path().join(format!("out.{target}"));
            assert_eq!(convert(&input, target, &output).unwrap(), [], "{target}");
        }
    }

    /// Raw blocks the writers read as a heading, a table or a paragraph
    /// keep their structure: nothing to warn of.
    #[test]
    fn raw_html_read_as_structure_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("structure.md");
        std::fs::write(
            &input,
            "<h2>Title</h2>\n\n<table><tr><td>a</td><td>b</td></tr></table>\n\n\
             <div>Boxed <b>bold</b></div>\n",
        )
        .unwrap();
        for target in ["txt", "typst", "pdf", "tex"] {
            let output = input.with_extension(target);
            assert_eq!(convert(&input, target, &output).unwrap(), [], "{target}");
        }
    }

    #[test]
    fn warnings_say_how_many_and_where() {
        let warning = |count, line| Warning {
            one: "SVG image became its alt text",
            many: "SVG images became their alt text",
            count,
            line,
        };
        assert_eq!(
            warning(1, None).message(),
            "1 SVG image became its alt text"
        );
        assert_eq!(
            warning(3, None).message(),
            "3 SVG images became their alt text"
        );
        assert_eq!(
            warning(1, Some(12)).message(),
            "1 SVG image became its alt text at line 12"
        );
    }

    /// What epubcheck reports of our books that's accepted for now, by its
    /// message ID, each with why. Each is to be fixed, or its reason made
    /// for good (#79).
    const EPUBCHECK_ACCEPTED: &[(&str, &str)] = &[];

    /// Our books pass epubcheck (the EPUB 3 specification's checker): the
    /// fixtures, a Typst source among them, and a book with display math,
    /// an image converted to PNG, raw HTML, a note and a cover. Skipped
    /// where epubcheck isn't installed.
    #[test]
    fn books_pass_epubcheck() {
        let dir = tempfile::tempdir().unwrap();
        image::RgbImage::new(8, 8)
            .save(dir.path().join("cover.png"))
            .unwrap();
        image::RgbImage::new(8, 8)
            .save(dir.path().join("chart.bmp"))
            .unwrap();
        let extras = dir.path().join("extras.md");
        std::fs::write(
            &extras,
            "---\ntitle: Extras\ncover-image: cover.png\n---\n\n# Extras\n\n\
             $$\\int_0^1 x^2\\,dx = \\frac{1}{3}$$\n\n\
             A chart: ![chart](chart.bmp)\n\n\
             <div class=\"note\"><p>Raw <b>HTML</b>.</p></div>\n\n\
             <details><summary>More</summary>Hidden text.</details>\n\n\
             A sentence with a note.[^n]\n\n[^n]: The note.\n",
        )
        .unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs");
        let mut inputs: Vec<PathBuf> = std::fs::read_dir(&fixtures)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        inputs.sort();
        inputs.push(extras);
        // The books are written either way; only checking them needs Java.
        let books: Vec<(String, PathBuf)> = inputs
            .iter()
            .map(|input| {
                let name = input.file_stem().unwrap().to_string_lossy().into_owned();
                let book = dir.path().join(format!("{name}.epub"));
                convert(input, "epub", &book).unwrap_or_else(|e| panic!("{name}: {e}"));
                (name, book)
            })
            .collect();
        let Ok(epubcheck) = which::which("epubcheck") else {
            // CI installs it, so there a pass means the books were checked.
            assert!(
                std::env::var("CI").as_deref() != Ok("true"),
                "epubcheck isn't installed, and CI checks the books with it"
            );
            eprintln!("epubcheck isn't installed: skipped");
            return;
        };

        let mut reported = Vec::new();
        for (name, book) in &books {
            let json = dir.path().join(format!("{name}.json"));
            let run = std::process::Command::new(&epubcheck)
                .arg(book)
                .arg("--json")
                .arg(&json)
                .output()
                .unwrap();
            let json = std::fs::read(&json)
                .unwrap_or_else(|_| panic!("{name}: {}", String::from_utf8_lossy(&run.stderr)));
            let (messages, findings) =
                epubcheck_findings(name, &json).unwrap_or_else(|e| panic!("{e}"));
            println!("epubcheck: {name}: {messages} messages");
            reported.extend(findings);
        }
        println!("epubcheck checked {} books", books.len());
        assert!(reported.is_empty(), "{}", reported.join("\n"));
    }

    /// What epubcheck's JSON report on the book `name` says: how many
    /// messages it holds, and its FATAL, ERROR and WARNING ones not
    /// [`EPUBCHECK_ACCEPTED`]. A report with no checker or messages is no
    /// report (epubcheck failed, or its format changed): an error, so a
    /// check that read nothing can't pass.
    fn epubcheck_findings(name: &str, json: &[u8]) -> Result<(usize, Vec<String>), String> {
        let report: serde_json::Value =
            serde_json::from_slice(json).map_err(|e| format!("{name}: not JSON: {e}"))?;
        if report.get("checker").is_none() {
            return Err(format!("{name}: the report names no checker"));
        }
        let messages = report["messages"]
            .as_array()
            .ok_or_else(|| format!("{name}: the report holds no messages"))?;
        let findings = messages
            .iter()
            .filter(|message| {
                let id = message["ID"].as_str().unwrap_or("");
                let severity = message["severity"].as_str().unwrap_or("");
                matches!(severity, "FATAL" | "ERROR" | "WARNING")
                    && !EPUBCHECK_ACCEPTED
                        .iter()
                        .any(|(accepted, _)| *accepted == id)
            })
            .map(|message| {
                let at = &message["locations"][0];
                format!(
                    "{name}: {} {} {} ({}:{})",
                    message["severity"].as_str().unwrap_or(""),
                    message["ID"].as_str().unwrap_or(""),
                    message["message"].as_str().unwrap_or(""),
                    at["path"].as_str().unwrap_or("?"),
                    at["line"]
                )
            })
            .collect();
        Ok((messages.len(), findings))
    }

    #[test]
    fn epubcheck_reports_must_name_their_checker_and_messages() {
        for broken in ["not JSON", r#"{"messages": []}"#, r#"{"checker": {}}"#] {
            assert!(
                epubcheck_findings("b", broken.as_bytes()).is_err(),
                "{broken}"
            );
        }
        // FATAL, ERROR and WARNING count; USAGE and INFO don't.
        let report = r#"{"checker": {}, "messages": [
            {"ID": "PKG-008", "severity": "FATAL", "message": "Unreadable",
             "locations": [{"path": "b.epub", "line": -1}]},
            {"ID": "RSC-005", "severity": "ERROR", "message": "Bad",
             "locations": [{"path": "a.xhtml", "line": 3}]},
            {"ID": "OPF-085", "severity": "WARNING", "message": "Odd",
             "locations": [{"path": "content.opf", "line": 2}]},
            {"ID": "ACC-007", "severity": "USAGE", "message": "Hint", "locations": []},
            {"ID": "INF-001", "severity": "INFO", "message": "Fine", "locations": []}
        ]}"#;
        assert_eq!(
            epubcheck_findings("b", report.as_bytes()).unwrap(),
            (
                5,
                vec![
                    "b: FATAL PKG-008 Unreadable (b.epub:-1)".to_string(),
                    "b: ERROR RSC-005 Bad (a.xhtml:3)".to_string(),
                    "b: WARNING OPF-085 Odd (content.opf:2)".to_string(),
                ]
            )
        );
    }

    /// Asserts that tectonic (XeLaTeX) compiles `tex` to a PDF, and returns
    /// what it printed. Skipped, returning `None`, where tectonic isn't
    /// installed or can't download its bundle.
    fn assert_latex_compiles(tex: &Path) -> Option<String> {
        let Ok(tectonic) = which::which("tectonic") else {
            eprintln!("skipping: tectonic not found");
            return None;
        };
        let output = std::process::Command::new(tectonic)
            .args(["-c", "minimal"])
            .arg(tex)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        // tectonic downloads its bundle on first use.
        if !output.status.success()
            && ["network", "dns", "connection"]
                .iter()
                .any(|w| stderr.contains(w))
        {
            eprintln!("skipping: tectonic couldn't download its bundle");
            return None;
        }
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stderr),
            std::fs::read_to_string(tex).unwrap()
        );
        assert!(tex.with_extension("pdf").is_file());
        Some(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    const TRICKY_MARKDOWN: &str = "---\ntitle: Tricky [test] & more\nauthor: [A_B, C#D]\n---\n\n\
# Heading *styled*[^h] with ~~strike~~ and `code`\n\n\
- [link] text\n- *em* first\n- [x] task\n    1. one\n       - two\n         1. three\n            - four\n              1. five\n                 - six\n\n\
1. outer\n\n   3. inner\n\n2. next\n\n\
| [first] | b | c |\n|:--|:-:|--:|\n| *star | [x] cell | z<br>[w] |\n| note[^t] | a<br>b[^t2] | 100% |\n\n\
Specials: \\\\ { } $ & % # _ ~ ^ [link](https://e.org/a_b?x=1%20#top) <https://e.org/~u> [up](#heading-styled-with-strike-and-code)\n\n\
$$\\begin{align}a &= b \\\\ c &= d\\end{align}$$\n\n$$\\frac{1}{2}$$\n\n\
```\n\\end{verbatim}\n  indented\n```\n\n\
> Quote:\n>\n> - one\n>   - two\n\n\
中文段落與 α → β ├──\n\n\
![pic](pic.png) ![missing](missing.png) ![remote](https://e.org/x.png)\n\n\
<p align=\"center\"><img src=\"pic.png\" alt=\"Logo\"></p>\n\n<img src=\"logo.svg\">\n\n\
Logo: <img src=\"pic.png\" width=\"50\"> inline.\n\n> ## Only heading\n\n- ## Only in list\n\n---\n\n\
> > > > > > > ## Deep\n> > > > > > >\n> > > > > > > Seven quotes, past the limit:\n> > > > > > >\n\
> > > > > > > ```\n> > > > > > > code\n> > > > > > > ```\n> > > > > > >\n> > > > > > > - item\n> > > > > > >\n\
> > > > > > > $$x$$\n> > > > > > >\n> > > > > > > ---\n\n\
Ref[^code].\n\n\
[^h]: Heading note.\n[^t]: Table note.\n[^t2]: Cell note.\n\
[^code]: Code:\n\n    ```\n    let x = 1;\n\n    let y = 2;\n    ```\n\n    ![pic](pic.png)\n\n    | a | b |\n    |---|---|\n    | 1 | 2 |\n";

    /// Regression for #44: the notes inside notes, which LaTeX would drop
    /// inside another's `\footnote`, are in the PDF, numbered as typst
    /// numbers them, in a longtable row too, where LaTeX sets a note's
    /// text only after the row. Pandoc, reading the LaTeX back, keeps the
    /// paragraph around them one paragraph, and the note inside a note.
    #[test]
    fn notes_inside_notes_are_typeset_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let source = "A[^1] B[^4].\n\n| x | y |\n|---|---|\n| c[^6] | d[^7] |\n\n\
                      [^1]: Outer[^2] and[^3].\n[^2]: Inner note[^5].\n[^3]: Second.\n\
                      [^4]: Next.\n[^5]: Level two.\n[^6]: Cell[^8].\n[^7]: Later cell.\n\
                      [^8]: In cell.\n";
        let (tex, text) = converted(dir.path(), "nested.md", source, "tex");
        assert_latex_compiles(&tex);
        if let Some(shown) = pdf_text(&tex.with_extension("pdf")) {
            let shown: String = shown.split_whitespace().collect();
            for note in [
                "A1B5.",
                "c6",
                "d8",
                "1Outer2and4.",
                "2Innernote3.",
                "3Leveltwo.",
                "4Second.",
                "5Next.",
                "6Cell7.",
                "7Incell.",
                "8Latercell.",
            ] {
                assert!(shown.contains(note), "{note} in {shown}");
            }
        }
        // Pandoc's own writers drop a note inside a note, so its AST shows
        // it: the first of its top-level blocks holds A and B.
        if let Some(back) = pandoc_read_back(&tex, &["-t", "native"]) {
            let first = back.split("\n, ").next().unwrap();
            assert!(first.contains("Str \"B\""), "{back}\n{text}");
            assert!(back.contains("Str \"Inner\""), "{back}");
        }
    }

    /// Regression for #44: a note in a place that can't hold its text, a
    /// strikeout (which stops at a `\footnote`), a superscript (which
    /// drops its text) or a longtable's head (which LaTeX sets once for
    /// every page, without it), is marked there and set after it, numbered
    /// in turn with the notes after it in the table's body.
    #[test]
    fn notes_where_latex_cant_set_them_are_set_after() {
        let dir = tempfile::tempdir().unwrap();
        let source = "A ~~strike[^s]~~ b ^sup[^p]^ c.\n\n| H[^h] |\n|---|\n| body[^b] |\n\n\
                      [^s]: In strike.\n[^p]: In sup.\n[^h]: In head.\n[^b]: In body.\n";
        let (tex, text) = converted(dir.path(), "held.md", source, "tex");
        assert!(
            text.contains("\\sout{strike\\mbox{\\footnotemark[1]}}\\footnotetext[1]"),
            "{text}"
        );
        assert_latex_compiles(&tex);
        if let Some(shown) = pdf_text(&tex.with_extension("pdf")) {
            let shown: String = shown.split_whitespace().collect();
            // The texts in number order, the head's before the body's.
            for expected in ["1Instrike.2Insup.3Inhead.4Inbody.", "H3", "body4"] {
                assert!(shown.contains(expected), "{expected} in {shown}");
            }
        }
    }

    /// Regression for #53: in a document with CJK text, the spaces beside
    /// the quotes and dashes of English sentences stay, those quoting CJK
    /// words included, as typst keeps them, and CJK text keeps its own.
    #[test]
    fn cjk_documents_keep_the_spaces_beside_english_punctuation() {
        let dir = tempfile::tempdir().unwrap();
        let source = "The “raw” data — and “draft” ones… it’s a ‘test’.\n\n\
                      He said “你好” to me, and `say “hi” — now`.\n\n他说“你好”——“好”。\n";
        let (tex, _) = converted(dir.path(), "mixed.md", source, "tex");
        assert_latex_compiles(&tex);
        if let Some(shown) = pdf_text(&tex.with_extension("pdf")) {
            for line in [
                "The “raw” data — and “draft” ones… it’s a ‘test’.",
                "He said “你好” to me, and say “hi” — now.",
                "他说“你好”——“好”。",
            ] {
                assert!(shown.contains(line), "{line} in {shown}");
            }
        }
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
