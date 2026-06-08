use clap::{Args, Parser, Subcommand};

/// Subcommand names that signal CLI (non-GUI) invocation.
const SUBCOMMANDS: &[&str] = &[
    "convert", "detect-formats", "trim", "merge", "duration", "doctor",
];

/// True when the first CLI argument should route to the developer CLI
/// instead of launching the GUI.
pub fn is_cli_invocation() -> bool {
    is_subcommand(std::env::args().nth(1).as_deref())
}

fn is_subcommand(arg: Option<&str>) -> bool {
    match arg {
        Some(a) => {
            SUBCOMMANDS.contains(&a)
                || matches!(a, "-h" | "--help" | "-V" | "--version" | "help")
        }
        None => false,
    }
}

#[derive(Parser)]
#[command(name = "swift-shifter", about = "Swift Shifter developer CLI", version)]
struct Cli {
    #[command(flatten)]
    cfg: ConfigArgs,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Args)]
struct ConfigArgs {
    /// Directory to write outputs to (default: alongside each input).
    #[arg(long, global = true)]
    output_dir: Option<String>,
    /// JPEG quality, 1-100.
    #[arg(long, global = true)]
    jpeg_quality: Option<u8>,
    /// AVIF quality, 1-100.
    #[arg(long, global = true)]
    avif_quality: Option<u8>,
    /// Use marker-pdf for PDF conversion when available.
    #[arg(long, global = true)]
    marker: bool,
    /// Enable Ollama post-processing for PDF conversion.
    #[arg(long, global = true)]
    llm: bool,
    /// Ollama model name.
    #[arg(long, global = true)]
    llm_model: Option<String>,
    /// Ollama base URL (http:// or https://).
    #[arg(long, global = true)]
    llm_url: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Convert one or more files to FORMAT.
    Convert {
        format: String,
        #[arg(required = true, num_args = 1..)]
        inputs: Vec<String>,
    },
    /// Print the valid target formats for an input file.
    DetectFormats { input: String },
    /// Trim a media file between START and END (HH:MM:SS).
    Trim { input: String, start: String, end: String },
    /// Merge two or more PDFs into one.
    Merge {
        #[arg(required = true, num_args = 2..)]
        inputs: Vec<String>,
    },
    /// Print the duration of a media file in seconds.
    Duration { input: String },
    /// Report which external tools are installed.
    Doctor,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_subcommands() {
        assert!(is_subcommand(Some("convert")));
        assert!(is_subcommand(Some("doctor")));
        assert!(is_subcommand(Some("--help")));
    }

    #[test]
    fn ignores_non_subcommands() {
        assert!(!is_subcommand(None));
        assert!(!is_subcommand(Some("/Applications/Swift Shifter.app")));
        assert!(!is_subcommand(Some("-NSDocumentRevisionsDebugMode")));
    }

    #[test]
    fn parses_convert_format_first() {
        let cli = Cli::try_parse_from([
            "swift-shifter", "convert", "webp", "a.png", "b.jpg",
        ])
        .unwrap();
        match cli.command {
            Commands::Convert { format, inputs } => {
                assert_eq!(format, "webp");
                assert_eq!(inputs, vec!["a.png", "b.jpg"]);
            }
            _ => panic!("expected Convert"),
        }
    }
}
