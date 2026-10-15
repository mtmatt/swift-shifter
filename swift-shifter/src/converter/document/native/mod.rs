//! Swift Shifter's own document converter, replacing pandoc one hop at a time
//! (see docs/superpowers/specs/2026-10-15-native-document-converter-design.md).
//! Readers build an [`ast::Document`]; writers render it.

pub mod ast;
pub mod read;
pub mod write;

use std::path::{Path, PathBuf};

use tauri::Emitter;

use crate::converter::document::ProgressPayload;
use crate::converter::document::utils::output_path;
use ast::{Block, Document, Inline, List};

/// Input formats the native readers handle (normalized extensions).
pub const INPUTS: &[&str] = &["md", "txt", "html"];
/// Output formats the native writers handle.
pub const OUTPUTS: &[&str] = &["md", "txt", "html"];

/// Whether the native engine converts `from` → `to` (normalized extensions).
pub fn handles(from: &str, to: &str) -> bool {
    INPUTS.contains(&from) && OUTPUTS.contains(&to) && from != to
}

/// Convert `path` to `target` (md, txt or html), writing next to the input or
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
    let out = output_path(path, target, output_dir)?;
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
    let source =
        std::fs::read(input).map_err(|e| format!("Failed to read {}: {e}", input.display()))?;
    // Lossy rather than failing on a stray invalid byte; a BOM is dropped.
    let source = String::from_utf8_lossy(&source);
    let source = source.trim_start_matches('\u{feff}');
    let from = input
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| crate::converter::graph::normalize_ext(&e.to_lowercase()).to_string())
        .unwrap_or_default();

    let mut doc = match from.as_str() {
        "md" => read::markdown::read(source),
        "txt" => read::plain::read(source),
        "html" => read::html::read(source),
        other => return Err(format!("The native converter can't read .{other}")),
    };
    if target == "html" {
        fill_title(&mut doc, input);
    }
    if let (Some(src_dir), Some(out_dir)) = (input.parent(), out.parent())
        && !same_dir(src_dir, out_dir)
    {
        rebase_relative_urls(&mut doc.blocks, src_dir);
    }

    let rendered = match target {
        "md" => write::markdown::write(&doc),
        "txt" => write::plain::write(&doc),
        "html" => write::html::write(&doc),
        other => return Err(format!("The native converter can't write {other}")),
    };
    std::fs::write(out, rendered).map_err(|e| format!("Failed to write {}: {e}", out.display()))
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

/// Plain text as Markdown with the same meaning (markup characters escaped),
/// for converters that only take Markdown (pandoc, for txt → pdf/tex/typst).
pub fn plain_text_to_markdown(text: &str) -> String {
    write::markdown::write(&read::plain::read(text))
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

/// Make relative image and link targets absolute against `base`, so they
/// still resolve when the output is written to a different folder (an
/// output directory, or a temp dir between conversion hops).
fn rebase_relative_urls(blocks: &mut [Block], base: &Path) {
    for block in blocks {
        match block {
            Block::Heading { content, .. } | Block::Paragraph(content) => {
                rebase_inlines(content, base)
            }
            Block::Quote(children)
            | Block::FootnoteDefinition {
                content: children, ..
            } => rebase_relative_urls(children, base),
            Block::List(List { items, .. }) => items
                .iter_mut()
                .for_each(|item| rebase_relative_urls(&mut item.content, base)),
            Block::Table(table) => table
                .header
                .iter_mut()
                .chain(table.rows.iter_mut().flatten())
                .for_each(|cell| rebase_inlines(cell, base)),
            _ => {}
        }
    }
}

fn rebase_inlines(inlines: &mut [Inline], base: &Path) {
    for inline in inlines {
        match inline {
            Inline::Image { url, .. } => rebase_url(url, base),
            Inline::Link { url, content, .. } => {
                rebase_url(url, base);
                rebase_inlines(content, base);
            }
            Inline::Emph(c)
            | Inline::Strong(c)
            | Inline::Strikethrough(c)
            | Inline::Superscript(c)
            | Inline::Subscript(c) => rebase_inlines(c, base),
            _ => {}
        }
    }
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
fn percent_decode(text: &str) -> String {
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
        assert!(!handles("md", "pdf"));
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
            html.contains("<html lang=\"ja\">"),
            "kana means Japanese: {html}"
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

    #[test]
    fn plain_text_to_markdown_keeps_markup_literal() {
        let md = plain_text_to_markdown("# not a heading\n*literal*");
        assert_eq!(
            read::markdown::read(&md).blocks,
            read::plain::read("# not a heading\n*literal*").blocks
        );
    }

    #[test]
    fn markdown_to_html_uses_given_title() {
        let html = markdown_to_html("a<br>b\n\n| x |\n|---|\n| 1<br>2 |\n", "report");
        assert!(html.contains("<title>report</title>"));
        assert!(html.contains("<td>1<br>\n2</td>"), "{html}");
    }
}
