//! [`Document`] → EPUB 3: one XHTML file per chapter, a navigation document
//! and NCX built from the headings, packaged images and an e-reader
//! stylesheet. Chapters are rendered by the HTML writer and re-serialized as
//! XHTML (see `xhtml.rs`), so the output is well-formed whatever raw HTML the
//! source contains.

use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

use super::html::{NoteStyle, Writer, WrittenHeading};
use super::xhtml::{self, Features, Hooks, escape_attr, escape_text};
use crate::converter::document::native::ast::{Block, Document};
use crate::converter::document::native::local_path;

pub struct Options {
    /// Where relative image paths resolve.
    pub base_dir: PathBuf,
}

const OPS_NS: &str = "http://www.idpf.org/2007/ops";

/// Write `doc` as an EPUB 3 file.
pub fn write(doc: &Document, options: &Options) -> Result<Vec<u8>, String> {
    let title = doc
        .meta
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Untitled".to_string());
    let lang = doc
        .effective_lang()
        .or_else(|| sys_locale::get_locale().filter(|l| is_language_tag(l)))
        .unwrap_or_else(|| "en".to_string());

    // Render every chapter first: links can point forward, so ids must be
    // known for all of them before any is serialized.
    let split = split_chapters(&doc.blocks);
    let mut writer = Writer::new(doc, NoteStyle::EpubAsides);
    let rendered: Vec<Rendered> = split
        .chapters
        .iter()
        .enumerate()
        .map(|(i, blocks)| {
            let (html, headings) = writer.chapter(blocks);
            Rendered {
                file: format!("ch{:03}.xhtml", i + 1),
                html: xhtml::parse(&html),
                headings,
            }
        })
        .collect();
    let mut id_files: HashMap<String, String> = HashMap::new();
    for chapter in &rendered {
        for id in xhtml::ids(&chapter.html) {
            id_files.entry(id).or_insert_with(|| chapter.file.clone());
        }
    }

    let mut images = Images::new(&options.base_dir);
    let chapters: Vec<Chapter> = rendered
        .into_iter()
        .map(|chapter| {
            let link = |href: &str| book_link(href, &chapter.file, &id_files);
            let mut image = |src: &str| images.add(src).map(|href| format!("../{href}"));
            let mut hooks = Hooks {
                image: &mut image,
                link: &link,
            };
            let (body, features) = xhtml::serialize(&chapter.html, &mut hooks);
            Chapter {
                file: chapter.file,
                headings: chapter.headings,
                body,
                features,
            }
        })
        .collect();
    let cover = doc.meta.cover.as_deref().and_then(|src| images.add(src));

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for chapter in &chapters {
        let chapter_title = chapter
            .headings
            .first()
            .map(|h| h.text.as_str())
            .unwrap_or(&title);
        files.push((
            format!("text/{}", chapter.file),
            xhtml_page(&lang, chapter_title, "../style.css", &chapter.body).into_bytes(),
        ));
    }
    if let Some(cover) = &cover {
        let body = format!(
            "<section epub:type=\"cover\" class=\"cover\"><img src=\"../{}\" alt=\"{}\"/></section>",
            escape_attr(cover),
            escape_attr(&title)
        );
        files.push((
            "text/cover.xhtml".into(),
            xhtml_page(&lang, &title, "../style.css", &body).into_bytes(),
        ));
    }

    let toc = toc_entries(&chapters, split.level, &title);
    let book = BookInfo {
        title: &title,
        lang: &lang,
        identifier: identifier(&files),
    };
    files.push(("nav.xhtml".into(), nav_page(&book, &toc).into_bytes()));
    files.push(("toc.ncx".into(), ncx(&book, &toc).into_bytes()));
    files.push(("style.css".into(), STYLE.as_bytes().to_vec()));
    let opf = package(doc, &book, &chapters, &images, cover.as_deref());
    files.push(("content.opf".into(), opf.into_bytes()));
    for image in &images.files {
        files.push((image.href.clone(), image.data.clone()));
    }
    zip_package(&files)
}

/// The book-wide values every package file repeats.
struct BookInfo<'a> {
    title: &'a str,
    lang: &'a str,
    identifier: String,
}

/// A chapter rendered to HTML, before serialization.
struct Rendered {
    file: String,
    html: scraper::Html,
    headings: Vec<WrittenHeading>,
}

/// A chapter as packaged.
struct Chapter {
    file: String,
    headings: Vec<WrittenHeading>,
    body: String,
    features: Features,
}

/// Where a link in chapter `file` points inside the book: fragment links to
/// another chapter name its file, and web and mail links are kept. Anything
/// else (local files, `javascript:`, fragments nothing defines) can't work
/// in an EPUB, so `None` keeps only the link's text.
fn book_link(href: &str, file: &str, id_files: &HashMap<String, String>) -> Option<String> {
    if let Some(id) = href.strip_prefix('#') {
        return id_files.get(id).map(|target| {
            if target == file {
                href.to_string()
            } else {
                format!("{target}{href}")
            }
        });
    }
    let scheme = href.split(':').next().unwrap_or("").to_ascii_lowercase();
    (href.contains(':') && matches!(scheme.as_str(), "http" | "https" | "mailto"))
        .then(|| href.to_string())
}

/// A BCP 47-looking tag (`en`, `en-US`, `zh-Hant-TW`); POSIX locales like
/// `C` and `POSIX` aren't.
fn is_language_tag(tag: &str) -> bool {
    let mut subtags = tag.split('-');
    let primary = subtags.next().unwrap_or("");
    (2..=8).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_alphabetic())
        && !primary.eq_ignore_ascii_case("posix")
        && subtags
            .all(|s| (1..=8).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Top-level blocks split into chapters, and the heading level they split at.
struct Split<'a> {
    chapters: Vec<&'a [Block]>,
    level: Option<u8>,
}

/// Split the top-level blocks into chapters at the chapter heading level:
/// the smallest level used, unless that heading appears once, first, as the
/// document's title, in which case the next level down.
fn split_chapters(blocks: &[Block]) -> Split<'_> {
    let levels: Vec<(usize, u8)> = blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match b {
            Block::Heading { level, .. } => Some((i, *level)),
            _ => None,
        })
        .collect();
    let whole = |level| Split {
        chapters: vec![blocks],
        level,
    };
    let Some(&top) = levels.iter().map(|(_, l)| l).min() else {
        return whole(None);
    };
    let top_count = levels.iter().filter(|(_, l)| *l == top).count();
    let title_only = top_count == 1 && levels[0].1 == top && levels.len() > 1;
    let split_level = if title_only {
        levels.iter().map(|(_, l)| *l).filter(|l| *l > top).min()
    } else {
        Some(top)
    };
    let Some(split_level) = split_level else {
        return whole(Some(top));
    };
    let mut starts: Vec<usize> = levels
        .iter()
        .filter(|(_, l)| *l == split_level)
        .map(|(i, _)| *i)
        .collect();
    // Content before the first chapter heading (a title, an introduction)
    // opens the first chapter rather than standing alone.
    starts[0] = 0;
    let mut chapters = Vec::new();
    for (k, &start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(blocks.len());
        chapters.push(&blocks[start..end]);
    }
    Split {
        chapters,
        level: Some(split_level),
    }
}

/// Images packaged into the book, each source once.
struct Images {
    base: PathBuf,
    /// Each source seen, by file path or `data:` URL. `None` records one that
    /// can't be packaged, so it isn't tried again.
    by_source: HashMap<String, Option<String>>,
    files: Vec<PackagedImage>,
}

struct PackagedImage {
    href: String,
    media_type: &'static str,
    data: Vec<u8>,
}

/// Image bytes in a format EPUB readers must support.
struct LoadedImage {
    ext: &'static str,
    media_type: &'static str,
    data: Vec<u8>,
}

impl Images {
    fn new(base: &Path) -> Self {
        Images {
            base: base.to_path_buf(),
            by_source: HashMap::new(),
            files: Vec::new(),
        }
    }

    /// The package path (`images/N.ext`) of a local or `data:` image, or
    /// `None` for a remote, missing or unreadable one.
    fn add(&mut self, src: &str) -> Option<String> {
        let (key, loaded) = if src.starts_with("data:") {
            (src.to_string(), None)
        } else {
            let path = local_path(src, &self.base)?;
            (path.to_string_lossy().into_owned(), Some(path))
        };
        if let Some(href) = self.by_source.get(&key) {
            return href.clone();
        }
        let image = match &loaded {
            Some(path) => load_image_file(path),
            None => load_data_url(src),
        };
        let packaged = image.map(|image| {
            let href = format!("images/{}.{}", self.files.len() + 1, image.ext);
            self.files.push(PackagedImage {
                href: href.clone(),
                media_type: image.media_type,
                data: image.data,
            });
            href
        });
        self.by_source.insert(key, packaged.clone());
        packaged
    }
}

/// The extension and media type for an image format EPUB readers must
/// support (PNG, JPEG, GIF, SVG, WebP), by extension or media subtype.
fn epub_image_type(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "png" => ("png", "image/png"),
        "jpg" | "jpeg" => ("jpg", "image/jpeg"),
        "gif" => ("gif", "image/gif"),
        "svg" | "svg+xml" => ("svg", "image/svg+xml"),
        "webp" => ("webp", "image/webp"),
        _ => return None,
    })
}

fn load_image_file(path: &Path) -> Option<LoadedImage> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match epub_image_type(&ext) {
        Some((ext, media_type)) => std::fs::read(path).ok().map(|data| LoadedImage {
            ext,
            media_type,
            data,
        }),
        None => to_png(::image::open(path).ok()?),
    }
}

/// A base64 `data:image/…` URL, as embedded by saved web pages.
fn load_data_url(url: &str) -> Option<LoadedImage> {
    let (header, payload) = url.strip_prefix("data:")?.split_once(',')?;
    let mut params = header.split(';');
    let subtype = params.next()?.strip_prefix("image/")?.to_ascii_lowercase();
    if !params.any(|p| p.eq_ignore_ascii_case("base64")) {
        return None;
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .ok()?;
    match epub_image_type(&subtype) {
        Some((ext, media_type)) => Some(LoadedImage {
            ext,
            media_type,
            data,
        }),
        None => to_png(::image::load_from_memory(&data).ok()?),
    }
}

fn to_png(image: ::image::DynamicImage) -> Option<LoadedImage> {
    let mut data = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut data), ::image::ImageFormat::Png)
        .ok()?;
    Some(LoadedImage {
        ext: "png",
        media_type: "image/png",
        data,
    })
}

fn xhtml_page(lang: &str, title: &str, css: &str, body: &str) -> String {
    let lang = escape_attr(lang);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"{OPS_NS}\" lang=\"{lang}\" xml:lang=\"{lang}\">\n\
         <head>\n<meta charset=\"utf-8\"/>\n<title>{}</title>\n\
         <link rel=\"stylesheet\" type=\"text/css\" href=\"{css}\"/>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        escape_text(title)
    )
}

/// A table-of-contents entry; `children` are the next level down.
struct TocEntry {
    text: String,
    href: String,
    children: Vec<TocEntry>,
}

/// Headings down to one level below the chapter level, nested under the
/// chapter heading before them; a title heading above the chapter level is
/// an entry of its own. A book without headings gets one entry per chapter,
/// so the TOC is never empty.
fn toc_entries(chapters: &[Chapter], split_level: Option<u8>, title: &str) -> Vec<TocEntry> {
    let Some(level) = split_level else {
        return chapters
            .iter()
            .enumerate()
            .map(|(i, c)| TocEntry {
                text: if chapters.len() == 1 {
                    title.to_string()
                } else {
                    format!("{title} ({})", i + 1)
                },
                href: format!("text/{}", c.file),
                children: Vec::new(),
            })
            .collect();
    };
    let mut entries: Vec<TocEntry> = Vec::new();
    for chapter in chapters {
        for heading in &chapter.headings {
            if heading.level > level + 1 {
                continue;
            }
            let entry = TocEntry {
                text: heading.text.clone(),
                href: format!("text/{}#{}", chapter.file, heading.id),
                children: Vec::new(),
            };
            match entries.last_mut() {
                Some(parent) if heading.level > level => parent.children.push(entry),
                _ => entries.push(entry),
            }
        }
    }
    entries
}

fn nav_page(book: &BookInfo, toc: &[TocEntry]) -> String {
    fn list(out: &mut String, entries: &[TocEntry]) {
        out.push_str("<ol>\n");
        for entry in entries {
            out.push_str(&format!(
                "<li><a href=\"{}\">{}</a>",
                escape_attr(&entry.href),
                escape_text(&entry.text)
            ));
            if !entry.children.is_empty() {
                out.push('\n');
                list(out, &entry.children);
            }
            out.push_str("</li>\n");
        }
        out.push_str("</ol>\n");
    }
    let mut body = String::from("<nav epub:type=\"toc\" id=\"toc\">\n<h1>Contents</h1>\n");
    list(&mut body, toc);
    body.push_str("</nav>");
    xhtml_page(book.lang, book.title, "style.css", &body)
}

/// The EPUB 2 table of contents, for older readers.
fn ncx(book: &BookInfo, toc: &[TocEntry]) -> String {
    fn points(out: &mut String, entries: &[TocEntry], order: &mut usize) {
        for entry in entries {
            *order += 1;
            out.push_str(&format!(
                "<navPoint id=\"nav-{0}\" playOrder=\"{0}\"><navLabel><text>{1}</text></navLabel><content src=\"{2}\"/>\n",
                order,
                escape_text(&entry.text),
                escape_attr(&entry.href)
            ));
            points(out, &entry.children, order);
            out.push_str("</navPoint>\n");
        }
    }
    let mut map = String::new();
    points(&mut map, toc, &mut 0);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\">\n\
         <head><meta name=\"dtb:uid\" content=\"{}\"/></head>\n\
         <docTitle><text>{}</text></docTitle>\n<navMap>\n{map}</navMap>\n</ncx>\n",
        escape_attr(&book.identifier),
        escape_text(book.title)
    )
}

fn package(
    doc: &Document,
    book: &BookInfo,
    chapters: &[Chapter],
    images: &Images,
    cover: Option<&str>,
) -> String {
    let mut metadata = format!(
        "<dc:identifier id=\"book-id\">{}</dc:identifier>\n<dc:title>{}</dc:title>\n<dc:language>{}</dc:language>\n",
        escape_text(&book.identifier),
        escape_text(book.title),
        escape_text(book.lang)
    );
    for author in &doc.meta.authors {
        metadata.push_str(&format!(
            "<dc:creator>{}</dc:creator>\n",
            escape_text(author)
        ));
    }
    if let Some(date) = doc.meta.date.as_deref().filter(|d| is_w3c_date(d)) {
        metadata.push_str(&format!("<dc:date>{}</dc:date>\n", escape_text(date)));
    }
    if let Some(description) = &doc.meta.description {
        metadata.push_str(&format!(
            "<dc:description>{}</dc:description>\n",
            escape_text(description)
        ));
    }
    metadata.push_str(&format!(
        "<meta property=\"dcterms:modified\">{}</meta>\n",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    ));

    let mut manifest = String::from(
        "<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n\
         <item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n\
         <item id=\"style\" href=\"style.css\" media-type=\"text/css\"/>\n",
    );
    let mut spine = String::new();
    if cover.is_some() {
        manifest.push_str(
            "<item id=\"cover\" href=\"text/cover.xhtml\" media-type=\"application/xhtml+xml\"/>\n",
        );
        spine.push_str("<itemref idref=\"cover\"/>\n");
    }
    for (i, chapter) in chapters.iter().enumerate() {
        let mut properties = Vec::new();
        if chapter.features.mathml {
            properties.push("mathml");
        }
        if chapter.features.svg {
            properties.push("svg");
        }
        let properties = if properties.is_empty() {
            String::new()
        } else {
            format!(" properties=\"{}\"", properties.join(" "))
        };
        manifest.push_str(&format!(
            "<item id=\"ch{}\" href=\"text/{}\" media-type=\"application/xhtml+xml\"{properties}/>\n",
            i + 1,
            chapter.file
        ));
        spine.push_str(&format!("<itemref idref=\"ch{}\"/>\n", i + 1));
    }
    for (i, image) in images.files.iter().enumerate() {
        let is_cover = cover == Some(image.href.as_str());
        if is_cover {
            // EPUB 2 readers (and Kindle tooling) find the cover this way.
            metadata.push_str(&format!(
                "<meta name=\"cover\" content=\"img{}\"/>\n",
                i + 1
            ));
        }
        manifest.push_str(&format!(
            "<item id=\"img{}\" href=\"{}\" media-type=\"{}\"{}/>\n",
            i + 1,
            image.href,
            image.media_type,
            if is_cover {
                " properties=\"cover-image\""
            } else {
                ""
            }
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"book-id\" xml:lang=\"{}\">\n\
         <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n{metadata}</metadata>\n\
         <manifest>\n{manifest}</manifest>\n<spine toc=\"ncx\">\n{spine}</spine>\n</package>\n",
        escape_attr(book.lang)
    )
}

/// The W3C date-time forms `dc:date` allows: `YYYY`, `YYYY-MM`,
/// `YYYY-MM-DD`, or a full date with `Thh:mm`, optional seconds (and
/// fraction), and `Z` or a `±hh:mm` offset.
fn is_w3c_date(date: &str) -> bool {
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let (day, time) = match date.split_once('T') {
        Some((day, time)) => (day, Some(time)),
        None => (date, None),
    };
    let parts: Vec<&str> = day.split('-').collect();
    let day_ok = match parts.as_slice() {
        [y] => digits(y, 4),
        [y, m] => digits(y, 4) && digits(m, 2),
        [y, m, d] => digits(y, 4) && digits(m, 2) && digits(d, 2),
        _ => false,
    };
    let Some(time) = time else {
        return day_ok;
    };
    if !day_ok || parts.len() != 3 {
        return false;
    }
    let (clock, zone) = match time.find(['Z', '+', '-']) {
        Some(i) => time.split_at(i),
        None => return false,
    };
    let zone_ok = zone == "Z"
        || zone
            .get(1..)
            .and_then(|z| z.split_once(':'))
            .is_some_and(|(h, m)| digits(h, 2) && digits(m, 2));
    let clock: Vec<&str> = clock.split(':').collect();
    let clock_ok = match clock.as_slice() {
        [h, m] => digits(h, 2) && digits(m, 2),
        [h, m, s] => {
            let (whole, fraction) = s.split_once('.').unwrap_or((s, "0"));
            digits(h, 2)
                && digits(m, 2)
                && digits(whole, 2)
                && !fraction.is_empty()
                && fraction.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    };
    zone_ok && clock_ok
}

/// A stable `urn:uuid` from the book's content: converting the same input
/// again gives the same identifier.
fn identifier(files: &[(String, Vec<u8>)]) -> String {
    let mut hasher = Sha256::new();
    for (name, data) in files {
        hasher.update(name.as_bytes());
        hasher.update(data);
    }
    let mut bytes: [u8; 16] = hasher.finalize()[..16].try_into().expect("16 bytes");
    // RFC 4122 version 5 (name-based) and variant bits.
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The OCF container: `mimetype` first and uncompressed, as readers require.
fn zip_package(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let error = |e: &dyn std::fmt::Display| format!("Failed to write the EPUB: {e}");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("mimetype", stored).map_err(|e| error(&e))?;
    zip.write_all(b"application/epub+zip")
        .map_err(|e| error(&e))?;
    zip.start_file("META-INF/container.xml", deflated)
        .map_err(|e| error(&e))?;
    zip.write_all(CONTAINER.as_bytes()).map_err(|e| error(&e))?;
    for (name, data) in files {
        zip.start_file(format!("OEBPS/{name}"), deflated)
            .map_err(|e| error(&e))?;
        zip.write_all(data).map_err(|e| error(&e))?;
    }
    Ok(zip.finish().map_err(|e| error(&e))?.into_inner())
}

const CONTAINER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\">\n\
<rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles>\n\
</container>\n";

/// For e-readers: no colours or backgrounds (readers apply their own themes),
/// sizes relative to the reader's font size, code that wraps.
const STYLE: &str = r#"@namespace epub "http://www.idpf.org/2007/ops";
body { margin: 0 4%; line-height: 1.5; text-align: justify; hyphens: auto; -webkit-hyphens: auto; }
:lang(zh), :lang(ja), :lang(ko) { line-break: strict; hyphens: none; -webkit-hyphens: none; }
h1, h2, h3, h4, h5, h6 { line-height: 1.25; text-align: left; hyphens: none; -webkit-hyphens: none; page-break-after: avoid; break-after: avoid; }
h1 { font-size: 1.6em; margin: 1.5em 0 0.8em; }
h2 { font-size: 1.35em; margin: 1.4em 0 0.6em; }
h3 { font-size: 1.15em; margin: 1.2em 0 0.5em; }
p { margin: 0 0 0.8em; }
blockquote { margin: 0.8em 0 0.8em 1.2em; padding-left: 0.8em; border-left: 2px solid currentColor; }
code, pre { font-family: monospace; font-size: 0.9em; }
pre { white-space: pre-wrap; word-wrap: break-word; margin: 0.8em 0; padding-left: 0.8em; border-left: 2px solid currentColor; }
img { max-width: 100%; height: auto; }
table { border-collapse: collapse; margin: 0.8em 0; }
th, td { border: 1px solid currentColor; padding: 0.2em 0.5em; vertical-align: top; }
hr { border: 0; border-top: 1px solid currentColor; margin: 1.5em 20%; }
li.task { list-style: none; }
li.task input { margin: 0 0.4em 0 -1.3em; }
sup.footnote-ref a, .footnote-back { text-decoration: none; }
aside[epub|type~="footnote"] { font-size: 0.9em; }
.cover { text-align: center; margin: 0; }
.cover img { max-height: 95vh; }
nav ol { list-style: none; padding-left: 1em; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::read::markdown;
    use std::io::Read;

    struct Book {
        names: Vec<String>,
        files: HashMap<String, Vec<u8>>,
        first_stored: bool,
    }

    impl Book {
        fn text(&self, name: &str) -> String {
            String::from_utf8(self.files[name].clone()).unwrap()
        }
    }

    fn open(epub: &[u8]) -> Book {
        let mut archive = zip::ZipArchive::new(Cursor::new(epub)).unwrap();
        let mut names = Vec::new();
        let mut files = HashMap::new();
        let first_stored =
            archive.by_index(0).unwrap().compression() == zip::CompressionMethod::Stored;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i).unwrap();
            let mut data = Vec::new();
            file.read_to_end(&mut data).unwrap();
            names.push(file.name().to_string());
            files.insert(file.name().to_string(), data);
        }
        Book {
            names,
            files,
            first_stored,
        }
    }

    fn book(md: &str, base: &Path) -> Book {
        let doc = markdown::read(md);
        let options = Options {
            base_dir: base.to_path_buf(),
        };
        let book = open(&write(&doc, &options).unwrap());
        assert_package_is_well_formed(&book);
        book
    }

    /// Every XML file parses, and the manifest lists exactly the files.
    fn assert_package_is_well_formed(book: &Book) {
        assert_eq!(book.names[0], "mimetype");
        assert!(book.first_stored, "mimetype must be stored");
        assert_eq!(book.files["mimetype"], b"application/epub+zip");
        for (name, data) in &book.files {
            let is_xml = [".xhtml", ".opf", ".ncx", ".xml"]
                .iter()
                .any(|ext| name.ends_with(ext));
            if is_xml {
                let text = std::str::from_utf8(data).unwrap();
                roxmltree::Document::parse_with_options(text, xml_options())
                    .unwrap_or_else(|e| panic!("{name}: {e}\n{text}"));
            }
        }
        let opf = book.text("OEBPS/content.opf");
        let opf = roxmltree::Document::parse(&opf).unwrap();
        let mut listed: Vec<String> = opf
            .descendants()
            .filter(|n| n.has_tag_name("item"))
            .map(|n| format!("OEBPS/{}", n.attribute("href").unwrap()))
            .collect();
        listed.sort();
        let mut packaged: Vec<String> = book
            .names
            .iter()
            .filter(|n| n.starts_with("OEBPS/") && *n != "OEBPS/content.opf")
            .cloned()
            .collect();
        packaged.sort();
        assert_eq!(listed, packaged);
    }

    fn xml_options<'a>() -> roxmltree::ParsingOptions<'a> {
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        }
    }

    /// A fresh, empty folder, so no stray file can satisfy an image path.
    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn every_fixture_writes_a_well_formed_book() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/docs");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let doc = match path.extension().and_then(|e| e.to_str()) {
                Some("md") => markdown::read(&std::fs::read_to_string(&path).unwrap()),
                Some("html") => crate::converter::document::native::read::html::read(
                    &std::fs::read_to_string(&path).unwrap(),
                ),
                Some("txt") => crate::converter::document::native::read::plain::read(
                    &std::fs::read_to_string(&path).unwrap(),
                ),
                _ => continue,
            };
            let options = Options {
                base_dir: dir.clone(),
            };
            let epub = write(&doc, &options).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_package_is_well_formed(&open(&epub));
        }
    }

    #[test]
    fn a_title_heading_doesnt_make_a_chapter_of_its_own() {
        let book = book(
            "# Book\n\nIntro.\n\n## One\n\nA.\n\n## Two\n\nB.\n",
            tmp().path(),
        );
        let ch1 = book.text("OEBPS/text/ch001.xhtml");
        assert!(
            ch1.contains("<h1 id=\"book\">Book</h1>") && ch1.contains("One"),
            "{ch1}"
        );
        assert!(book.text("OEBPS/text/ch002.xhtml").contains("Two"));
        assert!(!book.files.contains_key("OEBPS/text/ch003.xhtml"));
        let nav = book.text("OEBPS/nav.xhtml");
        assert!(
            nav.contains("<a href=\"text/ch001.xhtml#book\">Book</a>"),
            "{nav}"
        );
        assert!(
            nav.contains("<a href=\"text/ch002.xhtml#two\">Two</a>"),
            "{nav}"
        );
    }

    #[test]
    fn repeated_top_headings_are_chapters() {
        let book = book("Preface.\n\n# A\n\nx\n\n# B\n\ny\n", tmp().path());
        let ch1 = book.text("OEBPS/text/ch001.xhtml");
        assert!(
            ch1.contains("Preface") && ch1.contains("<h1 id=\"a\">"),
            "{ch1}"
        );
        assert!(
            book.text("OEBPS/text/ch002.xhtml")
                .contains("<h1 id=\"b\">")
        );
    }

    #[test]
    fn a_book_without_headings_is_one_chapter_with_a_toc_entry() {
        let book = book("---\ntitle: Notes\n---\n\nJust text.\n", tmp().path());
        assert!(!book.files.contains_key("OEBPS/text/ch002.xhtml"));
        assert!(
            book.text("OEBPS/nav.xhtml")
                .contains("<a href=\"text/ch001.xhtml\">Notes</a>")
        );
    }

    #[test]
    fn footnotes_are_asides_in_the_chapter_that_first_cites_them() {
        let book = book(
            "# A\n\nOne[^x].\n\n# B\n\nAgain[^x] and[^y].\n\n[^x]: X note[^z].\n[^y]: Y note.\n[^z]: Z note.\n",
            tmp().path(),
        );
        let ch1 = book.text("OEBPS/text/ch001.xhtml");
        assert!(ch1.contains("epub:type=\"noteref\""), "{ch1}");
        assert!(
            ch1.contains("<aside epub:type=\"footnote\" id=\"fn-1\""),
            "{ch1}"
        );
        // Notes cited in the main text are numbered first (x 1, y 2), then
        // the note cited inside x (z 3), which goes with x.
        assert!(
            ch1.contains("id=\"fn-3\"") && ch1.contains("Z note"),
            "nested note: {ch1}"
        );
        let ch2 = book.text("OEBPS/text/ch002.xhtml");
        assert!(ch2.contains("href=\"ch001.xhtml#fn-1\""), "{ch2}");
        assert!(
            ch2.contains("id=\"fn-2\"") && !ch2.contains("id=\"fn-1\""),
            "{ch2}"
        );
    }

    #[test]
    fn links_to_headings_in_other_chapters_name_the_file() {
        let book = book(
            "# A\n\nSee [b](#b).\n\n# B\n\nBack to [a](#a).\n",
            tmp().path(),
        );
        assert!(
            book.text("OEBPS/text/ch001.xhtml")
                .contains("href=\"ch002.xhtml#b\"")
        );
        assert!(
            book.text("OEBPS/text/ch002.xhtml")
                .contains("href=\"ch001.xhtml#a\"")
        );
    }

    #[test]
    fn raw_html_comes_out_well_formed() {
        let book = book(
            "Text<br>with <sup>unbalanced\n\n<div><p>Open<img src=\"x.png\"></div>\n",
            tmp().path(),
        );
        assert!(book.text("OEBPS/text/ch001.xhtml").contains("<br/>"));
    }

    #[test]
    fn images_are_packaged_once_and_converted_when_needed() {
        let dir = tempfile::tempdir().unwrap();
        ::image::RgbImage::new(4, 4)
            .save(dir.path().join("a.png"))
            .unwrap();
        ::image::RgbImage::new(4, 4)
            .save(dir.path().join("b.bmp"))
            .unwrap();
        let book = book(
            "---\ncover-image: a.png\n---\n\n![a](a.png) ![again](a.png) ![b](b.bmp) \
             ![remote](https://e.org/c.png) ![gone](missing.png)\n",
            dir.path(),
        );
        assert!(book.files.contains_key("OEBPS/images/1.png"));
        assert!(book.files["OEBPS/images/2.png"].starts_with(b"\x89PNG"));
        assert!(!book.files.contains_key("OEBPS/images/3.png"));
        let ch1 = book.text("OEBPS/text/ch001.xhtml");
        assert_eq!(ch1.matches("src=\"../images/1.png\"").count(), 2, "{ch1}");
        assert!(ch1.contains("[remote]") && ch1.contains("[gone]"), "{ch1}");
        let opf = book.text("OEBPS/content.opf");
        assert!(
            opf.contains(
                "href=\"images/1.png\" media-type=\"image/png\" properties=\"cover-image\""
            ),
            "{opf}"
        );
        assert!(opf.contains("<itemref idref=\"cover\"/>"), "{opf}");
    }

    #[test]
    fn metadata_and_math_are_declared() {
        let book = book(
            "---\ntitle: T & U\nauthor: [Ann, Bo]\ndate: 2024-05-01\nlang: fr\n---\n\n$x^2$\n",
            tmp().path(),
        );
        let opf = book.text("OEBPS/content.opf");
        assert!(opf.contains("<dc:title>T &amp; U</dc:title>"), "{opf}");
        assert!(opf.contains("<dc:creator>Ann</dc:creator>\n<dc:creator>Bo</dc:creator>"));
        assert!(opf.contains("<dc:date>2024-05-01</dc:date>"));
        assert!(opf.contains("<dc:language>fr</dc:language>"));
        assert!(opf.contains("properties=\"mathml\""), "{opf}");
        assert!(opf.contains("<dc:identifier id=\"book-id\">urn:uuid:"));
        let ch1 = book.text("OEBPS/text/ch001.xhtml");
        assert!(
            ch1.contains("<math xmlns=\"http://www.w3.org/1998/Math/MathML\""),
            "{ch1}"
        );
        assert!(ch1.contains("xml:lang=\"fr\""));
    }

    #[test]
    fn the_toc_lists_chapters_and_their_sections_only() {
        let book = book(
            "# Book\n\n## One\n\n### One.1\n\n#### Deep\n\n> ## Quoted\n\n## Two\n",
            tmp().path(),
        );
        let nav = book.text("OEBPS/nav.xhtml");
        for entry in ["Book", "One", "One.1", "Two"] {
            assert!(nav.contains(&format!(">{entry}</a>")), "{entry}: {nav}");
        }
        assert!(!nav.contains("Deep") && !nav.contains("Quoted"), "{nav}");
    }

    #[test]
    fn links_that_cant_work_in_a_book_keep_only_their_text() {
        let dir = tmp();
        std::fs::write(dir.path().join("other.md"), "x").unwrap();
        let book = book(
            "[other](other.md) [web](https://e.org) [mail](mailto:a@e.org) [nowhere](#nope) \
             <a href=\"javascript:x()\">js</a>\n\n<div><o:p>word</o:p></div>\n",
            dir.path(),
        );
        let ch1 = book.text("OEBPS/text/ch001.xhtml");
        assert!(
            ch1.contains("<p>other <a href=\"https://e.org\">web</a>"),
            "{ch1}"
        );
        assert!(
            ch1.contains("<a href=\"mailto:a@e.org\">mail</a> nowhere js"),
            "{ch1}"
        );
        assert!(ch1.contains("<div>word</div>"), "{ch1}");
    }

    #[test]
    fn data_url_images_are_packaged() {
        let mut png = Vec::new();
        ::image::RgbImage::new(2, 2)
            .write_to(&mut Cursor::new(&mut png), ::image::ImageFormat::Png)
            .unwrap();
        let mut bmp = Vec::new();
        ::image::RgbImage::new(2, 2)
            .write_to(&mut Cursor::new(&mut bmp), ::image::ImageFormat::Bmp)
            .unwrap();
        let b64 = |data: &[u8]| base64::engine::general_purpose::STANDARD.encode(data);
        let book = book(
            &format!(
                "![a](data:image/png;base64,{}) ![b](data:image/bmp;base64,{}) ![c](data:text/plain,hi)\n",
                b64(&png),
                b64(&bmp)
            ),
            tmp().path(),
        );
        assert_eq!(book.files["OEBPS/images/1.png"], png);
        assert!(book.files["OEBPS/images/2.png"].starts_with(b"\x89PNG"));
        assert!(book.text("OEBPS/text/ch001.xhtml").contains("[c]"));
    }

    #[test]
    fn locales_are_used_only_when_they_are_language_tags() {
        assert!(is_language_tag("en") && is_language_tag("zh-Hant-TW") && is_language_tag("de-CH"));
        assert!(!is_language_tag("C") && !is_language_tag("POSIX") && !is_language_tag("en_US"));
    }

    #[test]
    fn identifiers_are_stable_and_dates_validated() {
        let files = vec![("a".to_string(), b"x".to_vec())];
        assert_eq!(identifier(&files), identifier(&files));
        assert_ne!(
            identifier(&files),
            identifier(&[("a".into(), b"y".to_vec())])
        );
        for date in [
            "2024",
            "2024-05",
            "2024-05-01",
            "2024-05-01T10:00Z",
            "2024-05-01T10:00:00.5+08:00",
        ] {
            assert!(is_w3c_date(date), "{date}");
        }
        for date in ["2024-05-01Tgarbage", "2024-05T10:00Z", "2024-05-01T10:00"] {
            assert!(!is_w3c_date(date), "{date}");
        }
        assert!(!is_w3c_date("May 2024") && !is_w3c_date("24-05-01"));
    }
}
