//! How the reader's reading of a document differs from pandoc's, which
//! reads `.tex` today: each is written as an outline (see
//! [`round_trip::outline`]), pandoc's read through its HTML, and compared
//! in order (see [`round_trip::differences`]). Where pandoc isn't
//! installed, as in CI, the tests pass by. Run on the bench's documents by
//! hand:
//!
//! `cargo test --release --bin swift-shifter differential -- --ignored --nocapture`

use super::round_trip::{differences, outline};
use super::walk;
use crate::converter::document::native::ast::{Block, Document, Inline};
use crate::converter::document::native::read::html;
use std::path::Path;
use std::process::Command;

/// `file`, in `dir`, as pandoc reads it, with the files it reads in, from
/// its HTML (see [`pandoc_html_document`]); `None` where pandoc can't be
/// run.
fn pandoc_reading(dir: &Path, file: &str) -> Option<Result<Document, String>> {
    let output = Command::new("pandoc")
        .args(["--from=latex", "--to=html5", "--mathml", "--standalone"])
        .arg(file)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(Err(String::from_utf8_lossy(&output.stderr).into_owned()));
    }
    let stem = Path::new(file).file_stem().and_then(|stem| stem.to_str());
    Some(Ok(pandoc_html_document(
        &String::from_utf8_lossy(&output.stdout),
        stem.unwrap_or(""),
    )))
}

/// The document pandoc's HTML of a file named `stem` holds, as the reader
/// would have it, read by the HTML reader, which takes each formula's TeX
/// from its MathML:
///
/// - each display formula a block of its own, as the reader has it, not
///   in a paragraph, as pandoc has it where the LaTeX does;
/// - the date from the page's header, which the HTML reader passes by;
/// - no title where the page is titled `stem`, as pandoc titles a
///   document with none.
fn pandoc_html_document(page: &str, stem: &str) -> Document {
    const BLOCK_MATH: &str = "<math display=\"block\"";
    let mut apart = String::with_capacity(page.len());
    let mut rest = page;
    while let Some(start) = rest.find(BLOCK_MATH) {
        let end = rest[start..]
            .find("</math>")
            .map_or(rest.len(), |end| start + end + "</math>".len());
        // A paragraph it's in ends before it, and goes on after it; an empty
        // one is none.
        apart.push_str(&rest[..start]);
        apart.push_str("</p>");
        apart.push_str(&rest[start..end]);
        apart.push_str("<p>");
        rest = &rest[end..];
    }
    apart.push_str(rest);
    let mut doc = html::read(&apart);
    doc.meta.date = page
        .split_once("<p class=\"date\">")
        .and_then(|(_, after)| {
            let (date, _) = after.split_once("</p>")?;
            Some(html::text_of(date)).filter(|date| !date.is_empty())
        });
    if doc.meta.title.as_deref() == Some(stem) {
        doc.meta.title = None;
    }
    doc
}

/// How many of each kind of thing a document holds, anywhere in it.
#[derive(Default)]
struct Counts {
    headings: usize,
    tables: usize,
    list_items: usize,
    notes: usize,
    /// Inline and display.
    formulas: usize,
    images: usize,
}

impl Counts {
    fn of(doc: &Document) -> Self {
        let mut blocks = doc.blocks.clone();
        let mut counts = Counts::default();
        walk::for_each_block(&mut blocks, &mut |block| match block {
            Block::Heading { .. } => counts.headings += 1,
            Block::Table(_) => counts.tables += 1,
            Block::List(list) => counts.list_items += list.items.len(),
            Block::FootnoteDefinition { .. } => counts.notes += 1,
            Block::Math(_) => counts.formulas += 1,
            _ => {}
        });
        walk::for_each_inlines(&mut blocks, &mut |inlines| {
            for inline in inlines.iter() {
                match inline {
                    Inline::Math(_) => counts.formulas += 1,
                    Inline::Image { .. } => counts.images += 1,
                    _ => {}
                }
            }
        });
        counts
    }

    /// Each kind's name and count.
    fn named(&self) -> [(&'static str, usize); 6] {
        [
            ("headings", self.headings),
            ("tables", self.tables),
            ("items", self.list_items),
            ("notes", self.notes),
            ("formulas", self.formulas),
            ("images", self.images),
        ]
    }
}

/// How alike two outlines are: the share of their lines in their longest
/// common subsequence, 1 where they're the same, 0 where they've none in
/// common.
fn similarity(expected: &[String], actual: &[String]) -> f64 {
    let lines = expected.len() + actual.len();
    if lines == 0 {
        return 1.0;
    }
    1.0 - differences(expected, actual).len() as f64 / lines as f64
}

#[cfg(test)]
mod tests {
    use super::super::{read, read_in};
    use super::*;

    #[test]
    fn similarity_is_the_share_of_lines_in_common() {
        let lines = |text: &str| text.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(similarity(&lines("a\nb"), &lines("a\nb")), 1.0);
        assert_eq!(similarity(&lines("a\nb"), &lines("c")), 0.0);
        // `a` is in both: two lines of four.
        assert_eq!(similarity(&lines("a\nb"), &lines("a\nc")), 0.5);
        assert_eq!(similarity(&[], &[]), 1.0);
    }

    /// A small document's reading is pandoc's, where pandoc is installed.
    #[test]
    fn a_document_is_read_as_pandoc_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let source = "\\documentclass{article}\n\\title{Paper}\n\\date{May}\n\\begin{document}\n\\section{One}\nSome \\emph{text} and $x^2$:\n\\[y\\]\n\nthen more.\n\\begin{itemize}\n\\item first\n\\item second\n\\end{itemize}\n\\end{document}\n";
        std::fs::write(dir.path().join("main.tex"), source).unwrap();
        let Some(pandoc) = pandoc_reading(dir.path(), "main.tex") else {
            return;
        };
        let pandoc = outline(&pandoc.unwrap());
        let (ours, _) = read_in(source, dir.path());
        assert_eq!(differences(&pandoc, &outline(&ours)), Vec::<String>::new());
    }

    /// Pandoc's HTML's display formulas are blocks of their own, its date
    /// is read, and its title where it's the file's name is none.
    #[test]
    fn pandoc_html_is_read_as_the_reader_reads() {
        let math = |tex: &str| {
            format!(
                "<math display=\"block\"><semantics><mi>x</mi><annotation encoding=\"application/x-tex\">{tex}</annotation></semantics></math>"
            )
        };
        let page = format!(
            "<html><head><title>main</title></head><body><header><p class=\"date\">May</p></header><p>a {} b</p><p>{}</p><p>c</p></body></html>",
            math("x"),
            math("y")
        );
        let doc = pandoc_html_document(&page, "main");
        let text = |text: &str| Block::Paragraph(vec![Inline::Text(text.into())]);
        assert_eq!(
            doc.blocks,
            [
                text("a"),
                Block::Math("x".into()),
                text("b"),
                Block::Math("y".into()),
                text("c"),
            ]
        );
        assert_eq!(doc.meta.date.as_deref(), Some("May"));
        assert_eq!(doc.meta.title, None);
        let doc = pandoc_html_document("<title>Paper</title><p>a</p>", "main");
        assert_eq!(doc.meta.title.as_deref(), Some("Paper"));
        assert_eq!(doc.meta.date, None);
    }

    #[test]
    fn counts_are_of_each_kind_anywhere() {
        let (doc, _) = read(
            "\\begin{document}\\section{A}$x$\\[y\\]\\begin{itemize}\\item \\includegraphics{p}\\item b\\footnote{$z$}\\end{itemize}\\end{document}",
        );
        let counts: Vec<usize> = Counts::of(&doc).named().map(|(_, count)| count).into();
        assert_eq!(counts, [1, 0, 2, 1, 3, 1]);
    }

    /// The bench's documents, as the reader and pandoc read them: how alike
    /// their outlines are, and the first of their differences.
    #[test]
    #[ignore = "a report on the bench's documents, run by hand"]
    fn differential() {
        let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench");
        let manifest = std::fs::read_to_string(bench.join("documents.toml")).unwrap();
        let mut dir = String::new();
        let mut total = 0.0;
        let mut count = 0;
        for line in manifest.lines() {
            let value = line.split('"').nth(1).unwrap_or("");
            if line.starts_with("dir =") {
                dir = value.to_string();
            } else if line.starts_with("main =") {
                let folder = bench.join("documents").join(&dir);
                let Some(pandoc) = pandoc_reading(&folder, value) else {
                    println!("pandoc couldn't be run");
                    return;
                };
                let pandoc = match pandoc {
                    Ok(pandoc) => pandoc,
                    Err(why) => {
                        println!("{dir}: pandoc failed: {}", why.trim());
                        continue;
                    }
                };
                let bytes = std::fs::read(folder.join(value)).unwrap();
                let (ours, _) = read_in(&String::from_utf8_lossy(&bytes), &folder);
                // Each kind's count, pandoc's / ours.
                let kinds: Vec<String> = Counts::of(&pandoc)
                    .named()
                    .iter()
                    .zip(Counts::of(&ours).named())
                    .map(|((kind, theirs), (_, ours))| format!("{kind} {theirs}/{ours}"))
                    .collect();
                let (pandoc, ours) = (outline(&pandoc), outline(&ours));
                let alike = similarity(&pandoc, &ours);
                total += alike;
                count += 1;
                println!(
                    "{dir}: {alike:.3} alike, lines {}/{}, {}",
                    pandoc.len(),
                    ours.len(),
                    kinds.join(", ")
                );
                for difference in differences(&pandoc, &ours).iter().take(6) {
                    let shown: String = difference.chars().take(100).collect();
                    println!("  {shown}");
                }
            }
        }
        println!(
            "mean: {:.3} alike, of {count}",
            total / f64::from(count.max(1))
        );
    }
}
