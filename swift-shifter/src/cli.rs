use std::path::Path;
use std::sync::Mutex;

use crate::config::{AppState, Config};
use crate::converter;
use crate::converter::{document, media};
use clap::{Args, Parser, Subcommand};
use tauri::Manager;

/// Subcommand names that signal CLI (non-GUI) invocation.
const SUBCOMMANDS: &[&str] = &[
    "convert",
    "detect-formats",
    "trim",
    "merge",
    "duration",
    "doctor",
];

/// True when the args should route to the developer CLI instead of the GUI.
pub fn is_cli_invocation() -> bool {
    args_select_cli(std::env::args().skip(1))
}

/// Scan all args (not just argv[1]) for a subcommand or help/version token, so
/// global flags placed before the subcommand — e.g.
/// `swift-shifter --jpeg-quality 80 convert in.png` — still route to the CLI.
fn args_select_cli(args: impl Iterator<Item = String>) -> bool {
    args.into_iter().any(|a| is_cli_token(&a))
}

/// True if `arg` is a known subcommand name or a help/version flag.
fn is_cli_token(arg: &str) -> bool {
    SUBCOMMANDS.contains(&arg) || matches!(arg, "-h" | "--help" | "-V" | "--version" | "help")
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
}

fn build_config(args: &ConfigArgs) -> Config {
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
    cfg
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
    Trim {
        input: String,
        start: String,
        end: String,
    },
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

fn run_doctor() -> i32 {
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
        format_check("marker-pdf", document::find_marker_binary().as_deref())
    );
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
    // Windowless, the app still needs a window server on macOS. Without a
    // session (over SSH, say), AppKit ends the process with status 0 while
    // the app is built, past where anything here could say so, so it's
    // checked first. Elsewhere an app that can't start says so itself.
    #[cfg(target_os = "macos")]
    if !has_window_server() {
        eprintln!(
            "{}",
            nothing_done("there's no window server session, as over SSH")
        );
        return 1;
    }

    tauri::Builder::default()
        .manage(AppState {
            config: Mutex::new(cfg),
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
        // `run` would end the process with the loop; this returns.
        .run_return(|_, _| {});
    // The task exits the process once it's done, so the event loop ending
    // first means it stopped partway, having maybe converted some files.
    eprintln!("{STOPPED_PARTWAY}");
    1
}

/// What the CLI says when its app ended before the command finished.
const STOPPED_PARTWAY: &str =
    "swift-shifter: the command stopped before it finished: the app it runs in ended first";

/// Whether this macOS session has a window server, which a session over
/// SSH hasn't.
#[cfg(target_os = "macos")]
fn has_window_server() -> bool {
    use std::ffi::c_void;
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> *const c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }
    // SAFETY: it takes nothing, and returns a dictionary the caller owns,
    // released here, or null where there's no session.
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    if session.is_null() {
        return false;
    }
    unsafe { CFRelease(session) };
    true
}

/// What the CLI says when, for `why`, it did nothing.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn nothing_done(why: &str) -> String {
    format!("swift-shifter: nothing was done: {why}")
}

pub fn run_convert(
    context: tauri::Context,
    cfg: Config,
    format: String,
    inputs: Vec<String>,
) -> i32 {
    run_with_app(context, cfg, move |app| {
        Box::pin(async move {
            let state = app.state::<AppState>();
            let cfg = state.config.lock().unwrap().clone();
            let mut failed = false;
            for input in &inputs {
                let result = converter::convert_file(&app, input, &format, &cfg).await;
                let reported = report(
                    input,
                    result,
                    &mut std::io::stdout(),
                    &mut std::io::stderr(),
                );
                // An output path that couldn't be printed wasn't delivered.
                failed |= !reported.unwrap_or(false);
            }
            if failed { 1 } else { 0 }
        })
    })
}

/// Prints how converting `input` went: the output's path to `out`, and
/// what it left out, a line each, or why it failed, to `err`. Returns
/// whether it succeeded; what it left out doesn't make it fail.
fn report(
    input: &str,
    result: Result<converter::Converted, String>,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> std::io::Result<bool> {
    match result {
        Ok(converted) => {
            writeln!(out, "{}", converted.output)?;
            for warning in &converted.warnings {
                writeln!(err, "{input}: warning: {}", warning.message())?;
            }
            Ok(true)
        }
        Err(e) => {
            writeln!(err, "{input}: {e}")?;
            Ok(false)
        }
    }
}

pub fn run_trim(
    context: tauri::Context,
    cfg: Config,
    input: String,
    start: String,
    end: String,
) -> i32 {
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

    let cfg = build_config(&cli.cfg);

    match cli.command {
        Commands::DetectFormats { input } => run_detect(&input),
        Commands::Merge { inputs } => run_merge(&inputs, &cfg),
        Commands::Duration { input } => run_duration(&input),
        Commands::Doctor => run_doctor(),
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
        }
    }

    #[test]
    fn build_config_clamps_quality() {
        let mut a = cfg_args();
        a.jpeg_quality = Some(200);
        a.avif_quality = Some(0);
        let cfg = build_config(&a);
        assert_eq!(cfg.jpeg_quality, 100);
        assert_eq!(cfg.avif_quality, 1);
    }

    fn select(args: &[&str]) -> bool {
        args_select_cli(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn detects_known_subcommands() {
        assert!(is_cli_token("convert"));
        assert!(is_cli_token("doctor"));
        assert!(is_cli_token("--help"));
    }

    #[test]
    fn ignores_non_subcommands() {
        assert!(!is_cli_token("/Applications/Swift Shifter.app"));
        assert!(!is_cli_token("-NSDocumentRevisionsDebugMode"));
    }

    #[test]
    fn no_args_is_gui() {
        assert!(!select(&[]));
        assert!(!select(&["-NSDocumentRevisionsDebugMode", "YES"]));
    }

    #[test]
    fn global_flags_before_subcommand_select_cli() {
        assert!(select(&[
            "--jpeg-quality",
            "80",
            "convert",
            "in.png",
            "png"
        ]));
        assert!(select(&["convert", "webp", "a.png"]));
    }

    use std::path::PathBuf;

    /// Regression for #118: where its app can't run, the CLI says it did
    /// nothing; where the app ends partway, that it stopped partway.
    #[test]
    fn an_app_that_cant_run_or_ends_first_is_said_so() {
        assert_eq!(
            nothing_done("there's no window server session"),
            "swift-shifter: nothing was done: there's no window server session"
        );
        assert!(!STOPPED_PARTWAY.contains("nothing was done"));
        // This session either has a window server or hasn't; asking
        // mustn't fail.
        #[cfg(target_os = "macos")]
        let _ = has_window_server();
    }

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
        let cli =
            Cli::try_parse_from(["swift-shifter", "convert", "webp", "a.png", "b.jpg"]).unwrap();
        match cli.command {
            Commands::Convert { format, inputs } => {
                assert_eq!(format, "webp");
                assert_eq!(inputs, vec!["a.png", "b.jpg"]);
            }
            _ => panic!("expected Convert"),
        }
    }

    /// What [`report`] prints to stdout and stderr, and whether it
    /// succeeded.
    fn reported(result: Result<converter::Converted, String>) -> (String, String, bool) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let ok = report("in/doc.md", result, &mut out, &mut err).unwrap();
        let text = |bytes| String::from_utf8(bytes).unwrap();
        (text(out), text(err), ok)
    }

    /// A pipe its reader has closed.
    struct Closed;

    impl std::io::Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn an_output_path_that_cant_be_printed_is_an_error() {
        let converted = converter::Converted::whole("out/doc.tex".into());
        let reported = report("in/doc.md", Ok(converted), &mut Closed, &mut Vec::new());
        assert!(reported.is_err());
    }

    #[test]
    fn warnings_go_to_stderr_without_failing() {
        let lost = |count| converter::document::native::Warning {
            one: "SVG image became its alt text",
            many: "SVG images became their alt text",
            count,
            line: None,
        };
        let converted = |warnings| {
            Ok(converter::Converted {
                output: "out/doc.tex".into(),
                warnings,
            })
        };
        assert_eq!(
            reported(converted(vec![lost(1), lost(2)])),
            (
                "out/doc.tex\n".into(),
                "in/doc.md: warning: 1 SVG image became its alt text\n\
                 in/doc.md: warning: 2 SVG images became their alt text\n"
                    .into(),
                true
            )
        );
        assert_eq!(
            reported(converted(Vec::new())),
            ("out/doc.tex\n".into(), String::new(), true)
        );
        assert_eq!(
            reported(Err("no".into())),
            (String::new(), "in/doc.md: no\n".into(), false)
        );
    }
}
