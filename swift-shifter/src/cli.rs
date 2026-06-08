use std::path::Path;
use std::sync::Mutex;

use clap::{Args, Parser, Subcommand};
use tauri::Manager;
use crate::config::{AppState, Config};
use crate::converter;
use crate::converter::{document, media};

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

fn build_config(args: &ConfigArgs) -> Result<Config, String> {
    let mut cfg = Config::default();
    if let Some(d) = &args.output_dir {
        cfg.output_dir = Some(d.clone());
    }
    if let Some(q) = args.jpeg_quality {
        cfg.jpeg_quality = q.clamp(1, 100);
    }
    if let Some(q) = args.avif_quality {
        cfg.avif_quality = q.clamp(1, 100);
    }
    cfg.use_marker_pdf = args.marker;
    cfg.use_local_llm = args.llm;
    if let Some(m) = &args.llm_model {
        cfg.local_llm_model = m.clone();
    }
    if let Some(u) = &args.llm_url {
        let u = u.trim();
        if !u.starts_with("http://") && !u.starts_with("https://") {
            return Err(format!(
                "Invalid --llm-url '{u}': must start with http:// or https://"
            ));
        }
        cfg.local_llm_url = u.to_string();
    }
    Ok(cfg)
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

fn format_check(name: &str, found: Option<&Path>) -> String {
    match found {
        Some(p) => format!("\u{2713} {name}: {}", p.display()),
        None => format!("\u{2717} {name}: not found"),
    }
}

fn run_detect(input: &str) -> i32 {
    match converter::detect_output_formats(input) {
        Ok(formats) => {
            for f in formats {
                println!("{f}");
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn run_merge(inputs: &[String], cfg: &Config) -> i32 {
    match converter::merge_pdfs(inputs, cfg.output_dir.as_deref()) {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn run_duration(input: &str) -> i32 {
    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
    match rt.block_on(media::media_duration_secs(input)) {
        Ok(secs) => {
            println!("{secs}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn run_doctor(cfg: &Config) -> i32 {
    println!(
        "{}",
        format_check("ffmpeg", media::find_ffmpeg().as_deref())
    );
    println!(
        "{}",
        format_check("pandoc", document::find_pandoc_binary().as_deref())
    );
    println!(
        "{}",
        format_check(
            "pymupdf4llm",
            document::find_pymupdf4llm_python().as_deref()
        )
    );
    println!(
        "{}",
        format_check(
            "ebook-convert (Calibre)",
            document::find_ebook_convert_binary().as_deref()
        )
    );
    println!(
        "{}",
        format_check("marker-pdf", document::find_marker_binary().as_deref())
    );

    let rt = tokio::runtime::Runtime::new().expect("failed to create runtime");
    let ollama = rt.block_on(document::ollama_reachable(&cfg.local_llm_url));
    if ollama {
        println!("\u{2713} ollama: reachable at {}", cfg.local_llm_url);
    } else {
        println!("\u{2717} ollama: not reachable at {}", cfg.local_llm_url);
    }
    0
}

/// Build a minimal, windowless Tauri app, run `task` with a real AppHandle,
/// then exit the process with the task's status code.
///
/// The caller must supply `context` from `tauri::generate_context!()` so the
/// plist-embedding symbol is defined exactly once per binary.
pub fn run_with_app<F>(mut context: tauri::Context, cfg: Config, task: F) -> i32
where
    F: FnOnce(tauri::AppHandle) -> std::pin::Pin<Box<dyn std::future::Future<Output = i32> + Send>>
        + Send
        + 'static,
{
    // Clear the window config so no GUI window is created in CLI mode.
    context.config_mut().app.windows.clear();

    tauri::Builder::default()
        .manage(AppState {
            config: Mutex::new(cfg),
            ollama_process: Mutex::new(None),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let code = task(handle).await;
                std::process::exit(code);
            });
            Ok(())
        })
        .build(context)
        .expect("failed to build headless app")
        .run(|_, _| {});

    0 // unreachable: the spawned task calls std::process::exit
}

pub fn run_convert(context: tauri::Context, cfg: Config, format: String, inputs: Vec<String>) -> i32 {
    run_with_app(context, cfg, move |app| {
        Box::pin(async move {
            let state = app.state::<AppState>();
            let cfg = state.config.lock().unwrap().clone();
            let mut failed = false;
            for input in &inputs {
                match converter::convert_file(&app, input, &format, &cfg).await {
                    Ok(out) => println!("{out}"),
                    Err(e) => {
                        eprintln!("{input}: {e}");
                        failed = true;
                    }
                }
            }
            if failed { 1 } else { 0 }
        })
    })
}

pub fn run_trim(context: tauri::Context, cfg: Config, input: String, start: String, end: String) -> i32 {
    run_with_app(context, cfg, move |app| {
        Box::pin(async move {
            let state = app.state::<AppState>();
            let out_dir = state.config.lock().unwrap().output_dir.clone();
            match media::trim_media(&app, &input, &start, &end, out_dir.as_deref()).await {
                Ok(out) => {
                    println!("{out}");
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        })
    })
}

/// Parse args and dispatch. Returns the process exit code.
/// AppHandle-backed commands (`convert`, `trim`) never return — they
/// `std::process::exit` from inside the headless runner.
pub fn run(context: tauri::Context) -> i32 {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // Prints help/version/usage as appropriate and exits.
            e.exit();
        }
    };

    let cfg = match build_config(&cli.cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };

    match cli.command {
        Commands::DetectFormats { input } => run_detect(&input),
        Commands::Merge { inputs } => run_merge(&inputs, &cfg),
        Commands::Duration { input } => run_duration(&input),
        Commands::Doctor => run_doctor(&cfg),
        Commands::Convert { format, inputs } => run_convert(context, cfg, format, inputs),
        Commands::Trim { input, start, end } => run_trim(context, cfg, input, start, end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_args() -> ConfigArgs {
        ConfigArgs {
            output_dir: None,
            jpeg_quality: None,
            avif_quality: None,
            marker: false,
            llm: false,
            llm_model: None,
            llm_url: None,
        }
    }

    #[test]
    fn build_config_clamps_quality() {
        let mut a = cfg_args();
        a.jpeg_quality = Some(200);
        a.avif_quality = Some(0);
        let cfg = build_config(&a).unwrap();
        assert_eq!(cfg.jpeg_quality, 100);
        assert_eq!(cfg.avif_quality, 1);
    }

    #[test]
    fn build_config_rejects_bad_llm_url() {
        let mut a = cfg_args();
        a.llm_url = Some("localhost:11434".to_string());
        assert!(build_config(&a).is_err());
    }

    #[test]
    fn build_config_accepts_good_llm_url() {
        let mut a = cfg_args();
        a.llm_url = Some("http://localhost:11434".to_string());
        let cfg = build_config(&a).unwrap();
        assert_eq!(cfg.local_llm_url, "http://localhost:11434");
    }

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

    use std::path::PathBuf;

    #[test]
    fn format_check_marks_found_and_missing() {
        let found = format_check("ffmpeg", Some(&PathBuf::from("/usr/bin/ffmpeg")));
        assert!(found.contains("ffmpeg"));
        assert!(found.contains("/usr/bin/ffmpeg"));
        assert!(found.starts_with('\u{2713}')); // ✓

        let missing = format_check("pandoc", None);
        assert!(missing.contains("not found"));
        assert!(missing.starts_with('\u{2717}')); // ✗
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
