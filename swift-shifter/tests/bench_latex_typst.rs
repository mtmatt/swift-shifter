//! The benchmark's LaTeX → Typst scores (#150): each test document
//! converted to Typst with the app's CLI, as a user converts it, and that
//! compiled to PDF, against its XeLaTeX PDF. Per document and in total:
//! - whether the Typst compiled;
//! - how alike the two PDFs' text is (see `bench::text`);
//! - the warnings the conversion reported;
//! - the formulas the Typst kept, and those of them that parse (see
//!   `bench::formulas`).
//!
//! The scores go to `target/bench/latex-typst.json`, for later runs to
//! compare against, and to stdout, as a table and in full, for the run's
//! log. A low score doesn't fail the run: what's wrong is in the scores.

// Shared with the other benchmark tests; not all of it is used here.
#[allow(dead_code)]
mod bench;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use bench::{Document, compile, manifest, xelatex};

/// One document's scores.
#[derive(serde::Serialize, Default)]
struct Scores {
    dir: String,
    compiled: bool,
    /// Why there's no Typst PDF: the conversion that failed and all it
    /// said.
    error: Option<String>,
    /// See `bench::text::similarity`; none where there's nothing to compare.
    text_similarity: Option<f64>,
    formulas: Formulas,
    warnings: Warnings,
    seconds: f64,
}

/// A document's formulas, counted as `bench::formulas` counts them.
#[derive(serde::Serialize, Default, Debug, PartialEq, Clone, Copy)]
struct Formulas {
    /// In the LaTeX.
    source: usize,
    /// In the Typst; none where there's no Typst.
    output: usize,
    /// Of those in the Typst, the ones holding a syntax error.
    parse_errors: usize,
}

#[derive(serde::Serialize, Default, Debug, PartialEq)]
struct Warnings {
    /// What they add up to: each says how many of a thing were lost.
    count: usize,
    messages: Vec<String>,
}

/// The scores of every document together.
#[derive(serde::Serialize, Debug, PartialEq)]
struct Total {
    /// The share of documents whose Typst compiled.
    compile_rate: f64,
    /// The documents' mean, those whose Typst didn't compile counting 0.
    text_similarity: f64,
    /// The mean of those whose Typst compiled.
    text_similarity_compiled: Option<f64>,
    /// The documents' formulas kept over those in the LaTeX, all added up;
    /// none where the LaTeX has none. A document keeps as many as its
    /// Typst has, as many as its LaTeX has at most, so that one with more
    /// (a formula split in two) doesn't make up for others' losses.
    formulas_kept: Option<f64>,
    /// Of the formulas in the Typst, all added up, the share that parse;
    /// none where there are none.
    formulas_parse: Option<f64>,
    warnings: usize,
}

/// The conversion's warnings in the CLI's `stderr`: its lines
/// `<input>: warning: <N> <what>`.
fn warnings(stderr: &str) -> Warnings {
    let mut warnings = Warnings::default();
    for line in stderr.lines() {
        let Some((_, message)) = line.split_once(": warning: ") else {
            continue;
        };
        let count = message.split(' ').next().and_then(|n| n.parse().ok());
        warnings.count += count.unwrap_or(1);
        warnings.messages.push(message.to_string());
    }
    warnings
}

fn total(scores: &[Scores]) -> Total {
    let documents = scores.len().max(1) as f64;
    let compiled: Vec<f64> = scores
        .iter()
        .filter(|s| s.compiled)
        .map(|s| s.text_similarity.unwrap_or(0.0))
        .collect();
    let similarity: f64 = compiled.iter().sum();
    let formulas = scores.iter().fold(Formulas::default(), |all, s| Formulas {
        source: all.source + s.formulas.source,
        output: all.output + s.formulas.output,
        parse_errors: all.parse_errors + s.formulas.parse_errors,
    });
    let kept: usize = scores
        .iter()
        .map(|s| s.formulas.output.min(s.formulas.source))
        .sum();
    Total {
        compile_rate: compiled.len() as f64 / documents,
        text_similarity: similarity / documents,
        text_similarity_compiled: (!compiled.is_empty())
            .then(|| similarity / compiled.len() as f64),
        formulas_kept: (formulas.source > 0).then(|| kept as f64 / formulas.source as f64),
        formulas_parse: (formulas.output > 0)
            .then(|| (formulas.output - formulas.parse_errors) as f64 / formulas.output as f64),
        warnings: scores.iter().map(|s| s.warnings.count).sum(),
    }
}

/// The CLI, run in `dir`, as `swift-shifter convert <format> <input>`: the
/// file it made and its stderr, or why the conversion failed. An error
/// where the CLI couldn't be run.
fn convert(
    dir: &Path,
    format: &str,
    input: &Path,
) -> Result<Result<(PathBuf, String), String>, String> {
    let out = Command::new(env!("CARGO_BIN_EXE_swift-shifter"))
        .args(["convert", format])
        .arg(input)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("swift-shifter: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Ok(Err(format!("convert {format}: {}", stderr.trim())));
    }
    let made = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(Ok((PathBuf::from(made), stderr)))
}

/// Whether the CLI converts at all here, tried on a line of plain text:
/// where its app can't start (no display, no window server), every
/// document would fail for that, not as converted, so nothing is scored.
fn cli_converts(scratch: &Path) -> Result<(), String> {
    let probe = scratch.join("probe.txt");
    std::fs::write(&probe, "probe\n").map_err(|e| e.to_string())?;
    match convert(scratch, "md", &probe)? {
        Ok((made, _)) if made.is_file() => Ok(()),
        Ok((made, _)) => Err(format!("the CLI made no {}", made.display())),
        Err(why) => Err(why),
    }
}

/// `name`, a tool scoring needs, or `None` where it isn't installed (CI
/// installs it, so there a missing one fails rather than skips): without
/// it, every conversion would fail for want of it, not as converted.
fn required(name: &str) -> Option<PathBuf> {
    let found = which::which(name).ok();
    if found.is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "{name} isn't installed, and CI scores the benchmark with it"
        );
        eprintln!("skipping: {name} not found");
    }
    found
}

/// `pdf`'s text, as `pdftotext` gives it.
fn pdf_text(pdf: &Path) -> Result<String, String> {
    let out = Command::new("pdftotext")
        .args(["-enc", "UTF-8"])
        .arg(pdf)
        .arg("-")
        .output()
        .map_err(|e| format!("pdftotext: {e}"))?;
    if !out.status.success() {
        return Err(format!("pdftotext {}: failed", pdf.display()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `doc`'s scores, its copy and PDFs made under `scratch`. A conversion
/// that fails is in them; scoring that can't be done (no answer key, a CLI
/// that can't be run, no text from a PDF) is an error, as no score would
/// say how the conversion went.
fn score(doc: &Document, xelatex: &Path, scratch: &Path) -> Result<Scores, String> {
    let started = Instant::now();
    let mut scores = Scores {
        dir: doc.dir.clone(),
        ..Scores::default()
    };
    let answer = compile(doc, xelatex, scratch).map_err(|e| format!("XeLaTeX: {e}"))?;
    // The Typst PDF is written where the XeLaTeX one is.
    let kept = answer.with_file_name("xelatex.pdf");
    std::fs::rename(&answer, &kept).map_err(|e| format!("{}: {e}", answer.display()))?;
    let main = scratch.join(&doc.dir).join(&doc.main);
    let work = main.parent().unwrap();
    scores.formulas.source =
        bench::formulas::in_latex(&main).map_err(|e| format!("formulas: {e}"))?;
    let converted = match convert(work, "typst", &main)? {
        Ok((typst, stderr)) => {
            scores.warnings = warnings(&stderr);
            // Counted whether or not it compiles.
            let source =
                std::fs::read_to_string(&typst).map_err(|e| format!("{}: {e}", typst.display()))?;
            (scores.formulas.output, scores.formulas.parse_errors) =
                bench::formulas::in_typst(&source);
            convert(work, "pdf", &typst)?
        }
        Err(why) => Err(why),
    };
    match converted {
        Ok((pdf, _)) => {
            scores.compiled = true;
            scores.text_similarity =
                Some(bench::text::similarity(&pdf_text(&kept)?, &pdf_text(&pdf)?));
        }
        Err(why) => scores.error = Some(why),
    }
    scores.seconds = started.elapsed().as_secs_f64();
    Ok(scores)
}

/// Slow, and needs what the benchmark workflow installs (TeX Live,
/// pandoc, pdftotext, a display for the CLI's app), so it runs on its
/// own: `cargo test --test bench_latex_typst -- --ignored`.
#[test]
#[ignore = "converts every benchmark document; run with --ignored"]
fn latex_to_typst_scores() {
    let Some(xelatex) = xelatex() else { return };
    // `.tex` converts through pandoc until the app's LaTeX reader does (#73).
    if required("pandoc").is_none() || required("pdftotext").is_none() {
        return;
    }
    let manifest = manifest();
    let scratch = tempfile::tempdir().unwrap();
    if let Err(why) = cli_converts(scratch.path()) {
        panic!("the CLI doesn't convert here, so nothing can be scored: {why}");
    }
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let scores = std::sync::Mutex::new(Vec::new());
    let unscored = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(doc) = manifest
                    .document
                    .get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                {
                    match score(doc, &xelatex, scratch.path()) {
                        Ok(scored) => scores.lock().unwrap().push(scored),
                        Err(why) => unscored.lock().unwrap().push(format!("{}: {why}", doc.dir)),
                    }
                }
            });
        }
    });
    // Scores for some documents only would read as a different benchmark.
    let unscored = unscored.into_inner().unwrap();
    assert!(
        unscored.is_empty(),
        "{} of {} documents couldn't be scored:\n\n{}",
        unscored.len(),
        manifest.document.len(),
        unscored.join("\n\n")
    );
    let mut scores = scores.into_inner().unwrap();
    scores.sort_by(|a, b| a.dir.cmp(&b.dir));
    let total = total(&scores);

    let results = serde_json::json!({
        "commit": std::env::var("GITHUB_SHA").ok(),
        "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
        "documents": scores,
        "total": total,
    });
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench");
    std::fs::create_dir_all(&out).unwrap();
    let file = out.join("latex-typst.json");
    let json = serde_json::to_string_pretty(&results).unwrap();
    std::fs::write(&file, &json).unwrap();

    // To stdout itself, not through `println!`, which the test harness
    // keeps from a test that passes: the scores are for the run's log.
    // The table comes last, where a long log's end shows it.
    let mut log = std::io::stdout().lock();
    writeln!(log, "scores, in {}:\n{json}", file.display()).unwrap();
    // Formulas: in the Typst / in the LaTeX, and how many don't parse.
    let formulas = |f: Formulas| format!("{}/{} ({})", f.output, f.source, f.parse_errors);
    writeln!(
        log,
        "{:<20} {:<9} {:>6} {:>16} {:>9}  error",
        "document", "compiled", "text", "formulas", "warnings"
    )
    .unwrap();
    for s in &scores {
        writeln!(
            log,
            "{:<20} {:<9} {:>6} {:>16} {:>9}  {}",
            s.dir,
            if s.compiled { "yes" } else { "no" },
            s.text_similarity.map_or("-".into(), |t| format!("{t:.3}")),
            formulas(s.formulas),
            s.warnings.count,
            // Its last line: what failed comes after what the app's
            // toolkit says on the way (`dbind-WARNING`). All of it is in
            // the scores above.
            s.error
                .as_deref()
                .and_then(|e| e.lines().rfind(|line| !line.trim().is_empty()))
                .unwrap_or("")
        )
        .unwrap();
    }
    let share = |share: Option<f64>| share.map_or("-".into(), |s| format!("{s:.3}"));
    writeln!(
        log,
        "{:<20} {:<9} {:>6.3} {:>16} {:>9}",
        "total",
        format!(
            "{}/{}",
            scores.iter().filter(|s| s.compiled).count(),
            scores.len()
        ),
        total.text_similarity,
        format!(
            "{} ({})",
            share(total.formulas_kept),
            share(total.formulas_parse)
        ),
        total.warnings
    )
    .unwrap();
}

#[test]
fn warnings_are_read_from_the_cli() {
    let stderr = "main.tex: warning: 2 SVG images became their alt text\n\
                  other line\n\
                  main.tex: warning: 1 list nested too deeply was read as its text at line 4\n";
    let read = warnings(stderr);
    assert_eq!(read.count, 3);
    assert_eq!(
        read.messages,
        [
            "2 SVG images became their alt text",
            "1 list nested too deeply was read as its text at line 4"
        ]
    );
    assert_eq!(warnings(""), Warnings::default());
}

#[test]
fn a_document_that_doesnt_compile_scores_no_text() {
    let doc = |compiled, text: Option<f64>, warnings| Scores {
        compiled,
        text_similarity: text,
        warnings: Warnings {
            count: warnings,
            messages: Vec::new(),
        },
        ..Scores::default()
    };
    let scores = [
        doc(true, Some(0.9), 1),
        doc(true, Some(0.5), 0),
        doc(false, None, 2),
        doc(false, None, 0),
    ];
    assert_eq!(
        total(&scores),
        Total {
            compile_rate: 0.5,
            text_similarity: 0.35,
            text_similarity_compiled: Some(0.7),
            formulas_kept: None,
            formulas_parse: None,
            warnings: 3,
        }
    );
    assert_eq!(total(&[]).text_similarity_compiled, None);
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
    // 9 of 12 kept, 1 of 9 not parsing, whether the Typst compiled or not.
    let all = total(&[doc(10, 8, 1), doc(2, 1, 0), doc(0, 0, 0)]);
    assert_eq!(all.formulas_kept, Some(0.75));
    assert_eq!(all.formulas_parse, Some(8.0 / 9.0));
    // More in the Typst than in the LaTeX keeps them all, no more, and
    // doesn't make up for another document's losses.
    assert_eq!(total(&[doc(2, 3, 0)]).formulas_kept, Some(1.0));
    assert_eq!(
        total(&[doc(10, 20, 0), doc(10, 5, 0)]).formulas_kept,
        Some(0.75)
    );
    // None to keep, or none kept, is no share.
    let none = total(&[doc(0, 0, 0)]);
    assert_eq!((none.formulas_kept, none.formulas_parse), (None, None));
}
