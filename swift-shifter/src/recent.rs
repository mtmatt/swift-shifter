//! Recently modified files, for the quick bar to suggest before the user types
//! and to search as they do.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::converter::graph;

/// How many files `scan` returns at most. The quick bar searches this set
/// client-side, so it's large enough to find older files by name.
pub const MAX_RESULTS: usize = 500;

/// Folder levels below each root that get scanned (the root itself is 0).
const MAX_DEPTH: usize = 3;

/// Upper bound on directory entries visited under each root, so a huge
/// Documents tree can't stall the bar or starve the roots after it.
const MAX_ENTRIES_PER_ROOT: usize = 10_000;

/// Directories that are really documents or tool caches, not folders a user
/// browses for files to convert.
const SKIPPED_DIR_EXTENSIONS: &[&str] = &[
    "app",
    "bundle",
    "framework",
    "photoslibrary",
    "musiclibrary",
    "xcodeproj",
    "xcworkspace",
];
const SKIPPED_DIR_NAMES: &[&str] = &["node_modules", "target", "__pycache__"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecentFile {
    pub path: String,
    pub name: String,
    /// Containing folder for display, with the home directory shown as `~`.
    pub folder: String,
    /// Last modification time, in milliseconds since the Unix epoch.
    pub modified_ms: u64,
}

/// The folders scanned by default: Desktop, Documents and Downloads, where
/// they exist.
pub fn default_roots() -> Vec<PathBuf> {
    [
        dirs::desktop_dir(),
        dirs::document_dir(),
        dirs::download_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Files under `roots` that Swift Shifter can convert, newest first, at most
/// `limit` of them. Hidden files and folders, app bundles and the like are
/// skipped, and symlinks are not followed. A file under several roots (e.g. a
/// folder and a subfolder of it) is listed once.
pub fn scan(roots: &[PathBuf], limit: usize) -> Vec<RecentFile> {
    let home = dirs::home_dir();
    let mut walk = Walk {
        convertible_by_ext: HashMap::new(),
        entries_left: 0,
        seen: HashSet::new(),
        found: Vec::new(),
    };
    // Every root gets its own walk and budget, even one nested in another:
    // the outer walk may stop at MAX_DEPTH or run out of budget before it.
    for root in roots {
        walk.entries_left = MAX_ENTRIES_PER_ROOT;
        walk.visit_dir(root, 0);
    }

    let mut found = walk.found;
    found.sort_by_key(|&(_, modified_ms)| std::cmp::Reverse(modified_ms));
    found.truncate(limit);
    found
        .into_iter()
        .map(|(path, modified_ms)| to_recent_file(&path, modified_ms, home.as_deref()))
        .collect()
}

struct Walk {
    /// Memoised `graph::direct_targets(ext)` emptiness, keyed by lowercase extension.
    convertible_by_ext: HashMap<String, bool>,
    entries_left: usize,
    /// Files already in `found`, so overlapping roots don't list them twice.
    seen: HashSet<PathBuf>,
    /// Convertible files and their modification times in ms.
    found: Vec<(PathBuf, u64)>,
}

impl Walk {
    fn visit_dir(&mut self, dir: &Path, depth: usize) {
        // Unreadable folders (permissions, races with deletion) are skipped.
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if self.entries_left == 0 {
                return;
            }
            self.entries_left -= 1;

            let path = entry.path();
            if is_hidden(&path) {
                continue;
            }
            // `DirEntry::file_type` does not follow symlinks, so links are
            // neither files nor directories here and get skipped.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_DEPTH && !is_skipped_dir(&path) {
                    self.visit_dir(&path, depth + 1);
                }
            } else if file_type.is_file()
                && self.is_convertible(&path)
                && let Some(modified_ms) = modified_ms(&entry)
                && self.seen.insert(path.clone())
            {
                self.found.push((path, modified_ms));
            }
        }
    }

    fn is_convertible(&mut self, path: &Path) -> bool {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let ext = ext.to_lowercase();
        *self
            .convertible_by_ext
            .entry(ext)
            .or_insert_with_key(|ext| !graph::direct_targets(ext).is_empty())
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

fn is_skipped_dir(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    SKIPPED_DIR_NAMES.contains(&name)
        || SKIPPED_DIR_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

fn modified_ms(entry: &std::fs::DirEntry) -> Option<u64> {
    let modified = entry.metadata().ok()?.modified().ok()?;
    let since_epoch = modified.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since_epoch.as_millis()).ok()
}

fn to_recent_file(path: &Path, modified_ms: u64, home: Option<&Path>) -> RecentFile {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let parent = path.parent().unwrap_or(Path::new(""));
    RecentFile {
        path: path.to_string_lossy().into_owned(),
        name,
        folder: display_folder(parent, home),
        modified_ms,
    }
}

/// `parent` with the home directory replaced by `~`.
fn display_folder(parent: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| parent.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => Path::new("~").join(rest).to_string_lossy().into_owned(),
        None => parent.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, FileTimes};
    use std::time::{Duration, SystemTime};

    /// Create `rel` under `root`, last modified `age_s` seconds ago.
    fn touch(root: &Path, rel: &str, age_s: u64) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = File::create(&path).unwrap();
        let modified = SystemTime::now() - Duration::from_secs(age_s);
        file.set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        path
    }

    fn names(files: &[RecentFile]) -> Vec<&str> {
        files.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn lists_convertible_files_newest_first() {
        let root = tempfile::tempdir().unwrap();
        touch(root.path(), "old.png", 300);
        touch(root.path(), "new.jpg", 10);
        touch(root.path(), "middle.md", 100);
        touch(root.path(), "notes.unknownext", 1);
        // MOBI isn't converted any more (#133): a recent one isn't offered.
        touch(root.path(), "book.mobi", 2);

        let files = scan(&[root.path().to_path_buf()], MAX_RESULTS);

        assert_eq!(names(&files), ["new.jpg", "middle.md", "old.png"]);
    }

    #[test]
    fn respects_the_limit() {
        let root = tempfile::tempdir().unwrap();
        for age in 1..=5 {
            touch(root.path(), &format!("img{age}.png"), age);
        }

        let files = scan(&[root.path().to_path_buf()], 2);

        assert_eq!(names(&files), ["img1.png", "img2.png"]);
    }

    #[test]
    fn skips_hidden_bundles_and_folders_past_max_depth() {
        let root = tempfile::tempdir().unwrap();
        touch(root.path(), "a/b/c/deep-enough.png", 1);
        touch(root.path(), "a/b/c/d/too-deep.png", 1);
        touch(root.path(), ".hidden.png", 1);
        touch(root.path(), ".cache/inside-hidden.png", 1);
        touch(root.path(), "Tool.app/Contents/icon.png", 1);
        touch(root.path(), "project/node_modules/pkg/logo.png", 1);

        let files = scan(&[root.path().to_path_buf()], MAX_RESULTS);

        assert_eq!(names(&files), ["deep-enough.png"]);
    }

    #[test]
    fn merges_several_roots_and_ignores_missing_ones() {
        let downloads = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        touch(downloads.path(), "download.pdf", 50);
        touch(desktop.path(), "screenshot.png", 5);
        let missing = downloads.path().join("does-not-exist");

        let roots = [
            downloads.path().to_path_buf(),
            missing,
            desktop.path().to_path_buf(),
        ];
        let files = scan(&roots, MAX_RESULTS);

        assert_eq!(names(&files), ["screenshot.png", "download.pdf"]);
    }

    #[test]
    fn nested_roots_list_each_file_once() {
        let documents = tempfile::tempdir().unwrap();
        touch(documents.path(), "work/report.pdf", 5);
        let work = documents.path().join("work");

        // Either order: the inner folder adds nothing the outer one doesn't cover.
        for roots in [
            vec![documents.path().to_path_buf(), work.clone()],
            vec![work.clone(), documents.path().to_path_buf()],
        ] {
            assert_eq!(names(&scan(&roots, MAX_RESULTS)), ["report.pdf"]);
        }
    }

    #[test]
    fn a_root_nested_past_max_depth_is_still_scanned() {
        let home = tempfile::tempdir().unwrap();
        touch(home.path(), "a/b/c/d/clients/acme/brief.pdf", 5);
        let acme = home.path().join("a/b/c/d/clients/acme");

        let files = scan(&[home.path().to_path_buf(), acme], MAX_RESULTS);

        assert_eq!(names(&files), ["brief.pdf"]);
    }

    #[test]
    fn reports_path_folder_and_modified_time() {
        let root = tempfile::tempdir().unwrap();
        let path = touch(root.path(), "pics/cat.png", 60);

        let files = scan(&[root.path().to_path_buf()], MAX_RESULTS);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, path.to_string_lossy());
        // The temp dir is under home on Windows, so `~` may apply.
        let home = dirs::home_dir();
        assert_eq!(
            files[0].folder,
            display_folder(&root.path().join("pics"), home.as_deref())
        );
        let expected_ms = (SystemTime::now() - Duration::from_secs(60))
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(files[0].modified_ms.abs_diff(expected_ms) < 5_000);
    }

    #[test]
    fn display_folder_abbreviates_home() {
        let home = Path::new("/home/me");
        assert_eq!(display_folder(Path::new("/home/me"), Some(home)), "~");
        assert_eq!(
            display_folder(Path::new("/home/me/Downloads/sub"), Some(home)),
            Path::new("~/Downloads/sub").to_string_lossy()
        );
        assert_eq!(
            display_folder(Path::new("/srv/data"), Some(home)),
            "/srv/data"
        );
        assert_eq!(display_folder(Path::new("/srv/data"), None), "/srv/data");
    }
}
