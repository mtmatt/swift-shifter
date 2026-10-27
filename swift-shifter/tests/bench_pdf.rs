//! The benchmark's PDF → Markdown scores (#150): each test document's
//! XeLaTeX PDF converted to Markdown with the app's CLI, as a user
//! converts it. Per document and in total, how alike the Markdown's text
//! is to the PDF's (see `bench::text`).
//!
//! The scores go to `target/bench/pdf-md.json` and to stdout, as a table
//! and in full, for the run's log. A low score doesn't fail the run.

// Shared with the other benchmark tests; not all of it is used here.
#[allow(dead_code)]
mod bench;

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use bench::cli::{cli_converts, convert, pdf_text, required};
use bench::run::{each_document, keep_scores, last_line};
use bench::{Document, compile, manifest, xelatex};
use comrak::arena_tree::NodeEdge;
use comrak::nodes::NodeValue;

/// One document's scores.
#[derive(serde::Serialize, Default)]
struct Scores {
    dir: String,
    converted: bool,
    /// Why there's no Markdown: the conversion that failed and all it said.
    error: Option<String>,
    /// See `bench::text::similarity`; none where there's no Markdown.
    text_similarity: Option<f64>,
    seconds: f64,
}

/// The scores of every document together.
#[derive(serde::Serialize, Debug, PartialEq)]
struct Total {
    /// The share of documents converted.
    convert_rate: f64,
    /// The documents' mean, those not converted counting 0.
    text_similarity: f64,
    /// The mean of those converted.
    text_similarity_converted: Option<f64>,
}

fn total(scores: &[Scores]) -> Total {
    let documents = scores.len().max(1) as f64;
    let converted: Vec<f64> = scores
        .iter()
        .filter(|s| s.converted)
        .map(|s| s.text_similarity.unwrap_or(0.0))
        .collect();
    // An empty sum is -0.0, which would read "-0.000"; adding 0.0 makes it 0.
    let similarity: f64 = converted.iter().sum::<f64>() + 0.0;
    Total {
        convert_rate: converted.len() as f64 / documents,
        text_similarity: similarity / documents,
        text_similarity_converted: (!converted.is_empty())
            .then(|| similarity / converted.len() as f64),
    }
}

/// `markdown`'s text, without its markup (`#`, list markers, table rules,
/// emphasis), so that only what it says is scored as words: a newline
/// between blocks and lines (pymupdf4llm's `<br>` among them), formulas
/// and code as written, and no other HTML.
fn markdown_text(markdown: &str) -> String {
    let arena = comrak::Arena::new();
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.footnotes = true;
    options.extension.math_dollars = true;
    let root = comrak::parse_document(&arena, markdown, &options);
    let mut text = String::new();
    for edge in root.traverse() {
        match edge {
            NodeEdge::Start(node) => match &node.data.borrow().value {
                NodeValue::Text(t) => text.push_str(t),
                NodeValue::Code(code) => text.push_str(&code.literal),
                NodeValue::Math(math) => text.push_str(&math.literal),
                NodeValue::CodeBlock(code) => text.push_str(&code.literal),
                // A line break, as a newline: `bench::text::words` joins a
                // word broken with a hyphen at a line's end.
                NodeValue::SoftBreak | NodeValue::LineBreak => text.push('\n'),
                NodeValue::HtmlInline(html) if html.starts_with("<br") => text.push('\n'),
                _ => {}
            },
            NodeEdge::End(node) if node.data.borrow().value.block() => text.push('\n'),
            NodeEdge::End(_) => {}
        }
    }
    text
}

/// Whether `swift-shifter doctor`'s report (`doctor`) finds `tool`.
fn found(doctor: &str, tool: &str) -> bool {
    doctor
        .lines()
        .any(|line| line.starts_with(&format!("\u{2713} {tool}: ")))
}

/// Whether the app finds pymupdf4llm, which it reads PDFs with, as its
/// `doctor` says (CI installs it, so there a missing one fails rather
/// than skips): it has no command of its own for [`required`] to find.
fn pymupdf4llm() -> bool {
    let out = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .arg("doctor")
        .output()
        .expect("swift-shifter doctor");
    let has = found(&String::from_utf8_lossy(&out.stdout), "pymupdf4llm");
    if !has {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "pymupdf4llm isn't installed, and CI scores the benchmark with it"
        );
        eprintln!("skipping: pymupdf4llm not found");
    }
    has
}

/// `doc`'s scores, its copy and PDF made under `scratch`. A conversion that
/// fails is in them; scoring that can't be done is an error.
fn score(doc: &Document, xelatex: &Path, scratch: &Path) -> Result<Scores, String> {
    let started = Instant::now();
    let mut scores = Scores {
        dir: doc.dir.clone(),
        ..Scores::default()
    };
    let pdf = compile(doc, xelatex, scratch).map_err(|e| format!("XeLaTeX: {e}"))?;
    match convert(pdf.parent().unwrap(), "md", &pdf)? {
        Ok((markdown, _)) => {
            scores.converted = true;
            let markdown = std::fs::read_to_string(&markdown)
                .map_err(|e| format!("{}: {e}", markdown.display()))?;
            scores.text_similarity = Some(bench::text::similarity(
                &pdf_text(&pdf)?,
                &markdown_text(&markdown),
            ));
        }
        Err(why) => scores.error = Some(why),
    }
    scores.seconds = started.elapsed().as_secs_f64();
    Ok(scores)
}

/// Slow, and needs what the benchmark workflow installs (TeX Live,
/// pymupdf4llm, pdftotext, a display for the CLI's app), so it runs on its
/// own: `cargo test --test bench_pdf -- --ignored`.
#[test]
#[ignore = "converts every benchmark document's PDF; run with --ignored"]
fn pdf_to_markdown_scores() {
    let Some(xelatex) = xelatex() else { return };
    if required("pdftotext").is_none() || !pymupdf4llm() {
        return;
    }
    let manifest = manifest();
    let scratch = tempfile::tempdir().unwrap();
    if let Err(why) = cli_converts(scratch.path()) {
        panic!("the CLI doesn't convert here, so nothing can be scored: {why}");
    }
    let (scores, unscored) = each_document(&manifest.document, |doc| {
        score(doc, &xelatex, scratch.path())
    });
    // Scores for some documents only would read as a different benchmark.
    assert!(
        unscored.is_empty(),
        "{} of {} documents couldn't be scored:\n\n{}",
        unscored.len(),
        manifest.document.len(),
        unscored.join("\n\n")
    );
    let total = total(&scores);
    keep_scores("pdf-md", &scores, &total);

    // To stdout itself, as `keep_scores` writes: the table last.
    let mut log = std::io::stdout().lock();
    writeln!(
        log,
        "{:<20} {:<10} {:>6}  error",
        "document", "converted", "text"
    )
    .unwrap();
    for s in &scores {
        writeln!(
            log,
            "{:<20} {:<10} {:>6}  {}",
            s.dir,
            if s.converted { "yes" } else { "no" },
            s.text_similarity.map_or("-".into(), |t| format!("{t:.3}")),
            // All of it is in the scores above.
            last_line(s.error.as_deref())
        )
        .unwrap();
    }
    writeln!(
        log,
        "{:<20} {:<10} {:>6.3}",
        "total",
        format!(
            "{}/{}",
            scores.iter().filter(|s| s.converted).count(),
            scores.len()
        ),
        total.text_similarity
    )
    .unwrap();
}

#[test]
fn a_document_not_converted_scores_no_text() {
    let doc = |converted, text: Option<f64>| Scores {
        converted,
        text_similarity: text,
        ..Scores::default()
    };
    let scores = [
        doc(true, Some(0.9)),
        doc(true, Some(0.5)),
        doc(false, None),
        doc(false, None),
    ];
    assert_eq!(
        total(&scores),
        Total {
            convert_rate: 0.5,
            text_similarity: 0.35,
            text_similarity_converted: Some(0.7),
        }
    );
    assert_eq!(total(&[]).text_similarity_converted, None);
    assert_eq!(total(&[]).text_similarity.to_string(), "0");
}

#[test]
fn markdown_is_scored_by_its_text_not_its_markup() {
    let pdf = "Title\n\nOne two three\n\nfirst\nsecond\n\na b\n1 2\n\nx^2 y";
    let markdown = "# Title\n\nOne **two** *three*\n\n- first\n- second\n\n\
                    | a | b |\n|---|---|\n| 1 | 2 |\n\n$x^2$ `y`<br>\n";
    assert_eq!(
        bench::text::words(&markdown_text(markdown)),
        bench::text::words(pdf)
    );
    assert_eq!(bench::text::similarity(pdf, &markdown_text(markdown)), 1.0);
    // Words in separate blocks stay apart, and one cut by emphasis whole.
    assert_eq!(markdown_text("a\n\nb").split_whitespace().count(), 2);
    assert_eq!(markdown_text("con**ver**sion").trim(), "conversion");
    // So do words either side of a `<br>`, and one broken with a hyphen
    // at a line's end is joined, as in the PDF's text.
    let words = |markdown| bench::text::words(&markdown_text(markdown));
    assert_eq!(words("alpha<br>beta"), ["alpha", "beta"]);
    assert_eq!(words("con-\nversion"), ["conversion"]);
}

#[test]
fn doctor_says_which_tools_it_finds() {
    let doctor = "\u{2713} pandoc: /usr/bin/pandoc\n\
                  \u{2717} pymupdf4llm isn't installed; it's needed for reading PDFs.\n";
    assert!(found(doctor, "pandoc"));
    assert!(!found(doctor, "pymupdf4llm"));
    assert!(!found("", "pandoc"));
}
