use std::path::PathBuf;
use std::sync::OnceLock;

pub mod binaries;
pub mod conversion;
pub mod merge;
pub mod native;
pub mod pdf;
pub mod types;
mod utils;
pub use merge::*;

pub use binaries::*;
pub use conversion::*;
pub use types::*;

pub static PANDOC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static EBOOK_CONVERT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static PYMUPDF4LLM_PYTHON: OnceLock<Option<PathBuf>> = OnceLock::new();
