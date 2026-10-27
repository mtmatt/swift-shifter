//! The benchmark's PDF → Markdown scores (#150): each test document's
//! XeLaTeX PDF converted to Markdown with the app's CLI, as a user
//! converts it. Per document and in total:
//! - how alike the Markdown's text is to the PDF's (see `bench::text`);
//! - the formulas the app read and wrote, as its HTML of the Markdown has
//!   them, against those in the LaTeX (see `bench::formulas`), and those
//!   of them it couldn't parse.
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
    /// Why there's no Markdown, or no HTML of it: the conversion that
    /// failed and all it said.
    error: Option<String>,
    /// See `bench::text::similarity`; none where there's no Markdown.
    text_similarity: Option<f64>,
    formulas: Formulas,
    seconds: f64,
}

/// A document's formulas: those in its LaTeX, and those the app read in
/// its PDF and wrote, not the PDF's own, which nothing here can count.
#[derive(serde::Serialize, Default, Debug, PartialEq, Clone, Copy)]
struct Formulas {
    /// In the LaTeX, as `bench::formulas` counts them.
    source: usize,
    /// In the app's HTML of its Markdown (see [`in_html`]); none where
    /// there's no HTML.
    output: usize,
    /// Of those, the ones the app couldn't parse.
    parse_errors: usize,
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
    /// As in `bench_latex_typst`: the formulas kept, each document's as
    /// many as its LaTeX has at most, over those in the LaTeX; none where
    /// the LaTeX has none.
    formulas_kept: Option<f64>,
    /// Of the formulas written, the share that parse; none where there are
    /// none.
    formulas_parse: Option<f64>,
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
    let sum = |count: fn(&Formulas) -> usize| -> usize {
        scores.iter().map(|s| count(&s.formulas)).sum()
    };
    let (source, output) = (sum(|f| f.source), sum(|f| f.output));
    let kept = sum(|f| f.output.min(f.source));
    let parse_errors = sum(|f| f.parse_errors);
    Total {
        convert_rate: converted.len() as f64 / documents,
        text_similarity: similarity / documents,
        text_similarity_converted: (!converted.is_empty())
            .then(|| similarity / converted.len() as f64),
        formulas_kept: (source > 0).then(|| kept as f64 / source as f64),
        formulas_parse: (output > 0).then(|| (output - parse_errors) as f64 / output as f64),
    }
}

/// The formulas in the app's HTML, and of them those it couldn't parse:
/// it writes a formula as MathML (`<math`), or, where it can't parse it,
/// as its TeX (`<code class="math">`).
fn in_html(html: &str) -> (usize, usize) {
    let unparsed = html.matches("<code class=\"math\">").count();
    (html.matches("<math").count() + unparsed, unparsed)
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
    let main = scratch.join(&doc.dir).join(&doc.main);
    scores.formulas.source =
        bench::formulas::in_latex(&main).map_err(|e| format!("formulas: {e}"))?;
    let work = pdf.parent().unwrap();
    let html = match convert(work, "md", &pdf)? {
        Ok((markdown, _)) => {
            scores.converted = true;
            let text = read(&markdown)?;
            scores.text_similarity = Some(bench::text::similarity(
                &pdf_text(&pdf)?,
                &markdown_text(&text),
            ));
            convert(work, "html", &markdown)?
        }
        Err(why) => Err(why),
    };
    match html {
        Ok((html, _)) => {
            (scores.formulas.output, scores.formulas.parse_errors) = in_html(&read(&html)?);
        }
        Err(why) => scores.error = Some(why),
    }
    scores.seconds = started.elapsed().as_secs_f64();
    Ok(scores)
}

fn read(file: &Path) -> Result<String, String> {
    std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))
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
    // Formulas: written / in the LaTeX, and how many don't parse.
    let formulas = |f: Formulas| format!("{}/{} ({})", f.output, f.source, f.parse_errors);
    writeln!(
        log,
        "{:<20} {:<10} {:>6} {:>16}  error",
        "document", "converted", "text", "formulas"
    )
    .unwrap();
    for s in &scores {
        writeln!(
            log,
            "{:<20} {:<10} {:>6} {:>16}  {}",
            s.dir,
            if s.converted { "yes" } else { "no" },
            s.text_similarity.map_or("-".into(), |t| format!("{t:.3}")),
            formulas(s.formulas),
            // All of it is in the scores above.
            last_line(s.error.as_deref())
        )
        .unwrap();
    }
    let share = |share: Option<f64>| share.map_or("-".into(), |s| format!("{s:.3}"));
    writeln!(
        log,
        "{:<20} {:<10} {:>6.3} {:>16}",
        "total",
        format!(
            "{}/{}",
            scores.iter().filter(|s| s.converted).count(),
            scores.len()
        ),
        total.text_similarity,
        format!(
            "{} ({})",
            share(total.formulas_kept),
            share(total.formulas_parse)
        )
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
            formulas_kept: None,
            formulas_parse: None,
        }
    );
    assert_eq!(total(&[]).text_similarity_converted, None);
    assert_eq!(total(&[]).text_similarity.to_string(), "0");
}

#[test]
fn formulas_are_scored_all_added_up() {
    let doc = |source, output, parse_errors| Scores {
        formulas: Formulas {
            source,
            output,
            parse_errors,
        },
        ..Scores::default()
    };
    // 9 of 12 kept, 1 of 9 not parsing; one with more written than in its
    // LaTeX keeps them all, no more.
    let all = total(&[doc(10, 8, 1), doc(2, 1, 0), doc(0, 0, 0)]);
    assert_eq!(all.formulas_kept, Some(0.75));
    assert_eq!(all.formulas_parse, Some(8.0 / 9.0));
    assert_eq!(
        total(&[doc(10, 20, 0), doc(10, 5, 0)]).formulas_kept,
        Some(0.75)
    );
    let none = total(&[doc(0, 0, 0)]);
    assert_eq!((none.formulas_kept, none.formulas_parse), (None, None));
}

#[test]
fn formulas_are_counted_in_the_apps_html() {
    let html = "<p>Let <math display=\"inline\"><mi>x</mi></math> and \
                <code class=\"math\">\\frac{1}</code>.</p>\n\
                <div class=\"math-display\"><math display=\"block\"><mtable></mtable></math></div>\n\
                <pre><code class=\"language-rust\">&lt;math</code></pre>";
    assert_eq!(in_html(html), (3, 1));
    assert_eq!(in_html("<p>none</p>"), (0, 0));
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
