//! The typst `World`: sources, files, fonts and packages for one compile.

use std::any::Any;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use chrono::Datelike;
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{DiagSpan, FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World, WorldExt};
use typst_kit::downloader::Downloader;
use typst_kit::packages::SystemPackages;

use super::{Project, VIRTUAL_DIR, fonts};

static LIBRARY: LazyLock<LazyHash<Library>> = LazyLock::new(|| LazyHash::new(Library::default()));

/// mitex's Typst scope (see `mitex/README.md`), served under
/// `/__shifter__/mitex/` so generated documents can import it offline.
const EMBEDDED: &[(&str, &str)] = &[
    ("mitex/specs/mod.typ", include_str!("mitex/specs/mod.typ")),
    (
        "mitex/specs/prelude.typ",
        include_str!("mitex/specs/prelude.typ"),
    ),
    (
        "mitex/specs/latex/standard.typ",
        include_str!("mitex/specs/latex/standard.typ"),
    ),
    ("mitex/xarrow.typ", include_str!("mitex/xarrow.typ")),
];

pub(super) struct ShifterWorld {
    main: Source,
    root: PathBuf,
    files: HashMap<String, PathBuf>,
    packages: Option<SystemPackages>,
    /// Sources other than main, parsed once per compile.
    sources: Mutex<HashMap<FileId, Source>>,
}

impl ShifterWorld {
    pub fn new(project: Project) -> Self {
        let main_path = project
            .main_name
            .as_deref()
            .and_then(|name| VirtualPath::new(format!("/{name}")).ok())
            .unwrap_or_else(|| VirtualPath::new("/main.typ").expect("valid path"));
        let id = FileId::new(RootedPath::new(VirtualRoot::Project, main_path));
        ShifterWorld {
            main: Source::new(id, project.main),
            root: project.root,
            files: project.files,
            packages: project
                .allow_packages
                .then(|| SystemPackages::new(HttpDownloader)),
            sources: Mutex::new(HashMap::new()),
        }
    }

    pub fn main_id(&self) -> FileId {
        self.main.id()
    }

    /// `(path, line, column)`, 1-based, of where a diagnostic points.
    pub fn line_col(&self, span: DiagSpan) -> Option<(String, usize, usize)> {
        let id = span.id()?;
        let range = self.range(span)?;
        let source = self.source(id).ok()?;
        let line = source.lines().byte_to_line(range.start)?;
        let col = source.lines().byte_to_column(range.start)?;
        let path = id.vpath().get_without_slash().to_string();
        Some((path, line + 1, col + 1))
    }

    fn load_project_file(&self, vpath: &VirtualPath) -> FileResult<Bytes> {
        let with_slash = vpath.get_with_slash();
        if let Some(rest) = with_slash
            .strip_prefix(VIRTUAL_DIR)
            .and_then(|r| r.strip_prefix('/'))
        {
            if let Some((_, text)) = EMBEDDED.iter().find(|(path, _)| *path == rest) {
                return Ok(Bytes::new(text.as_bytes().to_vec()));
            }
            let path = self
                .files
                .get(with_slash)
                .ok_or_else(|| FileError::NotFound(with_slash.into()))?;
            return read(path);
        }
        let path = self.root.join(vpath.get_without_slash());
        read(&path)
    }
}

fn read(path: &std::path::Path) -> FileResult<Bytes> {
    std::fs::read(path)
        .map(Bytes::new)
        .map_err(|e| FileError::from_io(e, path))
}

impl World for ShifterWorld {
    fn library(&self) -> &LazyHash<Library> {
        &LIBRARY
    }

    fn book(&self) -> &LazyHash<FontBook> {
        fonts::store().book()
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            return Ok(self.main.clone());
        }
        if let Some(source) = self.sources.lock().unwrap().get(&id) {
            return Ok(source.clone());
        }
        let bytes = self.file(id)?;
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| FileError::InvalidUtf8)?;
        let source = Source::new(id, text);
        self.sources.lock().unwrap().insert(id, source.clone());
        Ok(source)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        match id.root() {
            VirtualRoot::Project => self.load_project_file(id.vpath()),
            VirtualRoot::Package(spec) => match &self.packages {
                Some(packages) => packages.obtain(spec)?.load(id.vpath()),
                None => Err(FileError::Other(Some(
                    "Typst packages can't be used here".into(),
                ))),
            },
        }
    }

    fn font(&self, index: usize) -> Option<Font> {
        fonts::store().font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        // `offset` is from UTC and may be fractional (e.g. +5:30).
        let date = match offset {
            Some(offset) => {
                let seconds = offset.seconds() as i64;
                (chrono::Utc::now().naive_utc() + chrono::Duration::seconds(seconds)).date()
            }
            None => chrono::Local::now().date_naive(),
        };
        Datetime::from_ymd(date.year(), date.month() as u8, date.day() as u8)
    }
}

/// Fetches typst packages with the HTTP stack the app already uses.
struct HttpDownloader;

impl Downloader for HttpDownloader {
    fn stream(&self, _key: &dyn Any, url: &str) -> io::Result<(Option<usize>, Box<dyn Read>)> {
        let response = reqwest::blocking::Client::builder()
            .user_agent(concat!("swift-shifter/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(io::Error::other)?
            .get(url)
            .send()
            .map_err(io::Error::other)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{url} not found"),
            ));
        }
        let response = response.error_for_status().map_err(io::Error::other)?;
        let size = response
            .content_length()
            .and_then(|n| usize::try_from(n).ok());
        Ok((size, Box::new(response)))
    }
}
