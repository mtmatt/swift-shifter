use std::path::PathBuf;
use std::sync::OnceLock;

pub mod types;
mod utils;
pub mod binaries;
pub mod llm;
pub mod conversion;

pub use types::*;
pub use binaries::*;
pub use llm::*;
pub use conversion::*;

pub static PANDOC_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static PDFTOHTML_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static EBOOK_CONVERT_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
pub static OLLAMA_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
