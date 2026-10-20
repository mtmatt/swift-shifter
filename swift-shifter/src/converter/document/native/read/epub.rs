//! EPUB → [`Document`]. The book is extracted to a temporary folder (its
//! images are read from there), the package document gives the metadata and
//! the reading order, and each chapter is read with the HTML reader. Links
//! and footnotes are then resolved across chapters, so a link to another
//! chapter's heading points at that heading in the single output document.
//!
//! A book is untrusted input: every path it names is resolved inside the
//! extracted folder, and anything pointing elsewhere is dropped.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use scraper::{ElementRef, Html};
use tempfile::TempDir;

use super::{html, walk};
use crate::converter::document::native::ast::{Block, Document, Inline, Meta, inline_text};
use crate::converter::document::native::write::xhtml::{VOID, escape_attr, escape_text};
use crate::converter::document::native::write::{self, HeadingIds};
use crate::converter::document::native::{local_path, percent_decode};

/// A book read into one document. Image URLs in `doc` are absolute paths
/// inside `dir`, which lives as long as this value.
pub struct Book {
    pub doc: Document,
    pub dir: TempDir,
}

/// Extraction limit, so a malicious archive can't fill the disk.
const MAX_EXTRACTED_BYTES: u64 = 2 << 30;

pub fn read(path: &Path) -> Result<Book, String> {
    let dir = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {e}"))?;
    extract(path, dir.path())?;
    let root = dir.path();

    let container = std::fs::read_to_string(root.join("META-INF/container.xml"))
        .map_err(|_| "Not an EPUB: META-INF/container.xml is missing".to_string())?;
    let opf_rel = rootfile_path(&container)
        .ok_or_else(|| "Not an EPUB: container.xml names no package document".to_string())?;
    let opf_path = contained(root, &opf_rel)
        .ok_or_else(|| format!("Invalid package document path: {opf_rel}"))?;
    let opf_text = std::fs::read_to_string(&opf_path)
        .map_err(|e| format!("Failed to read the package document: {e}"))?;
    let mut package = parse_package(&opf_text, opf_path.parent().unwrap_or(root))?;
    let inside = |path: &Path| contained(root, &relative_to(root, path));
    package.meta.cover = package
        .meta
        .cover
        .take()
        .and_then(|cover| inside(Path::new(&cover)))
        .map(|path| path.to_string_lossy().into_owned());

    // Chapters are the readable pages in reading order (an unreadable page
    // is skipped, as e-readers do). Pages outside the spine are read only
    // for their notes: EPUB 2 books often keep notes there.
    let read_page = |path: &PathBuf| {
        let path = inside(path)?;
        let source = std::fs::read(&path).ok()?;
        Some((path, as_html(&String::from_utf8_lossy(&source))))
    };
    let chapters: Vec<(PathBuf, String)> = package.spine.iter().filter_map(read_page).collect();
    let note_pages: Vec<(PathBuf, String)> = package
        .unlisted_pages
        .iter()
        .filter_map(read_page)
        .collect();
    let chapter_count = chapters.len();
    let pages: Vec<(PathBuf, String)> = chapters.into_iter().chain(note_pages).collect();
    // Page numbers: chapters first, so a chapter's number is its index.
    let page_numbers: HashMap<PathBuf, usize> = pages
        .iter()
        .enumerate()
        .map(|(i, (path, _))| (path.clone(), i))
        .collect();
    let chapter_numbers: HashMap<PathBuf, usize> = page_numbers
        .iter()
        .filter(|(_, i)| **i < chapter_count)
        .map(|(path, i)| (path.clone(), *i))
        .collect();

    let mut parts = Vec::new();
    let mut notes = Vec::new();
    for (i, (path, page)) in pages.iter().enumerate() {
        let mut blocks = html::read_book_page(page);
        let resolver = Resolver {
            root,
            page: i,
            page_dir: path.parent().unwrap_or(root),
            chapters: &chapter_numbers,
            pages: &page_numbers,
        };
        resolver.localize(&mut blocks);
        if i < chapter_count {
            parts.push(Part {
                blocks,
                anchors: heading_anchors(page),
            });
        } else {
            notes.extend(
                blocks
                    .into_iter()
                    .filter(|b| matches!(b, Block::FootnoteDefinition { .. })),
            );
        }
    }

    let targets = heading_targets(&parts);
    let mut blocks = Vec::new();
    for part in &mut parts {
        resolve_links(&mut part.blocks, &targets);
        blocks.append(&mut part.blocks);
    }
    if blocks.is_empty() {
        return Err("The EPUB has no readable chapters".to_string());
    }
    blocks.extend(notes);
    drop_undefined_refs(&mut blocks);
    Ok(Book {
        doc: Document {
            meta: package.meta,
            blocks,
        },
        dir,
    })
}

/// Unpack the archive into `dir`, skipping entries whose names would land
/// outside it.
fn extract(path: &Path, dir: &Path) -> Result<(), String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Not an EPUB (bad zip file): {e}"))?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read the EPUB: {e}"))?;
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let target = dir.join(name);
        if entry.is_dir() {
            continue;
        }
        let mut data = Vec::new();
        let limit = MAX_EXTRACTED_BYTES - total;
        (&mut entry)
            .take(limit + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("Failed to read the EPUB: {e}"))?;
        total += data.len() as u64;
        if total > MAX_EXTRACTED_BYTES {
            return Err("The EPUB is too large to convert".to_string());
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to extract the EPUB: {e}"))?;
        }
        std::fs::write(&target, data).map_err(|e| format!("Failed to extract the EPUB: {e}"))?;
    }
    Ok(())
}

/// `relative` inside `root`, resolving `.` and `..` by name, or `None` when
/// it would escape it.
fn contained(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir if path != root => {
                path.pop();
            }
            _ => return None,
        }
    }
    Some(path)
}

/// `path` relative to `root`, for re-checking with [`contained`]. A path
/// outside `root` stays absolute, which `contained` rejects.
fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn xml(text: &str) -> Option<roxmltree::Document<'_>> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    roxmltree::Document::parse_with_options(text, options).ok()
}

fn rootfile_path(container: &str) -> Option<String> {
    let doc = xml(container)?;
    doc.descendants()
        .find(|n| n.has_tag_name("rootfile"))
        .and_then(|n| n.attribute("full-path"))
        .map(|p| percent_decode(p.trim()))
}

struct Package {
    meta: Meta,
    /// Pages in reading order (the navigation page left out).
    spine: Vec<PathBuf>,
    /// Pages in the manifest but not the spine.
    unlisted_pages: Vec<PathBuf>,
}

const DC_NS: &str = "http://purl.org/dc/elements/1.1/";

/// Metadata and reading order from the package document. Paths resolve from
/// `opf_dir`.
fn parse_package(opf: &str, opf_dir: &Path) -> Result<Package, String> {
    let doc = xml(opf).ok_or_else(|| "The package document isn't valid XML".to_string())?;
    let text_of = |name: &str| {
        doc.descendants()
            .filter(|n| n.tag_name().name() == name && n.tag_name().namespace() == Some(DC_NS))
            .filter_map(|n| n.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
    };

    struct Item {
        path: PathBuf,
        media_type: String,
        properties: String,
    }
    impl Item {
        fn has_property(&self, property: &str) -> bool {
            self.properties.split_whitespace().any(|p| p == property)
        }
        fn is_page(&self) -> bool {
            matches!(
                self.media_type.as_str(),
                "application/xhtml+xml" | "text/html"
            ) && !self.has_property("nav")
        }
    }
    let items: Vec<(&str, Item)> = doc
        .descendants()
        .filter(|n| n.has_tag_name("item"))
        .filter_map(|n| {
            Some((
                n.attribute("id")?,
                Item {
                    path: opf_dir.join(percent_decode(n.attribute("href")?)),
                    media_type: n.attribute("media-type").unwrap_or("").to_string(),
                    properties: n.attribute("properties").unwrap_or("").to_string(),
                },
            ))
        })
        .collect();
    let by_id: HashMap<&str, &Item> = items.iter().map(|(id, item)| (*id, item)).collect();

    // EPUB 3 marks the cover image in the manifest; EPUB 2 in a <meta>.
    let cover_id = doc
        .descendants()
        .find(|n| n.has_tag_name("meta") && n.attribute("name") == Some("cover"))
        .and_then(|n| n.attribute("content"));
    let cover = items
        .iter()
        .map(|(_, item)| item)
        .find(|item| item.has_property("cover-image"))
        .or_else(|| cover_id.and_then(|id| by_id.get(id).copied()))
        .filter(|item| item.media_type.starts_with("image/"))
        .map(|item| item.path.to_string_lossy().into_owned());

    let spine: Vec<PathBuf> = doc
        .descendants()
        .filter(|n| n.has_tag_name("itemref"))
        .filter_map(|n| by_id.get(n.attribute("idref")?))
        .filter(|item| item.is_page())
        .map(|item| item.path.clone())
        .collect();
    let in_spine: HashSet<&PathBuf> = spine.iter().collect();
    let unlisted_pages = items
        .iter()
        .map(|(_, item)| item)
        .filter(|item| item.is_page() && !in_spine.contains(&item.path))
        .map(|item| item.path.clone())
        .collect();

    let meta = Meta {
        title: text_of("title").into_iter().next(),
        authors: text_of("creator"),
        lang: text_of("language").into_iter().next(),
        date: text_of("date").into_iter().next(),
        description: text_of("description").into_iter().next(),
        cover,
    };
    Ok(Package {
        meta,
        spine,
        unlisted_pages,
    })
}

/// A chapter as HTML for the HTML reader. EPUB chapters are XHTML, where
/// `<div/>` or `<a id="x"/>` close themselves, but an HTML parser would
/// leave them open and swallow what follows. So a well-formed chapter is
/// re-serialized with explicit end tags; anything else is read as HTML.
fn as_html(xhtml: &str) -> String {
    let Some(doc) = xml(xhtml) else {
        return xhtml.to_string();
    };
    let mut out = String::with_capacity(xhtml.len());
    write_html(&mut out, doc.root_element());
    out
}

fn write_html(out: &mut String, node: roxmltree::Node) {
    match node.node_type() {
        roxmltree::NodeType::Text => out.push_str(&escape_text(node.text().unwrap_or(""))),
        roxmltree::NodeType::Element => {
            let name = node.tag_name().name();
            out.push('<');
            out.push_str(name);
            for attr in node.attributes() {
                // `epub:type` keeps its prefix, which the HTML reader looks for.
                let attr_name = match attr.namespace() {
                    Some("http://www.idpf.org/2007/ops") => format!("epub:{}", attr.name()),
                    _ => attr.name().to_string(),
                };
                out.push_str(&format!(" {attr_name}=\"{}\"", escape_attr(attr.value())));
            }
            out.push('>');
            if VOID.contains(&name) {
                return;
            }
            for child in node.children() {
                write_html(out, child);
            }
            out.push_str(&format!("</{name}>"));
        }
        _ => {}
    }
}

/// One chapter's blocks, and its headings' anchors in document order.
struct Part {
    blocks: Vec<Block>,
    anchors: Vec<HeadingAnchor>,
}

/// A heading on a chapter page and the ids that point at it.
struct HeadingAnchor {
    /// The heading's text, whitespace collapsed.
    text: String,
    ids: Vec<String>,
}

/// The headings of a chapter page with the ids that target them: the
/// heading's own id, and that of a section or div it opens (pandoc writes
/// `<section id="intro"><h1>`).
fn heading_anchors(page: &str) -> Vec<HeadingAnchor> {
    let doc = Html::parse_document(page);
    doc.root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter(|el| matches!(el.value().name(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6"))
        .map(|heading| {
            let mut ids: Vec<String> = heading
                .value()
                .id()
                .map(str::to_string)
                .into_iter()
                .collect();
            let mut el = heading;
            while let Some(parent) = el.parent().and_then(ElementRef::wrap) {
                let opens_parent =
                    parent.children().filter_map(ElementRef::wrap).next() == Some(el);
                let is_wrapper =
                    matches!(parent.value().name(), "section" | "div" | "header" | "body");
                if !opens_parent || !is_wrapper {
                    break;
                }
                ids.extend(parent.value().id().map(str::to_string));
                el = parent;
            }
            let text = heading.text().collect::<String>();
            HeadingAnchor {
                text: text.split_whitespace().collect::<Vec<_>>().join(" "),
                ids,
            }
        })
        .collect()
}

/// Rewrites a page's URLs into book-wide terms.
struct Resolver<'a> {
    root: &'a Path,
    page: usize,
    page_dir: &'a Path,
    /// Chapters (spine pages) by path: links can only go there.
    chapters: &'a HashMap<PathBuf, usize>,
    /// Every page read, chapters and note pages: notes can live in either.
    pages: &'a HashMap<PathBuf, usize>,
}

/// Prefix of a link into the book while chapters are still being read:
/// `\u{1}{chapter}#{id}`, or [`UNRESOLVED`]. Resolved by [`resolve_links`].
const INTERNAL: char = '\u{1}';
/// A link into the book that can't go anywhere in the output (a page
/// outside the reading order, an image, a path leaving the book).
const UNRESOLVED: &str = "-";

impl Resolver<'_> {
    /// Images become absolute paths in the extracted book (or their alt
    /// text, when they point anywhere else); links into the book and
    /// footnote labels become `{page}#{id}`.
    fn localize(&self, blocks: &mut [Block]) {
        walk::for_each_inlines(blocks, &mut |inlines| {
            for inline in inlines.iter_mut() {
                match inline {
                    Inline::Image { url, alt, .. } if !is_web_image(url) => {
                        match self.image_path(url) {
                            Some(path) => *url = path,
                            None => *inline = Inline::Text(std::mem::take(alt)),
                        }
                    }
                    Inline::Link { url, .. } if !has_scheme(url) => {
                        let target = self.target(url, self.chapters);
                        *url = format!("{INTERNAL}{}", target.as_deref().unwrap_or(UNRESOLVED));
                    }
                    Inline::FootnoteRef(label) => *label = self.label(label),
                    _ => {}
                }
            }
        });
        for_each_definition(blocks, &mut |label| {
            *label = format!("{}#{label}", self.page);
        });
    }

    /// The image file `url` names, if it exists inside the book. Absolute
    /// paths, `file://` URLs and `..` past the book's root name files
    /// elsewhere on this computer and are refused.
    fn image_path(&self, url: &str) -> Option<String> {
        let path = local_path(url, self.page_dir)?;
        contained(self.root, &relative_to(self.root, &path))
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// `{page}#{id}` for a relative URL to one of `pages`.
    fn target(&self, url: &str, pages: &HashMap<PathBuf, usize>) -> Option<String> {
        let (file, fragment) = url.split_once('#').unwrap_or((url, ""));
        let page = if file.is_empty() {
            self.page
        } else {
            let path = contained(
                self.root,
                &relative_to(self.root, &self.page_dir.join(percent_decode(file))),
            )?;
            *pages.get(&path)?
        };
        Some(format!("{page}#{}", percent_decode(fragment)))
    }

    /// A footnote label in book-wide terms (see `html::note_label`).
    fn label(&self, label: &str) -> String {
        if label.contains('#') {
            self.target(label, self.pages)
                .unwrap_or_else(|| UNRESOLVED.to_string())
        } else {
            format!("{}#{label}", self.page)
        }
    }
}

/// A remote or inline image (`http:`, `https:`, `data:`), which needs no
/// file. Every other image URL, a Windows `C:\\…` path included, must
/// resolve inside the book.
fn is_web_image(url: &str) -> bool {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase());
    matches!(scheme.as_deref(), Some("http" | "https" | "data"))
}

/// Whether a URL names its scheme (`https:`, `mailto:`, `data:`), as
/// opposed to a path relative to the page.
fn has_scheme(url: &str) -> bool {
    let head = url.split(['/', '?', '#']).next().unwrap_or("");
    head.contains(':')
}

/// Where each in-book target lands in the output: `{chapter}#{id}` → the
/// slug the writers will give the heading it marks, and `{chapter}#` → the
/// chapter's first heading.
fn heading_targets(parts: &[Part]) -> HashMap<String, String> {
    let mut ids = HeadingIds::default();
    let mut targets: HashMap<String, String> = HashMap::new();
    for (chapter, part) in parts.iter().enumerate() {
        // Only the main text's headings: headings in notes get their ids
        // after every chapter's (see `DocumentIds`), and in-book links
        // don't target them.
        let mut headings = Vec::new();
        write::for_each_heading(&part.blocks, &[], &mut |_, content| {
            headings.push(content.to_vec())
        });
        let slugs: Vec<String> = headings.iter().map(|h| ids.next(h)).collect();
        if let Some(first) = slugs.first() {
            targets.insert(format!("{chapter}#"), first.clone());
        }
        let mut point = |anchor: &HeadingAnchor, slug: &str| {
            for id in &anchor.ids {
                targets
                    .entry(format!("{chapter}#{id}"))
                    .or_insert_with(|| slug.to_string());
            }
        };
        // Headings pair up with the page's anchors by position when the
        // counts agree (the reader drops empty or title-page headings),
        // otherwise by text.
        if part.anchors.len() == slugs.len() {
            for (anchor, slug) in part.anchors.iter().zip(&slugs) {
                point(anchor, slug);
            }
        } else {
            for (heading, slug) in headings.iter().zip(&slugs) {
                let text = inline_text(heading);
                if let Some(anchor) = part.anchors.iter().find(|a| a.text == text) {
                    point(anchor, slug);
                }
            }
        }
    }
    targets
}

/// Point links into the book at their heading slugs; a link to anything
/// else in the book (a paragraph anchor, a page outside the reading order)
/// keeps only its text.
fn resolve_links(blocks: &mut [Block], targets: &HashMap<String, String>) {
    walk::resolve_links(blocks, INTERNAL, &mut |target, _| {
        targets.get(target).cloned()
    });
}

/// Remove references to notes the book doesn't contain: writers would
/// print their internal label.
fn drop_undefined_refs(blocks: &mut [Block]) {
    let mut defined = HashSet::new();
    for_each_definition(blocks, &mut |label| {
        defined.insert(label.clone());
    });
    walk::for_each_inlines(blocks, &mut |inlines| {
        inlines.retain(|i| !matches!(i, Inline::FootnoteRef(label) if !defined.contains(label)));
    });
}

/// Calls `f` with every footnote label in `blocks`, in reading order.
fn for_each_definition(blocks: &mut [Block], f: &mut dyn FnMut(&mut String)) {
    walk::for_each_block(blocks, &mut |block| {
        if let Block::FootnoteDefinition { label, .. } = block {
            f(label);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::converter::document::native::{read::markdown, write};
    use std::io::Write;

    /// A zip with `files`, `mimetype` first, written to a temp folder.
    fn epub_file(files: &[(&str, &[u8])]) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        (dir, path)
    }

    const CONTAINER: &[u8] = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
<rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

    fn page(body: &str) -> Vec<u8> {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" \
             xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>t</title></head><body>{body}</body></html>"
        )
        .into_bytes()
    }

    #[test]
    fn our_own_books_round_trip() {
        let src = tempfile::tempdir().unwrap();
        ::image::RgbImage::new(3, 3)
            .save(src.path().join("pic.png"))
            .unwrap();
        let doc = markdown::read(
            "---\ntitle: Round Trip\nauthor: [Ann, Bo]\nlang: fr\ncover-image: pic.png\n---\n\n\
             # A\n\nSee [b](#b) and a note[^n].\n\n![pic](pic.png)\n\n# B\n\nAgain[^n] and[^m].\n\n\
             [^n]: Note N.\n[^m]: Note M.\n",
        );
        let options = write::epub::Options {
            base_dir: src.path().to_path_buf(),
        };
        let (_dir, path) = epub_file(&[]);
        std::fs::write(&path, write::epub::write(&doc, &options).unwrap()).unwrap();

        let book = read(&path).unwrap();
        let meta = &book.doc.meta;
        assert_eq!(meta.title.as_deref(), Some("Round Trip"));
        assert_eq!(meta.authors, ["Ann", "Bo"]);
        assert_eq!(meta.lang.as_deref(), Some("fr"));
        assert!(Path::new(meta.cover.as_deref().unwrap()).is_file());

        let md = write::markdown::write(&book.doc);
        assert!(md.contains("See [b](#b) and a note[^1]."), "{md}");
        assert!(md.contains("Again[^1] and[^2]."), "{md}");
        assert!(
            md.contains("[^1]: Note N.") && md.contains("[^2]: Note M."),
            "{md}"
        );
        let image = book.doc.blocks.iter().find_map(|b| match b {
            Block::Paragraph(c) => c.iter().find_map(|i| match i {
                Inline::Image { url, .. } if url.ends_with(".png") => Some(url.clone()),
                _ => None,
            }),
            _ => None,
        });
        let image = image.expect("the image survives");
        let images = write::markdown::write(&book.doc).matches("![").count();
        assert_eq!(images, 1, "the cover page isn't read as content");
        assert!(Path::new(&image).starts_with(book.dir.path()) && Path::new(&image).is_file());
    }

    #[test]
    fn reads_epub2_style_books_with_cross_file_links_and_notes() {
        let opf = br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
<dc:title>Hand Made</dc:title><dc:creator>Cy</dc:creator><dc:language>en</dc:language>
<meta name="cover" content="cover-img"/>
</metadata>
<manifest>
<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
<item id="c1" href="text/ch%201.xhtml" media-type="application/xhtml+xml"/>
<item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
<item id="notes" href="text/notes.xhtml" media-type="application/xhtml+xml"/>
<item id="cover-img" href="images/a.png" media-type="image/png"/>
</manifest>
<spine><itemref idref="nav"/><itemref idref="c1"/><itemref idref="c2"/><itemref idref="notes"/></spine>
</package>"#;
        let mut png = Vec::new();
        ::image::RgbImage::new(2, 2)
            .write_to(
                &mut std::io::Cursor::new(&mut png),
                ::image::ImageFormat::Png,
            )
            .unwrap();
        let ch1 = page(
            "<section id=\"sec1\"><h1>One</h1><p id=\"para\">Start<a id=\"x\"/> after the anchor.</p>\
             <p><img src=\"../images/a.png\" alt=\"A\"/></p></section>",
        );
        let ch2 = page(
            "<h1 id=\"two\">Two</h1><p><a href=\"ch%201.xhtml#sec1\">heading</a>, \
             <a href=\"ch%201.xhtml\">chapter</a>, <a href=\"ch%201.xhtml#para\">paragraph</a>, \
             <a href=\"https://e.org\">web</a>, note<a epub:type=\"noteref\" href=\"notes.xhtml#n1\">1</a>.</p>",
        );
        let notes = page("<aside epub:type=\"footnote\" id=\"n1\"><p>The note.</p></aside>");
        let nav = page(
            "<nav epub:type=\"toc\"><ol><li><a href=\"text/ch%201.xhtml\">One</a></li></ol></nav>",
        );
        let (_dir, path) = epub_file(&[
            ("mimetype", b"application/epub+zip"),
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", opf),
            ("OEBPS/nav.xhtml", &nav),
            ("OEBPS/text/ch 1.xhtml", &ch1),
            ("OEBPS/text/ch2.xhtml", &ch2),
            ("OEBPS/text/notes.xhtml", &notes),
            ("OEBPS/images/a.png", &png),
        ]);

        let book = read(&path).unwrap();
        assert_eq!(book.doc.meta.title.as_deref(), Some("Hand Made"));
        assert!(book.doc.meta.cover.as_deref().unwrap().ends_with("a.png"));
        let md = write::markdown::write(&book.doc);
        assert!(!md.contains("One\n\n- "), "the nav page is skipped: {md}");
        assert!(
            md.contains("Start after the anchor."),
            "self-closed <a/>: {md}"
        );
        assert!(
            md.contains(
                "[heading](#one), [chapter](#one), paragraph, [web](https://e.org), note[^1]."
            ),
            "{md}"
        );
        assert!(md.contains("[^1]: The note."), "{md}");
    }

    /// A minimal book whose spine is `pages` (name, body) under `OEBPS/`,
    /// plus `extra` manifest pages outside the spine and `files`.
    fn book_with(pages: &[(&str, &str)], extra: &[(&str, &str)], files: &[(&str, &[u8])]) -> Book {
        let item = |(name, _): &(&str, &str)| {
            format!("<item id=\"{name}\" href=\"{name}\" media-type=\"application/xhtml+xml\"/>")
        };
        let manifest: String = pages.iter().chain(extra).map(item).collect();
        let spine: String = pages
            .iter()
            .map(|(name, _)| format!("<itemref idref=\"{name}\"/>"))
            .collect();
        let opf = format!(
            "<package xmlns=\"http://www.idpf.org/2007/opf\"><manifest>{manifest}</manifest><spine>{spine}</spine></package>"
        );
        let bodies: Vec<(String, Vec<u8>)> = pages
            .iter()
            .chain(extra)
            .map(|(name, body)| (format!("OEBPS/{name}"), page(body)))
            .collect();
        let mut entries: Vec<(&str, &[u8])> = vec![
            ("META-INF/container.xml", CONTAINER),
            ("OEBPS/content.opf", opf.as_bytes()),
        ];
        entries.extend(bodies.iter().map(|(n, b)| (n.as_str(), b.as_slice())));
        entries.extend_from_slice(files);
        let (_dir, path) = epub_file(&entries);
        read(&path).unwrap()
    }

    #[test]
    fn images_outside_the_book_are_never_read() {
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.png");
        ::image::RgbImage::new(1, 1).save(&secret).unwrap();
        let file_url = url::Url::from_file_path(&secret).unwrap();
        let climb = format!("{}{}", "../".repeat(40), secret.display());
        let body = format!(
            "<p><img src=\"{}\" alt=\"abs\"/><img src=\"{file_url}\" alt=\"url\"/>\
             <img src=\"{climb}\" alt=\"climb\"/><img src=\"C:/Users/x/secret.png\" alt=\"drive\"/>\
             <img src=\"in.png\" alt=\"ok\"/></p>",
            secret.display()
        );
        let book = book_with(&[("c.xhtml", &body)], &[], &[("OEBPS/in.png", b"png")]);
        let Block::Paragraph(content) = &book.doc.blocks[0] else {
            panic!("{:?}", book.doc.blocks);
        };
        let urls: Vec<&str> = content
            .iter()
            .filter_map(|i| match i {
                Inline::Image { url, .. } => Some(url.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(urls.len(), 1, "{content:?}");
        assert!(Path::new(urls[0]).starts_with(book.dir.path()));
        assert_eq!(inline_text(content), "absurlclimbdriveok");
    }

    #[test]
    fn unreadable_pages_and_unreachable_links_are_dropped() {
        let book = book_with(
            &[
                ("missing.xhtml", ""),
                (
                    "c.xhtml",
                    "<p><a href=\"extra.xhtml\">extra</a> <a href=\"pic.png\">pic</a> \
                  <a href=\"../../x.xhtml\">out</a> note<a epub:type=\"noteref\" href=\"extra.xhtml#n1\">1</a> \
                  gone<a epub:type=\"noteref\" href=\"#nope\">2</a></p>",
                ),
            ],
            &[(
                "extra.xhtml",
                "<h1>Not in the spine</h1><aside epub:type=\"footnote\" id=\"n1\"><p>Far note.</p></aside>",
            )],
            &[("OEBPS/pic.png", b"png")],
        );
        let md = write::markdown::write(&book.doc);
        assert_eq!(
            md, "extra pic out note[^1] gone\n\n[^1]: Far note.\n",
            "{md}"
        );
    }

    #[test]
    fn links_inside_unresolved_links_still_resolve() {
        // The outer link goes nowhere in the output and keeps only its text,
        // which is itself a link to a heading: that link still resolves.
        let link = |url: String, content| Inline::Link {
            url,
            title: None,
            content,
        };
        let inner = link(format!("{INTERNAL}0#one"), vec![Inline::Text("x".into())]);
        let mut blocks = vec![Block::Paragraph(vec![link(
            format!("{INTERNAL}{UNRESOLVED}"),
            vec![inner],
        )])];
        let targets = HashMap::from([("0#one".to_string(), "one".to_string())]);
        resolve_links(&mut blocks, &targets);
        assert_eq!(
            blocks,
            [Block::Paragraph(vec![link(
                "#one".into(),
                vec![Inline::Text("x".into())]
            )])]
        );
    }

    #[test]
    fn display_math_in_a_paragraph_stays_display_math() {
        let book = book_with(
            &[(
                "c.xhtml",
                "<p><math xmlns=\"http://www.w3.org/1998/Math/MathML\" display=\"block\"><semantics><mi>x</mi>\
                 <annotation encoding=\"application/x-tex\">x^2</annotation></semantics></math></p>",
            )],
            &[],
            &[],
        );
        assert_eq!(book.doc.blocks, [Block::Math("x^2".into())]);
    }

    #[test]
    fn entries_outside_the_book_are_not_extracted() {
        let name = format!("swift-shifter-evil-{}.txt", std::process::id());
        let escape = format!("../{name}");
        let (_dir, path) = epub_file(&[
            ("META-INF/container.xml", CONTAINER),
            (escape.as_str(), b"x"),
            (
                "OEBPS/content.opf",
                br#"<package xmlns="http://www.idpf.org/2007/opf"><manifest><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#,
            ),
            ("OEBPS/c.xhtml", &page("<p>Hi</p>")),
        ]);
        let book = read(&path).unwrap();
        assert!(!book.dir.path().parent().unwrap().join(&name).exists());
        assert_eq!(write::plain::write(&book.doc).trim(), "Hi");
    }

    #[test]
    fn broken_books_say_what_is_wrong() {
        let (_dir, path) = epub_file(&[("mimetype", b"application/epub+zip")]);
        assert!(read(&path).err().unwrap().contains("container.xml"));
        let dir = tempfile::tempdir().unwrap();
        let not_zip = dir.path().join("x.epub");
        std::fs::write(&not_zip, "plain text").unwrap();
        assert!(read(&not_zip).err().unwrap().contains("bad zip"));
    }

    #[test]
    fn paths_stay_inside_the_book() {
        let root = Path::new("/book");
        assert_eq!(
            contained(root, "OEBPS/a.opf"),
            Some(PathBuf::from("/book/OEBPS/a.opf"))
        );
        assert_eq!(contained(root, "a/../b"), Some(PathBuf::from("/book/b")));
        assert_eq!(contained(root, "../etc/passwd"), None);
        assert_eq!(contained(root, "/etc/passwd"), None);
    }
}
