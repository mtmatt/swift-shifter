use std::path::PathBuf;
use std::sync::OnceLock;

pub mod binaries;
pub mod conversion;
pub mod llm;
pub mod merge;
pub mod types;
mod utils;
pub use merge::*;

pub use binaries::*;
pub use conversion::*;
pub use llm::*;
pub use types::*;

pub static PANDOC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static TYPST_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static EBOOK_CONVERT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static PYMUPDF4LLM_PYTHON: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static OLLAMA_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
