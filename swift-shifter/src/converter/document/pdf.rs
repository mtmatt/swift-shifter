//! Choosing pandoc's PDF engine for a document, including the font setup
//! that CJK (Chinese/Japanese/Korean) text needs.
//!
//! Pandoc's default fonts have no CJK glyphs, and each engine fails
//! differently: pdflatex errors out, xelatex/tectonic silently drop the
//! characters, and typst substitutes whatever system font it finds first
//! (often a mismatched mix of weights and scripts). So when a document
//! contains CJK text we give the engine an explicit CJK font, unless the
//! document picks its own fonts.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(target_os = "macos")]
use crate::converter::document::binaries::BREW_PATHS;
use crate::converter::document::binaries::find_typst_binary;

/// PDF engines pandoc can drive, most preferred first: LaTeX engines give
/// the best fidelity for .tex input, typst is a capable, dependency-light
/// fallback (and the one Swift Shifter installs itself).
const ENGINES: &[&str] = &[
    "tectonic",
    "xelatex",
    "pdflatex",
    "lualatex",
    "wkhtmltopdf",
    "typst",
];

/// Engine preference for documents with CJK text. pdflatex can't typeset
/// it at all. typst comes first because it is the only engine we can ask
/// which fonts are installed, except for LaTeX input, where a
/// XeLaTeX-family engine keeps raw LaTeX that the typst writer would drop.
const CJK_ENGINES: &[&str] = &["typst", "tectonic", "xelatex", "lualatex", "wkhtmltopdf"];
const CJK_LATEX_INPUT_ENGINES: &[&str] =
    &["tectonic", "xelatex", "lualatex", "typst", "wkhtmltopdf"];

/// CJK font families to try, most preferred first, across macOS, Windows
/// and common Linux packages. Serif faces lead to match typst's default
/// Latin font (Libertinus Serif); Korean-capable fonts come last because
/// the Chinese fonts before them lack Hangul.
const CJK_FONTS: &[&str] = &[
    "Noto Serif CJK SC",
    "Source Han Serif SC",
    "Songti SC",
    "STSong",
    "SimSun",
    "AR PL UMing CN",
    "Noto Sans CJK SC",
    "Source Han Sans SC",
    "PingFang SC",
    "Hiragino Sans GB",
    "Microsoft YaHei",
    "WenQuanYi Zen Hei",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
    "Noto Serif CJK TC",
    "Songti TC",
    "Microsoft JhengHei",
    "Hiragino Mincho ProN",
    "Yu Mincho",
    "Noto Sans CJK KR",
    "Apple SD Gothic Neo",
    "Malgun Gothic",
    "NanumGothic",
];

/// CJK font for the LaTeX engines, which can't report what's installed.
/// These ship with the OS (or the usual Noto CJK package on Linux).
#[cfg(target_os = "macos")]
const LATEX_CJK_FONT: &str = "Songti SC";
#[cfg(target_os = "windows")]
const LATEX_CJK_FONT: &str = "SimSun";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const LATEX_CJK_FONT: &str = "Noto Serif CJK SC";

const PDFLATEX_ONLY: &str = "This document contains Chinese, Japanese or Korean text, which \
     pdflatex can't typeset. Install typst (or XeLaTeX) and try again.";

/// An installed PDF engine: pandoc's name for it and the binary to run.
#[derive(Debug, Clone, PartialEq)]
struct PdfEngine {
    name: &'static str,
    program: PathBuf,
}

/// What the PDF setup needs to know about the document being converted.
#[derive(Debug, Default)]
struct SourceDocument {
    has_cjk: bool,
    is_latex: bool,
    /// Top-level keys of the document's YAML front matter. Font variables
    /// the document sets itself are left alone, since `-V` would override
    /// them.
    metadata_keys: HashSet<String>,
}

/// What typst's font setup depends on, beyond `--pdf-engine typst`.
#[derive(Debug, Default)]
struct TypstSetup {
    /// CJK fonts typst can see, when the document has CJK text.
    cjk: Vec<String>,
    /// Pandoc's typst template has no usable default font (see
    /// `typst_template_needs_mainfont`).
    template_needs_mainfont: bool,
}

/// Set up a `pandoc` command producing a PDF from `input`: pick the engine
/// (and, for CJK text, its fonts) and make sure pandoc can find it. Pass
/// `None` for generated input with no text of its own. Leaves pandoc's
/// default engine when none is installed.
pub async fn configure_pandoc_pdf(
    cmd: &mut tokio::process::Command,
    pandoc: &Path,
    input: Option<&Path>,
) -> Result<(), String> {
    let engines = available_engines();
    let doc = input.map(SourceDocument::inspect).unwrap_or_default();
    let typst_setup = match engines.iter().find(|e| e.name == "typst") {
        Some(typst) => TypstSetup {
            cjk: if doc.has_cjk {
                installed_cjk_fonts(&typst.program).await
            } else {
                Vec::new()
            },
            template_needs_mainfont: typst_template_needs_mainfont(pandoc).await,
        },
        None => TypstSetup::default(),
    };
    cmd.args(pdf_args(&engines, &doc, &typst_setup)?);
    if let Some(path) = pandoc_search_path(std::env::var_os("PATH"), &engines) {
        cmd.env("PATH", path);
    }
    Ok(())
}

/// Every installed PDF engine, in `ENGINES` preference order.
fn available_engines() -> Vec<PdfEngine> {
    ENGINES
        .iter()
        .filter_map(|&name| find_engine(name).map(|program| PdfEngine { name, program }))
        .collect()
}

fn find_engine(name: &str) -> Option<PathBuf> {
    // typst also lives in managed/user dirs that aren't on PATH.
    if name == "typst" {
        return find_typst_binary();
    }
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    #[cfg(target_os = "macos")]
    for dir in BREW_PATHS {
        let candidate = Path::new(dir).join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// The pandoc arguments for the chosen engine and its fonts.
///
/// Pandoc gets the engine's name, not its path (see `pandoc_search_path`
/// for how it finds engines that aren't on PATH): pandoc only keeps image
/// paths relative when the engine is named exactly `typst`. Given a path,
/// it extracts images to absolute temp paths that typst refuses to read,
/// so e.g. png -> pdf fails.
fn pdf_args(
    engines: &[PdfEngine],
    doc: &SourceDocument,
    typst_setup: &TypstSetup,
) -> Result<Vec<String>, String> {
    let Some(engine) = choose_engine(engines, doc) else {
        if doc.has_cjk && !engines.is_empty() {
            return Err(PDFLATEX_ONLY.to_string());
        }
        // Leave the error to pandoc, as for any document without an engine.
        return Ok(Vec::new());
    };
    let mut args = vec!["--pdf-engine".to_string(), engine.name.to_string()];
    match engine.name {
        "typst" => args.extend(typst_font_args(typst_setup, &doc.metadata_keys)),
        // xeCJK / luatexja (pulled in by pandoc's template when
        // CJKmainfont is set) handle CJK once they have a font.
        "tectonic" | "xelatex" | "lualatex"
            if doc.has_cjk && !doc.metadata_keys.contains("CJKmainfont") =>
        {
            args.push("-V".to_string());
            args.push(format!("CJKmainfont={LATEX_CJK_FONT}"));
        }
        // wkhtmltopdf: WebKit falls back to system CJK fonts on its own.
        _ => {}
    }
    Ok(args)
}

fn choose_engine<'a>(engines: &'a [PdfEngine], doc: &SourceDocument) -> Option<&'a PdfEngine> {
    if !doc.has_cjk {
        return engines.first();
    }
    let preference = if doc.is_latex {
        CJK_LATEX_INPUT_ENGINES
    } else {
        CJK_ENGINES
    };
    preference
        .iter()
        .find_map(|name| engines.iter().find(|e| e.name == *name))
}

/// Pandoc variables that set typst's fonts: typst's default Latin fonts
/// first, then the CJK fonts. Typst takes each character from the first
/// listed font that has it, so Latin text keeps its usual look. Variables
/// the document's own metadata sets are left alone; a document that sets
/// `mainfont` but has CJK text gets typst's own CJK fallback.
///
/// Pandoc inserts `mainfont` verbatim into `font: ("$mainfont$",)`, so
/// joining names with `", "` yields a typst list. `codefont` (code blocks)
/// is a list variable in the templates that support it.
fn typst_font_args(setup: &TypstSetup, metadata_keys: &HashSet<String>) -> Vec<String> {
    let cjk: Vec<String> = setup.cjk.iter().map(|f| typst_string_escape(f)).collect();
    let mut args = Vec::new();
    if !metadata_keys.contains("mainfont") && (setup.template_needs_mainfont || !cjk.is_empty()) {
        let mut body_fonts = vec!["Libertinus Serif".to_string()];
        body_fonts.extend(cjk.iter().cloned());
        args.push("-V".to_string());
        args.push(format!("mainfont={}", body_fonts.join(r#"", ""#)));
    }
    if !metadata_keys.contains("codefont") && !cjk.is_empty() {
        for font in std::iter::once("DejaVu Sans Mono".to_string()).chain(cjk) {
            args.push("-V".to_string());
            args.push(format!("codefont={font}"));
        }
    }
    args
}

/// Escape `text` for use inside a typst string literal.
fn typst_string_escape(text: &str) -> String {
    text.replace('\\', r"\\").replace('"', r#"\""#)
}

/// `current` PATH with every engine's directory appended, so pandoc (which
/// is given only the engine's name) also finds engines that aren't on
/// PATH, like typst in Swift Shifter's tool dir or Homebrew's bin when the
/// app is launched from Finder. Appending keeps anything already on PATH
/// first, which is also where `find_engine` looked first. `None` when
/// nothing needs adding or the result can't be represented.
fn pandoc_search_path(current: Option<OsString>, engines: &[PdfEngine]) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = current
        .as_deref()
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    let before = dirs.len();
    for engine in engines {
        if let Some(dir) = engine.program.parent()
            && !dirs.iter().any(|d| d == dir)
        {
            dirs.push(dir.to_path_buf());
        }
    }
    if dirs.len() == before {
        return None;
    }
    std::env::join_paths(dirs).ok()
}

/// Whether `pandoc`'s typst template breaks without `mainfont`. Older
/// pandoc (e.g. the 3.1.x in Debian/Ubuntu) defaults the template's font
/// list to `()`, which current typst rejects ("font fallback list must not
/// be empty"); newer pandoc leaves typst's own default font alone.
/// Checked once per run.
async fn typst_template_needs_mainfont(pandoc: &Path) -> bool {
    static NEEDS_MAINFONT: OnceLock<bool> = OnceLock::new();
    if let Some(&needs) = NEEDS_MAINFONT.get() {
        return needs;
    }
    let Ok(output) = crate::process::async_command(pandoc)
        .args(["-f", "markdown", "-t", "typst", "--standalone"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return false;
    };
    let needs = template_has_empty_font_list(&String::from_utf8_lossy(&output.stdout));
    NEEDS_MAINFONT.set(needs).ok();
    needs
}

fn template_has_empty_font_list(standalone_typst: &str) -> bool {
    standalone_typst.contains("font: (),")
}

/// The `CJK_FONTS` that typst can see, in preference order. Empty when
/// there are none (typst then falls back to fonts of its own choosing) or
/// `typst fonts` fails. Cached once found; an empty result isn't cached so
/// installing a font takes effect without restarting.
async fn installed_cjk_fonts(typst: &Path) -> Vec<String> {
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    if let Some(fonts) = CACHE.get() {
        return fonts.clone();
    }
    let output = match crate::process::async_command(typst)
        .arg("fonts")
        .output()
        .await
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            eprintln!(
                "`typst fonts` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return Vec::new();
        }
        Err(e) => {
            eprintln!("`typst fonts` failed to start: {e}");
            return Vec::new();
        }
    };
    let fonts = pick_cjk_fonts(&String::from_utf8_lossy(&output.stdout));
    if fonts.is_empty() {
        eprintln!("No known CJK font found; typst will pick fallback fonts itself");
    } else {
        CACHE.set(fonts.clone()).ok();
    }
    fonts
}

/// `CJK_FONTS` entries present in `typst fonts` output (one family per line).
fn pick_cjk_fonts(typst_fonts_output: &str) -> Vec<String> {
    let installed: HashSet<&str> = typst_fonts_output.lines().map(str::trim).collect();
    CJK_FONTS
        .iter()
        .filter(|font| installed.contains(**font))
        .map(|font| font.to_string())
        .collect()
}

impl SourceDocument {
    /// Reads plain-text formats directly and looks inside the XML of
    /// zip-based ones (docx, odt, epub). An unreadable file inspects as a
    /// document without CJK text or metadata.
    fn inspect(path: &Path) -> Self {
        let is_latex = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("tex") || e.eq_ignore_ascii_case("latex"));
        let Ok(bytes) = std::fs::read(path) else {
            return Self {
                is_latex,
                ..Self::default()
            };
        };
        if bytes.starts_with(b"PK\x03\x04") {
            return Self {
                has_cjk: zip_markup_has_cjk(&bytes),
                is_latex,
                metadata_keys: HashSet::new(),
            };
        }
        let text = String::from_utf8_lossy(&bytes);
        Self {
            has_cjk: contains_cjk(&text),
            is_latex,
            metadata_keys: front_matter_keys(&text),
        }
    }
}

/// Top-level keys of a leading YAML metadata block (`---` … `---`/`...`),
/// as pandoc reads it from Markdown. Empty when there is none.
fn front_matter_keys(text: &str) -> HashSet<String> {
    // pandoc ignores a leading byte-order mark, which Windows editors add.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return HashSet::new();
    }
    let yaml: Vec<&str> = lines
        .take_while(|line| !matches!(line.trim_end(), "---" | "..."))
        .collect();
    let Ok(serde_yaml::Value::Mapping(map)) = serde_yaml::from_str(&yaml.join("\n")) else {
        return HashSet::new();
    };
    map.keys()
        .filter_map(|key| key.as_str().map(str::to_string))
        .collect()
}

fn zip_markup_has_cjk(bytes: &[u8]) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return false;
    };
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        let is_markup = [".xml", ".xhtml", ".html", ".htm"]
            .iter()
            .any(|ext| entry.name().ends_with(ext));
        let mut text = String::new();
        if is_markup && entry.read_to_string(&mut text).is_ok() && contains_cjk(&text) {
            return true;
        }
    }
    false
}

/// Whether `text` contains CJK letters (ideographs, kana, Hangul). CJK
/// punctuation and fullwidth forms alone don't count: they turn up in
/// otherwise Latin text, and the default fonts' fallback copes with them.
fn contains_cjk(text: &str) -> bool {
    text.chars().any(is_cjk_letter)
}

fn is_cjk_letter(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11FF}'     // Hangul Jamo
        | '\u{2E80}'..='\u{2FDF}'   // CJK and Kangxi radicals
        | '\u{3040}'..='\u{30FF}'   // Hiragana, Katakana
        | '\u{3100}'..='\u{31FF}'   // Bopomofo, Hangul compatibility Jamo, …
        | '\u{3400}'..='\u{4DBF}'   // CJK Unified Ideographs Extension A
        | '\u{4E00}'..='\u{9FFF}'   // CJK Unified Ideographs
        | '\u{AC00}'..='\u{D7AF}'   // Hangul syllables
        | '\u{F900}'..='\u{FAFF}'   // CJK compatibility ideographs
        | '\u{20000}'..='\u{3134F}' // CJK Unified Ideographs Extensions B–G
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(name: &'static str) -> PdfEngine {
        PdfEngine {
            name,
            program: PathBuf::from(format!("/bin/{name}")),
        }
    }

    fn fonts(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn cjk_doc() -> SourceDocument {
        SourceDocument {
            has_cjk: true,
            ..SourceDocument::default()
        }
    }

    fn cjk_typst(cjk: &[&str]) -> TypstSetup {
        TypstSetup {
            cjk: fonts(cjk),
            template_needs_mainfont: false,
        }
    }

    fn keys(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    // ── CJK detection ───────────────────────────────────────────────────

    #[test]
    fn detects_chinese_japanese_and_korean() {
        assert!(contains_cjk("這是中文"));
        assert!(contains_cjk("简体中文"));
        assert!(contains_cjk("ひらがな"));
        assert!(contains_cjk("カタカナ"));
        assert!(contains_cjk("한국어"));
        assert!(contains_cjk("Mixed English 與中文"));
        assert!(contains_cjk("rare 𠀀 ideograph"));
    }

    #[test]
    fn latin_text_with_stray_cjk_punctuation_is_not_cjk() {
        assert!(!contains_cjk("Plain English, café, naïve — “quotes”"));
        assert!(!contains_cjk("Ελληνικά и русский"));
        assert!(!contains_cjk("pasted「quote」。and fullwidth ＡＢＣ"));
        assert!(!contains_cjk(""));
    }

    #[test]
    fn inspect_reads_text_files() {
        let dir = tempfile::tempdir().unwrap();
        let zh = dir.path().join("zh.md");
        let en = dir.path().join("en.md");
        std::fs::write(&zh, "# 標題\n\n內容").unwrap();
        std::fs::write(&en, "# Title\n\nBody").unwrap();
        assert!(SourceDocument::inspect(&zh).has_cjk);
        assert!(!SourceDocument::inspect(&en).has_cjk);
        assert!(!SourceDocument::inspect(&dir.path().join("missing.md")).has_cjk);
    }

    #[test]
    fn inspect_looks_inside_zip_markup() {
        use std::io::Write;
        fn docx_with(body: &str) -> Vec<u8> {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("word/document.xml", options).unwrap();
            zip.write_all(format!("<w:t>{body}</w:t>").as_bytes())
                .unwrap();
            zip.finish().unwrap().into_inner()
        }
        let dir = tempfile::tempdir().unwrap();
        let zh = dir.path().join("zh.docx");
        let en = dir.path().join("en.docx");
        std::fs::write(&zh, docx_with("中文內容")).unwrap();
        std::fs::write(&en, docx_with("English only")).unwrap();
        assert!(SourceDocument::inspect(&zh).has_cjk);
        assert!(!SourceDocument::inspect(&en).has_cjk);
    }

    #[test]
    fn inspect_flags_latex_input() {
        let dir = tempfile::tempdir().unwrap();
        let tex = dir.path().join("paper.TEX");
        std::fs::write(&tex, "\\section{中文}").unwrap();
        let doc = SourceDocument::inspect(&tex);
        assert!(doc.is_latex && doc.has_cjk);
    }

    // ── front matter ────────────────────────────────────────────────────

    #[test]
    fn front_matter_keys_reads_leading_yaml_block() {
        let md = "---\ntitle: T\nmainfont: Helvetica\nnested:\n  codefont: x\n---\n\n# Body\n";
        assert_eq!(
            front_matter_keys(md),
            keys(&["title", "mainfont", "nested"])
        );
        let dots = "---\nCJKmainfont: Kaiti SC\n...\nbody";
        assert_eq!(front_matter_keys(dots), keys(&["CJKmainfont"]));
        let bom = "\u{FEFF}---\nmainfont: Helvetica\n---\nbody";
        assert_eq!(front_matter_keys(bom), keys(&["mainfont"]));
    }

    #[test]
    fn front_matter_keys_ignores_non_metadata() {
        assert!(front_matter_keys("# Title\n\n---\nmainfont: X\n---\n").is_empty());
        assert!(front_matter_keys("---\n- a list\n---\n").is_empty());
        assert!(front_matter_keys("---\n: not yaml [\n---\n").is_empty());
        assert!(front_matter_keys("").is_empty());
    }

    // ── fonts typst can see ─────────────────────────────────────────────

    #[test]
    fn pick_cjk_fonts_keeps_preference_order() {
        let output = "Arial\nHiragino Sans GB\nSongti SC\n  PingFang SC  \nHelvetica\n";
        assert_eq!(
            pick_cjk_fonts(output),
            fonts(&["Songti SC", "PingFang SC", "Hiragino Sans GB"])
        );
        assert!(pick_cjk_fonts("Arial\nHelvetica\n").is_empty());
    }

    #[test]
    fn detects_template_without_default_font() {
        // pandoc 3.1.x
        assert!(template_has_empty_font_list(
            "  region: \"US\",\n  font: (),\n  fontsize: 11pt,"
        ));
        // pandoc 3.2+
        assert!(!template_has_empty_font_list(
            "  font: none,\n  fontsize: 11pt,"
        ));
    }

    // ── typst font variables ────────────────────────────────────────────

    #[test]
    fn typst_gets_no_font_variables_when_none_are_needed() {
        assert!(typst_font_args(&TypstSetup::default(), &HashSet::new()).is_empty());
    }

    #[test]
    fn typst_gets_a_mainfont_when_the_template_needs_one() {
        let fonts = TypstSetup {
            cjk: Vec::new(),
            template_needs_mainfont: true,
        };
        assert_eq!(
            typst_font_args(&fonts, &HashSet::new()),
            ["-V", "mainfont=Libertinus Serif"]
        );
    }

    #[test]
    fn typst_font_args_put_cjk_behind_latin_fonts() {
        let args = typst_font_args(
            &cjk_typst(&["Songti SC", "Apple SD Gothic Neo"]),
            &HashSet::new(),
        );
        assert_eq!(
            args,
            [
                "-V",
                r#"mainfont=Libertinus Serif", "Songti SC", "Apple SD Gothic Neo"#,
                "-V",
                "codefont=DejaVu Sans Mono",
                "-V",
                "codefont=Songti SC",
                "-V",
                "codefont=Apple SD Gothic Neo",
            ]
        );
    }

    #[test]
    fn typst_font_args_leave_fonts_the_document_sets() {
        let fonts = TypstSetup {
            cjk: fonts(&["Songti SC"]),
            template_needs_mainfont: true,
        };
        assert!(typst_font_args(&fonts, &keys(&["mainfont", "codefont"])).is_empty());
        let args = typst_font_args(&fonts, &keys(&["codefont"]));
        assert_eq!(args, ["-V", r#"mainfont=Libertinus Serif", "Songti SC"#]);
    }

    #[test]
    fn typst_font_args_escape_quotes() {
        let args = typst_font_args(&cjk_typst(&[r#"Odd "Font""#]), &HashSet::new());
        assert!(args[1].ends_with(r#"Odd \"Font\""#), "{args:?}");
    }

    // ── engine choice ───────────────────────────────────────────────────

    #[test]
    fn plain_documents_use_the_first_engine_by_name() {
        let engines = [engine("xelatex"), engine("typst")];
        let args = pdf_args(&engines, &SourceDocument::default(), &TypstSetup::default());
        assert_eq!(args.unwrap(), ["--pdf-engine", "xelatex"]);
    }

    #[test]
    fn cjk_prefers_typst_with_font_fallbacks() {
        let engines = [engine("xelatex"), engine("typst")];
        let args = pdf_args(&engines, &cjk_doc(), &cjk_typst(&["Songti SC"])).unwrap();
        assert_eq!(args[..2], ["--pdf-engine", "typst"]);
        assert_eq!(
            args[2..4],
            ["-V", r#"mainfont=Libertinus Serif", "Songti SC"#]
        );
    }

    #[test]
    fn cjk_with_typst_but_no_known_font_lets_typst_choose() {
        let args = pdf_args(&[engine("typst")], &cjk_doc(), &cjk_typst(&[])).unwrap();
        assert_eq!(args, ["--pdf-engine", "typst"]);
    }

    #[test]
    fn cjk_latex_input_prefers_xelatex_over_typst() {
        let engines = [engine("xelatex"), engine("typst")];
        let doc = SourceDocument {
            has_cjk: true,
            is_latex: true,
            ..SourceDocument::default()
        };
        let args = pdf_args(&engines, &doc, &cjk_typst(&["Songti SC"])).unwrap();
        assert_eq!(
            args,
            [
                "--pdf-engine".to_string(),
                "xelatex".to_string(),
                "-V".to_string(),
                format!("CJKmainfont={LATEX_CJK_FONT}"),
            ]
        );
    }

    #[test]
    fn cjk_without_typst_sets_latex_cjk_font_unless_the_document_does() {
        let engines = [engine("pdflatex"), engine("xelatex")];
        let args = pdf_args(&engines, &cjk_doc(), &TypstSetup::default()).unwrap();
        assert_eq!(
            args,
            [
                "--pdf-engine".to_string(),
                "xelatex".to_string(),
                "-V".to_string(),
                format!("CJKmainfont={LATEX_CJK_FONT}"),
            ]
        );
        let own_font = SourceDocument {
            metadata_keys: keys(&["CJKmainfont"]),
            ..cjk_doc()
        };
        let args = pdf_args(&engines, &own_font, &TypstSetup::default()).unwrap();
        assert_eq!(args, ["--pdf-engine", "xelatex"]);
    }

    #[test]
    fn cjk_falls_back_to_wkhtmltopdf() {
        let engines = [engine("pdflatex"), engine("wkhtmltopdf")];
        let args = pdf_args(&engines, &cjk_doc(), &TypstSetup::default()).unwrap();
        assert_eq!(args, ["--pdf-engine", "wkhtmltopdf"]);
    }

    #[test]
    fn cjk_with_only_pdflatex_is_an_error() {
        let err = pdf_args(&[engine("pdflatex")], &cjk_doc(), &TypstSetup::default());
        assert!(err.unwrap_err().contains("pdflatex can't typeset"));
    }

    #[test]
    fn no_engine_leaves_it_to_pandoc() {
        let args = pdf_args(&[], &cjk_doc(), &TypstSetup::default()).unwrap();
        assert!(args.is_empty());
    }

    // ── PATH for pandoc ─────────────────────────────────────────────────

    #[test]
    fn search_path_appends_missing_engine_dirs() {
        let current = std::env::join_paths(["/usr/bin", "/bin"]).unwrap();
        let managed = PdfEngine {
            name: "typst",
            program: PathBuf::from("/home/u/.local/share/swift-shifter/bin/typst"),
        };
        let on_path = PdfEngine {
            name: "xelatex",
            program: PathBuf::from("/usr/bin/xelatex"),
        };
        let path = pandoc_search_path(Some(current), &[on_path, managed]).unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(
            dirs,
            [
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
                PathBuf::from("/home/u/.local/share/swift-shifter/bin"),
            ]
        );
    }

    #[test]
    fn search_path_unchanged_when_engines_already_on_path() {
        let current = std::env::join_paths(["/usr/bin"]).unwrap();
        let engines = [PdfEngine {
            name: "xelatex",
            program: PathBuf::from("/usr/bin/xelatex"),
        }];
        assert_eq!(pandoc_search_path(Some(current), &engines), None);
        assert_eq!(pandoc_search_path(None, &[]), None);
    }

    // ── end to end through pandoc + typst ───────────────────────────────

    /// Font names as they appear in a PDF, reduced to comparable form:
    /// "ABCDEF+STSongti-SC-Regular" -> "stsongtiscregular".
    fn normalize_font_name(name: &str) -> String {
        let name = name.split_once('+').map_or(name, |(_, rest)| rest);
        name.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }

    fn embedded_font_names(pdf: &Path) -> Vec<String> {
        let doc = lopdf::Document::load(pdf).unwrap();
        doc.objects
            .values()
            .filter_map(|object| object.as_dict().ok())
            .filter(|dict| dict.has_type(b"Font"))
            .filter_map(|dict| dict.get(b"BaseFont").ok()?.as_name().ok())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect()
    }

    /// CJK glyphs must come from the fonts we chose, not from whatever
    /// typst falls back to on its own. Chinese only, since the preferred
    /// fonts on a machine may lack kana or Hangul. Skipped when pandoc,
    /// typst or any known CJK font is missing.
    #[tokio::test]
    async fn cjk_pdf_embeds_only_the_chosen_fonts() {
        use crate::converter::document::binaries::find_pandoc_binary;
        let (Some(pandoc), Some(typst)) = (find_pandoc_binary(), find_typst_binary()) else {
            eprintln!("SKIP: needs pandoc and typst");
            return;
        };
        let cjk_fonts = installed_cjk_fonts(&typst).await;
        if cjk_fonts.is_empty() {
            eprintln!("SKIP: no known CJK font installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("zh.md");
        let pdf = dir.path().join("zh.pdf");
        std::fs::write(&md, "# 中文標題\n\n這是一段中文，與 English 混合。\n").unwrap();

        let mut cmd = tokio::process::Command::new(&pandoc);
        cmd.args(["-f", "markdown", "-o"]).arg(&pdf);
        configure_pandoc_pdf(&mut cmd, &pandoc, Some(&md))
            .await
            .unwrap();
        let status = cmd.arg(&md).status().await.unwrap();
        assert!(status.success(), "pandoc failed: {cmd:?}");

        // Typst's bundled Latin, math and mono fonts are always fine.
        let mut allowed = vec![
            "libertinusserif".to_string(),
            "newcm".to_string(),
            "dejavusansmono".to_string(),
        ];
        allowed.extend(cjk_fonts.iter().map(|f| normalize_font_name(f)));
        let fonts = embedded_font_names(&pdf);
        assert!(!fonts.is_empty(), "no fonts embedded");
        for font in fonts {
            let normalized = normalize_font_name(&font);
            assert!(
                allowed.iter().any(|a| normalized.contains(a.as_str())),
                "unexpected fallback font {font}; chose {cjk_fonts:?}"
            );
        }
    }
}
