//! How the benchmark's tests go through its documents, and what its
//! scorers keep of a run.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::Document;

/// `run` on each of `documents`, on as many threads as there are cores:
/// what each gave, in order of their directories, and where it failed,
/// `<dir>: <why>`, in the same order.
pub fn each_document<T: Send>(
    documents: &[Document],
    run: impl Fn(&Document) -> Result<T, String> + Sync,
) -> (Vec<T>, Vec<String>) {
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get());
    let next = AtomicUsize::new(0);
    let done = Mutex::new(Vec::new());
    let failed = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(doc) = documents.get(next.fetch_add(1, Ordering::Relaxed)) {
                    match run(doc) {
                        Ok(gave) => done.lock().unwrap().push((doc.dir.clone(), gave)),
                        Err(why) => failed
                            .lock()
                            .unwrap()
                            .push((doc.dir.clone(), format!("{}: {why}", doc.dir))),
                    }
                }
            });
        }
    });
    (in_order(done), in_order(failed))
}

/// `found`, each with its document's directory, in their order.
fn in_order<T>(found: Mutex<Vec<(String, T)>>) -> Vec<T> {
    let mut found = found.into_inner().unwrap();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.into_iter().map(|(_, it)| it).collect()
}

/// Keep a run's scores in `target/bench/<name>.json`, with the commit
/// (CI's) and the date, for later runs to compare against, and write them
/// to stdout, for the run's log. To stdout itself, not through `println!`,
/// which the test harness keeps from a test that passes.
pub fn keep_scores(name: &str, documents: &impl serde::Serialize, total: &impl serde::Serialize) {
    let results = serde_json::json!({
        "commit": std::env::var("GITHUB_SHA").ok(),
        "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
        "documents": documents,
        "total": total,
    });
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench");
    std::fs::create_dir_all(&out).unwrap();
    let file = out.join(format!("{name}.json"));
    let json = serde_json::to_string_pretty(&results).unwrap();
    std::fs::write(&file, &json).unwrap();
    let mut log = std::io::stdout().lock();
    writeln!(log, "scores, in {}:\n{json}", file.display()).unwrap();
}

/// A conversion error's last line, for a table: what failed comes after
/// what the app's toolkit says on the way (`dbind-WARNING`).
pub fn last_line(error: Option<&str>) -> &str {
    error
        .and_then(|e| e.lines().rfind(|line| !line.trim().is_empty()))
        .unwrap_or("")
}

#[test]
fn each_document_gives_all_in_order_of_their_directories() {
    let doc = |dir: &str| Document {
        dir: dir.into(),
        main: "main.tex".into(),
        source: String::new(),
        license: String::new(),
        fetched: String::new(),
        covers: Vec::new(),
    };
    let documents: Vec<_> = ["c", "a", "d", "b"].map(doc).into();
    let (done, failed) = each_document(&documents, |doc| match doc.dir.as_str() {
        "d" | "b" => Err("broken".to_string()),
        dir => Ok(dir.to_uppercase()),
    });
    assert_eq!(done, ["A", "C"]);
    assert_eq!(failed, ["b: broken", "d: broken"]);
}

#[test]
fn an_error_is_told_by_its_last_line() {
    let error = "dbind-WARNING: no bus\nconvert pdf: failed\n\n";
    assert_eq!(last_line(Some(error)), "convert pdf: failed");
    assert_eq!(last_line(None), "");
}
