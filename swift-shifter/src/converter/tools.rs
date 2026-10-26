//! The external tools some conversions run, and how to install one that's
//! missing: this module finds a tool the user has, and says how to install
//! one they don't.

use std::path::PathBuf;

use super::{document, media};

/// An external tool a conversion runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ffmpeg,
    Pandoc,
    Pymupdf4llm,
    Marker,
}

impl Tool {
    pub const ALL: [Tool; 4] = [Tool::Ffmpeg, Tool::Pandoc, Tool::Pymupdf4llm, Tool::Marker];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "ffmpeg",
            Tool::Pandoc => "pandoc",
            Tool::Pymupdf4llm => "pymupdf4llm",
            Tool::Marker => "marker-pdf",
        }
    }

    fn needed_for(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "audio and video",
            Tool::Pandoc => "LaTeX (.tex) documents",
            Tool::Pymupdf4llm => "reading PDFs",
            Tool::Marker => "PDF to EPUB with marker (Settings, engines)",
        }
    }

    /// How to install it on this platform: a command, and where to read more.
    pub fn install_hint(self) -> &'static str {
        let macos = cfg!(target_os = "macos");
        let windows = cfg!(target_os = "windows");
        match self {
            Tool::Ffmpeg if macos => {
                "Install it with: brew install ffmpeg (or see https://ffmpeg.org/download.html)"
            }
            Tool::Ffmpeg if windows => {
                "Install it with: winget install Gyan.FFmpeg (or see https://ffmpeg.org/download.html)"
            }
            Tool::Ffmpeg => {
                "Install it with your package manager, e.g. sudo apt install ffmpeg (or see https://ffmpeg.org/download.html)"
            }
            Tool::Pandoc if macos => {
                "Install it with: brew install pandoc (or see https://pandoc.org/installing.html)"
            }
            Tool::Pandoc if windows => {
                "Install it with: winget install JohnMacFarlane.Pandoc (or see https://pandoc.org/installing.html)"
            }
            Tool::Pandoc => {
                "Install it with your package manager, e.g. sudo apt install pandoc (or see https://pandoc.org/installing.html)"
            }
            // pymupdf4llm has no command of its own: pipx needs --include-deps.
            Tool::Pymupdf4llm => {
                "Install it with: pipx install pymupdf4llm --include-deps (pipx: https://pipx.pypa.io)"
            }
            Tool::Marker => {
                "Install it with: pipx install marker-pdf, then: pipx inject marker-pdf psutil (pipx: https://pipx.pypa.io)"
            }
        }
    }

    /// The error a conversion that needs the tool returns when it's missing.
    pub fn missing(self) -> String {
        format!(
            "{} isn't installed; it's needed for {}. {}",
            self.name(),
            self.needed_for(),
            self.install_hint()
        )
    }

    /// Where the tool is, if the user has it.
    pub fn find(self) -> Option<PathBuf> {
        match self {
            Tool::Ffmpeg => media::find_ffmpeg(),
            Tool::Pandoc => document::find_pandoc_binary(),
            Tool::Pymupdf4llm => document::find_pymupdf4llm_python(),
            Tool::Marker => document::find_marker_binary(),
        }
    }

    /// The tool found, or [`Tool::missing`].
    pub fn require(self, found: Option<PathBuf>) -> Result<PathBuf, String> {
        found.ok_or_else(|| self.missing())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tool_says_what_needs_it_and_how_to_install_it() {
        for tool in Tool::ALL {
            let err = tool.require(None).unwrap_err();
            assert!(err.starts_with(tool.name()), "{err}");
            assert!(err.contains(tool.needed_for()), "{err}");
            assert!(err.ends_with(tool.install_hint()), "{err}");
            assert!(tool.install_hint().contains("https://"), "{err}");
        }
        let found = PathBuf::from("/bin/ffmpeg");
        assert_eq!(Tool::Ffmpeg.require(Some(found.clone())), Ok(found));
    }

    #[test]
    fn the_hint_is_for_this_platform() {
        let hint = Tool::Ffmpeg.install_hint();
        if cfg!(target_os = "macos") {
            assert!(hint.contains("brew install ffmpeg"), "{hint}");
        } else if cfg!(target_os = "windows") {
            assert!(hint.contains("winget install"), "{hint}");
        } else {
            assert!(!hint.contains("brew") && !hint.contains("winget"), "{hint}");
        }
    }
}
