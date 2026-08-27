use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

#[derive(serde::Serialize)]
pub struct PasteResult {
    pub path: String,
    pub is_temp: bool,
}

/// Decode percent-escapes in a URL path (e.g. `%20` → ` `).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// On Linux, read raw image bytes from the clipboard. Queries available
/// targets via `xclip -t TARGETS -o` (X11) then `wl-paste --list-types`
/// (Wayland), and only fetches the first matching MIME — instead of
/// fanning out one subprocess per (tool × MIME) and waiting for each to
/// fail. The MIME is authoritative for the file extension.
#[cfg(target_os = "linux")]
fn read_raw_image_from_clipboard_linux() -> Option<(Vec<u8>, &'static str)> {
    use std::process::Command;

    const MIME_TYPES: &[(&str, &str)] = &[
        ("image/jpeg", "jpg"),
        ("image/png", "png"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("image/tiff", "tif"),
        ("image/bmp", "bmp"),
    ];

    // X11 (xclip).
    if let Ok(targets) = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
        .output()
    {
        if targets.status.success() {
            let listed = String::from_utf8_lossy(&targets.stdout);
            for &(mime, ext) in MIME_TYPES {
                if listed.lines().any(|l| l.trim() == mime) {
                    if let Ok(data) = Command::new("xclip")
                        .args(["-selection", "clipboard", "-t", mime, "-o"])
                        .output()
                    {
                        if data.status.success() && !data.stdout.is_empty() {
                            return Some((data.stdout, ext));
                        }
                    }
                }
            }
        }
    }

    // Wayland (wl-paste).
    if let Ok(targets) = Command::new("wl-paste").arg("--list-types").output() {
        if targets.status.success() {
            let listed = String::from_utf8_lossy(&targets.stdout);
            for &(mime, ext) in MIME_TYPES {
                if listed.lines().any(|l| l.trim() == mime) {
                    if let Ok(data) = Command::new("wl-paste")
                        .args(["--type", mime, "--no-newline"])
                        .output()
                    {
                        if data.status.success() && !data.stdout.is_empty() {
                            return Some((data.stdout, ext));
                        }
                    }
                }
            }
        }
    }

    None
}

/// On macOS, read the bytes for a single UTI from NSPasteboard's general
/// pasteboard. The unsafe block runs inside an autoreleasepool so the NSData
/// returned by `dataForType:` is released when we leave — without the pool
/// these objects accumulate on the tokio worker thread.
#[cfg(target_os = "macos")]
fn pasteboard_data_for_uti(uti: &str) -> Option<Vec<u8>> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    autoreleasepool(|_| unsafe {
        let pb_class = AnyClass::get(c"NSPasteboard")?;
        let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
        if pb.is_null() { return None; }

        let type_str = NSString::from_str(uti);
        let data: *mut AnyObject = msg_send![pb, dataForType: &*type_str];
        if data.is_null() { return None; }
        let len: usize = msg_send![data, length];
        if len == 0 { return None; }
        let ptr: *const u8 = msg_send![data, bytes];
        if ptr.is_null() { return None; }
        Some(std::slice::from_raw_parts(ptr, len).to_vec())
    })
}

/// On macOS, read NSPasteboard's `stringForType:` for a UTI. NSPasteboard
/// coerces URL-like and string-like UTIs into NSString here even when
/// `dataForType:` returns a binary representation (e.g. an archived NSURL
/// for `public.file-url` from some Finder builds), so this is the
/// preferred path for string-typed UTIs.
#[cfg(target_os = "macos")]
fn pasteboard_string_for_uti(uti: &str) -> Option<String> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    autoreleasepool(|_| unsafe {
        let pb_class = AnyClass::get(c"NSPasteboard")?;
        let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
        if pb.is_null() { return None; }

        let type_str = NSString::from_str(uti);
        let s: *mut NSString = msg_send![pb, stringForType: &*type_str];
        if s.is_null() { return None; }
        Some((*s).to_string())
    })
}

/// On macOS, list the UTI strings currently on the general pasteboard.
/// Used by the `paste_diagnostics` command for live troubleshooting and
/// (potentially) to fast-path UTI matching without `dataForType:` calls.
#[cfg(target_os = "macos")]
fn pasteboard_available_types() -> Vec<String> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    autoreleasepool(|_| unsafe {
        let pb_class = match AnyClass::get(c"NSPasteboard") {
            Some(c) => c,
            None => return Vec::new(),
        };
        let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
        if pb.is_null() { return Vec::new(); }
        let arr: *mut AnyObject = msg_send![pb, types];
        if arr.is_null() { return Vec::new(); }
        let count: usize = msg_send![arr, count];
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let item: *mut NSString = msg_send![arr, objectAtIndex: i];
            if !item.is_null() {
                out.push((*item).to_string());
            }
        }
        out
    })
}

/// Parse a file URL string (`file:///...`) into a verified filesystem
/// path. Pure helper so the parsing rules are testable without a live
/// pasteboard. Trims surrounding whitespace and trailing NULs, so callers
/// can pass whatever NSPasteboard / xclip / wl-paste handed them.
fn parse_file_url_string(s: &str) -> Option<String> {
    let url = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    let path = url.strip_prefix("file://")?;
    let decoded = percent_decode(path);
    if std::path::Path::new(&decoded).is_file() {
        Some(decoded)
    } else {
        None
    }
}

/// Parse arbitrary pasteboard bytes that *should* contain a `file://` URL.
/// Tries plain UTF-8 first, then heuristically scans for an embedded
/// `file://` substring — this catches archived-NSURL or property-list
/// wrappers that some macOS apps put on the pasteboard.
fn parse_file_url_bytes(bytes: &[u8]) -> Option<String> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        if let Some(p) = parse_file_url_string(s) {
            return Some(p);
        }
    }
    // Lossy scan for an embedded URL.
    let lossy = String::from_utf8_lossy(bytes);
    let idx = lossy.find("file://")?;
    let rest = &lossy[idx..];
    // Stop at the first NUL or replacement char (binary boundary).
    let end = rest
        .char_indices()
        .find(|(_, c)| *c == '\0' || *c == '\u{FFFD}')
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    parse_file_url_string(&rest[..end])
}

/// Resolve a Finder file-reference URL (`file:///.file/id=...`) to a
/// real filesystem path via `NSURL.filePathURL`. Finder's ⌘C puts these
/// opaque IDs on the pasteboard instead of plain `file:///path` URLs;
/// `parse_file_url_string` rejects them because the literal path
/// doesn't exist on disk.
#[cfg(target_os = "macos")]
fn resolve_file_reference_url(url_str: &str) -> Option<String> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    autoreleasepool(|_| unsafe {
        let nsurl_class = AnyClass::get(c"NSURL")?;
        let s = NSString::from_str(url_str);
        let url: *mut AnyObject = msg_send![nsurl_class, URLWithString: &*s];
        if url.is_null() { return None; }
        let file_url: *mut AnyObject = msg_send![url, filePathURL];
        if file_url.is_null() { return None; }
        let path: *mut NSString = msg_send![file_url, path];
        if path.is_null() { return None; }
        let p = (*path).to_string();
        if std::path::Path::new(&p).is_file() { Some(p) } else { None }
    })
}

/// On macOS, read the file path from a Finder-style file copy. Tries
/// `stringForType:` first (NSPasteboard coerces URL UTIs to NSString
/// reliably), then falls back to raw `dataForType:` parsing — different
/// macOS releases / source apps vend the URL in different shapes.
#[cfg(target_os = "macos")]
fn read_file_path_from_pasteboard() -> Option<String> {
    if let Some(s) = pasteboard_string_for_uti("public.file-url") {
        if let Some(p) = parse_file_url_string(&s) {
            return Some(p);
        }
        if let Some(p) = resolve_file_reference_url(&s) {
            return Some(p);
        }
    }
    if let Some(bytes) = pasteboard_data_for_uti("public.file-url") {
        if let Some(p) = parse_file_url_bytes(&bytes) {
            return Some(p);
        }
        if let Ok(s) = std::str::from_utf8(&bytes) {
            if let Some(p) = resolve_file_reference_url(s.trim_end_matches('\0')) {
                return Some(p);
            }
        }
    }
    None
}

/// Structured rich data — PDFs, Office documents, video, audio. These
/// rarely coexist with a plain-text representation on the pasteboard, so
/// when one shows up we treat it as the primary content and prefer it
/// over text. Listed in priority order; documents > video > audio.
#[cfg(target_os = "macos")]
const RICH_DOC_UTIS: &[(&str, &str)] = &[
    // Documents
    ("com.adobe.pdf", "pdf"),
    ("org.openxmlformats.wordprocessingml.document", "docx"),
    ("com.microsoft.word.doc", "doc"),
    ("org.openxmlformats.presentationml.presentation", "pptx"),
    ("com.microsoft.powerpoint.ppt", "ppt"),
    ("org.openxmlformats.spreadsheetml.sheet", "xlsx"),
    ("com.microsoft.excel.xls", "xls"),
    ("org.idpf.epub-container", "epub"),
    // Video
    ("public.mpeg-4", "mp4"),
    ("com.apple.quicktime-movie", "mov"),
    ("public.avi", "avi"),
    ("public.mpeg-2-video", "mpg"),
    ("public.3gpp", "3gp"),
    // Audio
    ("public.mp3", "mp3"),
    ("public.mpeg-4-audio", "m4a"),
    ("public.aiff-audio", "aiff"),
    ("com.microsoft.waveform-audio", "wav"),
    ("com.apple.coreaudio-format", "caf"),
    ("org.xiph.flac", "flac"),
    ("org.xiph.ogg", "ogg"),
];

/// Rich text — RTF and HTML. These almost always coexist with plain text on
/// browser/Mail/Notes copies, where the user typically wants the plain text.
/// Used only as a last-resort fallback when no plain-text type is on the
/// pasteboard.
#[cfg(target_os = "macos")]
const RICH_TEXT_UTIS: &[(&str, &str)] = &[
    ("public.rtf", "rtf"),
    ("public.html", "html"),
];

#[cfg(target_os = "macos")]
fn read_rich_doc_from_pasteboard() -> Option<(Vec<u8>, &'static str)> {
    for &(uti, ext) in RICH_DOC_UTIS {
        if let Some(bytes) = pasteboard_data_for_uti(uti) {
            return Some((bytes, ext));
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn read_rich_text_from_pasteboard() -> Option<(Vec<u8>, &'static str)> {
    for &(uti, ext) in RICH_TEXT_UTIS {
        if let Some(bytes) = pasteboard_data_for_uti(uti) {
            return Some((bytes, ext));
        }
    }
    None
}

/// On macOS, read raw image bytes from NSPasteboard by trying each codec
/// UTI in preference order. The UTI is authoritative for the file
/// extension — NSPasteboard never lies about the type it's vending, so
/// magic-byte sniffing would only add a way to mislabel valid bytes.
///
/// `public.tiff` is intentionally omitted: macOS's NSPasteboard auto-promotes
/// almost everything to TIFF (text selections in Mail/Notes get a TIFF
/// rendering, Finder file copies attach the icon as TIFF, and most native
/// apps store images only as TIFF). Returning TIFF here would make every
/// paste look like a `.tif`. TIFF-only sources fall through to the Tauri
/// `read_image()` path which decodes to RGBA and writes a PNG.
#[cfg(target_os = "macos")]
fn read_raw_image_from_pasteboard() -> Option<(Vec<u8>, &'static str)> {
    const UTIS: &[(&str, &str)] = &[
        ("public.jpeg", "jpg"),
        ("public.png", "png"),
        ("com.compuserve.gif", "gif"),
        ("org.webmproject.webp", "webp"),
        ("public.heic", "heic"),
        ("public.avif", "avif"),
    ];
    for &(uti, ext) in UTIS {
        if let Some(bytes) = pasteboard_data_for_uti(uti) {
            return Some((bytes, ext));
        }
    }
    None
}

/// True if the pasteboard carries any plain-text representation. Used to
/// distinguish "user copied text" from "macOS coerced an NSURL/NSImage to a
/// string when read_text() was called".
#[cfg(target_os = "macos")]
fn pasteboard_has_text_data() -> bool {
    pasteboard_data_for_uti("public.utf8-plain-text").is_some()
        || pasteboard_data_for_uti("public.utf16-plain-text").is_some()
        || pasteboard_data_for_uti("public.plain-text").is_some()
}

/// True if the pasteboard carries any image data, including auto-promoted
/// TIFF. Used as the signal for "treat the paste as an image" when no
/// explicit codec UTI matched and no text type is present.
#[cfg(target_os = "macos")]
fn pasteboard_has_image_data() -> bool {
    const UTIS: &[&str] = &[
        "public.tiff",
        "public.png",
        "public.jpeg",
        "com.compuserve.gif",
        "org.webmproject.webp",
        "public.heic",
        "public.avif",
    ];
    UTIS.iter().any(|u| pasteboard_data_for_uti(u).is_some())
}

/// Detect the format of clipboard text. Defaults to plain text and only
/// returns a specialised extension when the content actually parses as
/// JSON, XML, or TOML. Loose heuristics (e.g. "looks YAML-ish",
/// "first line has commas") are deliberately avoided because they
/// false-positive on ordinary prose.
fn sniff_text_format(text: &str) -> &'static str {
    let trimmed = text.trim();
    if trimmed.is_empty() { return "txt"; }

    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        return "json";
    }

    // XML: must have an angle-bracketed root and at least one closing tag.
    if (trimmed.starts_with("<?xml") || trimmed.starts_with('<'))
        && trimmed.ends_with('>')
        && trimmed.contains("</")
    {
        return "xml";
    }

    // TOML: only attempt parse when the text looks remotely TOML-like;
    // `toml::from_str` accepts plain comment-only or empty input as a
    // valid empty table, which would mis-classify prose.
    let toml_shaped = text.lines().any(|l| {
        let t = l.trim();
        (t.starts_with('[') && t.ends_with(']') && t.len() > 2)
            || t.contains(" = ")
    });
    if toml_shaped && toml::from_str::<toml::Value>(text).is_ok() {
        return "toml";
    }

    "txt"
}

fn temp_path(ext: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("swift-shifter-{}-{}.{}", t, seq, ext))
}

/// Write clipboard text to a temp file with a sniffed extension. Two
/// shortcuts skip the write and return the existing path directly:
///   • the text is a `file://` URL pointing at an existing file
///   • the text is a single-line absolute path pointing at an existing file
///
/// The bare-path shortcut requires `is_absolute()` and no embedded
/// newlines/CR/NULs so prose that happens to coincide with an existing
/// relative file in the CWD doesn't get coerced into a file reference.
fn write_text_paste(text: &str) -> Result<PasteResult, String> {
    let trimmed = text.trim();
    if let Some(path) = trimmed.strip_prefix("file://") {
        let decoded = percent_decode(path);
        if std::path::Path::new(&decoded).is_file() {
            return Ok(PasteResult { path: decoded, is_temp: false });
        }
    }
    let candidate = std::path::Path::new(trimmed);
    if !trimmed.contains(['\n', '\r', '\0'])
        && candidate.is_absolute()
        && candidate.is_file()
    {
        return Ok(PasteResult { path: trimmed.to_string(), is_temp: false });
    }
    let ext = sniff_text_format(text);
    let p = temp_path(ext);
    std::fs::write(&p, text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(PasteResult { path: p.to_string_lossy().into_owned(), is_temp: true })
}

/// Decode the clipboard's RGBA image and write it as a PNG temp file.
fn write_image_as_png(img: &tauri::image::Image) -> Result<PasteResult, String> {
    let rb = image::RgbaImage::from_raw(img.width(), img.height(), img.rgba().to_vec())
        .ok_or("Invalid clipboard image dimensions")?;
    let p = temp_path("png");
    rb.save(&p).map_err(|e| e.to_string())?;
    Ok(PasteResult { path: p.to_string_lossy().into_owned(), is_temp: true })
}

/// Snapshot of what was synchronously readable off the system clipboard
/// at the start of a paste. Held in a plain struct so the dispatch
/// decision (`dispatch_paste_macos` / `dispatch_paste_other`) becomes a
/// pure function we can unit-test against any clipboard configuration.
#[derive(Default)]
struct ClipboardReads {
    /// macOS only: `public.file-url` resolved to an existing path.
    file_path: Option<String>,
    /// Codec-preserving image bytes (mac UTIs / Linux MIMEs).
    raw_image: Option<(Vec<u8>, &'static str)>,
    /// macOS only: PDF / Office / video / audio bytes.
    rich_doc: Option<(Vec<u8>, &'static str)>,
    /// macOS only: an explicit plain-text type is on the pasteboard.
    /// Distinguishes "real text" from "macOS coerced an NSURL/NSImage to
    /// a string when read_text() was called".
    has_text: bool,
    /// macOS only: any image data is on the pasteboard, including the
    /// auto-promoted TIFF that's not in `raw_image`.
    has_image: bool,
    /// macOS only: RTF / HTML bytes, used as a last-resort fallback.
    rich_text: Option<(Vec<u8>, &'static str)>,
}

/// What the dispatch decided to do with the clipboard. Materialising
/// this into a `PasteResult` is the caller's job (it needs Tauri's
/// `AppHandle` for `Text` and `ReadRgbaImage`); separating decision from
/// effect is what makes the priority ordering testable.
enum PasteOutcome {
    /// File on disk — return the path directly, no temp file.
    ExistingPath(String),
    /// Pasteboard bytes — write to a temp file with the given extension.
    Bytes(Vec<u8>, &'static str),
    /// Plain text — call `write_text_paste` to sniff format & write.
    Text,
    /// Image with no codec UTI (TIFF-only / auto-promoted) — let Tauri
    /// decode to RGBA and re-encode as PNG.
    ReadRgbaImage,
    Empty,
}

fn dispatch_paste_macos(r: ClipboardReads) -> PasteOutcome {
    if let Some(p) = r.file_path { return PasteOutcome::ExistingPath(p); }
    if let Some((b, e)) = r.raw_image { return PasteOutcome::Bytes(b, e); }
    if let Some((b, e)) = r.rich_doc { return PasteOutcome::Bytes(b, e); }
    if r.has_text { return PasteOutcome::Text; }
    if r.has_image { return PasteOutcome::ReadRgbaImage; }
    if let Some((b, e)) = r.rich_text { return PasteOutcome::Bytes(b, e); }
    PasteOutcome::Empty
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn dispatch_paste_other(r: ClipboardReads) -> PasteOutcome {
    // Non-macOS path. The Tauri `read_image()` call is performed by the
    // caller and signalled here as `ReadRgbaImage` (we don't have a way
    // to know up-front whether it'll succeed; the caller probes).
    if let Some(p) = r.file_path { return PasteOutcome::ExistingPath(p); }
    if let Some((b, e)) = r.raw_image { return PasteOutcome::Bytes(b, e); }
    PasteOutcome::ReadRgbaImage
}

fn finalize_outcome(
    app: &AppHandle,
    outcome: PasteOutcome,
) -> Result<PasteResult, String> {
    match outcome {
        PasteOutcome::ExistingPath(p) => Ok(PasteResult { path: p, is_temp: false }),
        PasteOutcome::Bytes(bytes, ext) => {
            let p = temp_path(ext);
            std::fs::write(&p, &bytes).map_err(|e| e.to_string())?;
            Ok(PasteResult { path: p.to_string_lossy().into_owned(), is_temp: true })
        }
        PasteOutcome::Text => {
            let text = app.clipboard().read_text().map_err(|e| e.to_string())?;
            if text.trim().is_empty() {
                return Err("Clipboard is empty".to_string());
            }
            write_text_paste(&text)
        }
        PasteOutcome::ReadRgbaImage => {
            // On non-macOS this is the catch-all: try image, then text,
            // and only error if both fail. macOS dispatch never returns
            // ReadRgbaImage unless `has_image` is true.
            if let Ok(img) = app.clipboard().read_image() {
                return write_image_as_png(&img);
            }
            if let Ok(text) = app.clipboard().read_text() {
                if !text.trim().is_empty() {
                    return write_text_paste(&text);
                }
            }
            Err("Clipboard is empty".to_string())
        }
        PasteOutcome::Empty => Err("Clipboard is empty".to_string()),
    }
}

#[cfg(target_os = "macos")]
fn snapshot_clipboard_macos() -> ClipboardReads {
    ClipboardReads {
        file_path: read_file_path_from_pasteboard(),
        raw_image: read_raw_image_from_pasteboard(),
        rich_doc:  read_rich_doc_from_pasteboard(),
        has_text:  pasteboard_has_text_data(),
        has_image: pasteboard_has_image_data(),
        rich_text: read_rich_text_from_pasteboard(),
    }
}

#[cfg(target_os = "linux")]
fn snapshot_clipboard_linux() -> ClipboardReads {
    ClipboardReads {
        raw_image: read_raw_image_from_clipboard_linux(),
        ..Default::default()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn snapshot_clipboard_other() -> ClipboardReads {
    ClipboardReads::default()
}

#[tauri::command]
pub async fn paste_from_clipboard(app: AppHandle) -> Result<PasteResult, String> {
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        let outcome = dispatch_paste_macos(snapshot_clipboard_macos());
        #[cfg(target_os = "linux")]
        let outcome = dispatch_paste_other(snapshot_clipboard_linux());
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let outcome = dispatch_paste_other(snapshot_clipboard_other());
        finalize_outcome(&app2, outcome)
    })
    .await
    .map_err(|e| format!("Clipboard task panicked: {e}"))?
}

/// Diagnostic command: returns the list of UTIs / MIME types currently
/// on the system clipboard. Surface this from the dev console
/// (`invoke('paste_diagnostics')`) when paste classification looks
/// wrong — the result tells you exactly what NSPasteboard / xclip /
/// wl-paste sees and which branch the dispatcher should take.
#[tauri::command]
pub async fn paste_diagnostics() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(|| -> Vec<String> {
        #[cfg(target_os = "macos")]
        { return pasteboard_available_types(); }
        #[cfg(target_os = "linux")]
        {
            use std::process::Command;
            if let Ok(o) = Command::new("xclip")
                .args(["-selection", "clipboard", "-t", "TARGETS", "-o"])
                .output()
            {
                if o.status.success() {
                    return String::from_utf8_lossy(&o.stdout)
                        .lines().map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty()).collect();
                }
            }
            if let Ok(o) = Command::new("wl-paste").arg("--list-types").output() {
                if o.status.success() {
                    return String::from_utf8_lossy(&o.stdout)
                        .lines().map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty()).collect();
                }
            }
            Vec::new()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        { Vec::new() }
    })
    .await
    .map_err(|e| format!("Diagnostics task panicked: {e}"))
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "tif", "avif"];

/// Write image bytes to the macOS general pasteboard under one or more
/// UTIs. Going direct (vs. arboard's `NSImage` → `writeObjects:` path)
/// means apps that read by UTI see exactly the bytes we provide, not a
/// re-encoded NSImage representation. Each `(uti, bytes)` pair becomes one
/// representation on the pasteboard; receivers pick the type they prefer.
#[cfg(target_os = "macos")]
fn write_image_to_pasteboard_macos(reps: &[(&str, &[u8])]) -> Result<(), String> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::{NSData, NSString};

    if reps.is_empty() {
        return Err("no pasteboard representations to write".to_string());
    }

    autoreleasepool(|_| unsafe {
        let pb_class = AnyClass::get(c"NSPasteboard")
            .ok_or("NSPasteboard class not found")?;
        let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
        if pb.is_null() { return Err("generalPasteboard returned nil".to_string()); }

        let _: i64 = msg_send![pb, clearContents];

        for (uti, bytes) in reps {
            let data = NSData::with_bytes(bytes);
            let uti_str = NSString::from_str(uti);
            let ok: bool = msg_send![pb, setData: &*data, forType: &*uti_str];
            if !ok {
                return Err(format!(
                    "NSPasteboard setData:forType: returned NO for {uti}"
                ));
            }
        }
        Ok(())
    })
}

/// Apple UTI for an image file extension. Returning `None` means the
/// format has no widely-recognised pasteboard UTI on macOS, so we fall
/// back to writing only a PNG re-encoding.
#[cfg(target_os = "macos")]
fn native_uti_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("public.png"),
        "jpg" | "jpeg" => Some("public.jpeg"),
        "gif" => Some("com.compuserve.gif"),
        "tiff" | "tif" => Some("public.tiff"),
        "bmp" => Some("com.microsoft.bmp"),
        "webp" => Some("org.webmproject.webp"),
        "heic" | "heif" => Some("public.heic"),
        "avif" => Some("public.avif"),
        _ => None,
    }
}

#[tauri::command]
pub async fn copy_file_to_clipboard(app: AppHandle, path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let ext = std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if IMAGE_EXTS.contains(&ext.as_str()) {
            // On macOS, go direct to NSPasteboard. We write the file's
            // own bytes under its native UTI so that pasting into Finder
            // (or any UTI-aware receiver) preserves the actual format
            // the user just converted to — pasting a GIF must yield a
            // GIF, not a PNG re-encoding. We also include a `public.png`
            // re-encoding for receivers that only know PNG (e.g. some
            // chat apps and web inputs).
            #[cfg(target_os = "macos")]
            {
                let raw_bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                let native_uti = native_uti_for_ext(&ext);

                // Formats every macOS receiver decodes via NSImage / Core
                // Graphics. Writing *only* the native UTI lets the OS
                // auto-derive TIFF/PICT/etc., and—crucially—makes apps
                // that pick "the most specific" type land on the format
                // the user actually asked for. Pasting a GIF must yield
                // a GIF, not a PNG re-encoding.
                let native_is_universal = matches!(
                    ext.as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "tiff" | "tif" | "bmp"
                );

                let mut reps: Vec<(&str, &[u8])> = Vec::with_capacity(2);
                if let Some(uti) = native_uti {
                    reps.push((uti, raw_bytes.as_slice()));
                }

                // For formats macOS doesn't decode natively (WebP, AVIF,
                // HEIC on older systems), also publish a PNG re-encoding
                // so plain `public.png` consumers can still paste.
                let png_bytes_opt: Option<Vec<u8>> = if native_is_universal {
                    None
                } else {
                    let img = image::open(&path).map_err(|e| e.to_string())?;
                    let mut buf: Vec<u8> = Vec::new();
                    img.write_to(
                        &mut std::io::Cursor::new(&mut buf),
                        image::ImageFormat::Png,
                    )
                    .map_err(|e| e.to_string())?;
                    Some(buf)
                };
                if let Some(ref png_bytes) = png_bytes_opt {
                    reps.push(("public.png", png_bytes.as_slice()));
                }

                return write_image_to_pasteboard_macos(&reps);
            }
            #[cfg(not(target_os = "macos"))]
            {
                let img = image::open(&path).map_err(|e| e.to_string())?;
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let raw = rgba.into_raw();
                let cb_img = tauri::image::Image::new_owned(raw, w, h);
                app.clipboard().write_image(&cb_img).map_err(|e| e.to_string())?;
            }
        } else {
            // Anything that reads as valid UTF-8 — source code, configs,
            // JSON/YAML/TOML/CSV, etc. — copies as text. Binary non-image
            // formats fail the UTF-8 read and surface as an error; the
            // system clipboard has no good way to carry them.
            let text = std::fs::read_to_string(&path)
                .map_err(|_| format!("Cannot copy .{ext} files to clipboard"))?;
            app.clipboard().write_text(text).map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Clipboard task panicked: {e}"))?
}

/// Validate that `path` looks like a file `temp_path` produced — a
/// `swift-shifter-`-prefixed filename inside the system temp directory.
/// Frontend-supplied paths flow into `remove_temp_file`; the prefix +
/// parent check together prevent a buggy or compromised renderer from
/// having us unlink arbitrary paths.
fn is_our_temp_file(path: &std::path::Path) -> bool {
    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    if !file_name.starts_with("swift-shifter-") {
        return false;
    }
    let parent = match path.parent() {
        Some(p) => p,
        None => return false,
    };
    let temp_dir = std::env::temp_dir();
    // Compare canonicalized forms when possible so symlinked temp dirs
    // (e.g. /tmp → /private/tmp on macOS) still match.
    match (parent.canonicalize(), temp_dir.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => parent == temp_dir,
    }
}

#[tauri::command]
pub async fn remove_temp_file(path: String) -> Result<(), String> {
    if !is_our_temp_file(std::path::Path::new(&path)) {
        return Err("Refusing to remove non-temp file".to_string());
    }
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_json_object() {
        assert_eq!(sniff_text_format(r#"{"key": "value"}"#), "json");
    }

    #[test]
    fn sniff_json_array() {
        assert_eq!(sniff_text_format("[1, 2, 3]"), "json");
    }

    #[test]
    fn sniff_toml() {
        assert_eq!(sniff_text_format("[section]\nkey = \"value\"\n"), "toml");
    }

    #[test]
    fn sniff_xml_with_declaration() {
        assert_eq!(
            sniff_text_format("<?xml version=\"1.0\"?><root><a/></root>"),
            "xml",
        );
    }

    #[test]
    fn sniff_xml_no_declaration() {
        assert_eq!(
            sniff_text_format("<root>\n  <child>text</child>\n</root>"),
            "xml",
        );
    }

    #[test]
    fn sniff_yaml_falls_back_to_txt() {
        assert_eq!(sniff_text_format("name: Alice\nage: 30\ncity: NYC\n"), "txt");
    }

    #[test]
    fn sniff_csv_falls_back_to_txt() {
        assert_eq!(sniff_text_format("name,age,city\nAlice,30,NYC\n"), "txt");
    }

    #[test]
    fn sniff_plain_text() {
        assert_eq!(sniff_text_format("hello world"), "txt");
    }

    #[test]
    fn sniff_prose_with_colons_is_text() {
        // Prose containing colons (URLs, time, "key: value" looking lines)
        // should NOT be misclassified as a structured format.
        assert_eq!(
            sniff_text_format("See https://example.com for details. Updated: today."),
            "txt",
        );
    }

    #[test]
    fn invalid_json_not_sniffed_as_json() {
        assert_ne!(sniff_text_format("{not valid json}"), "json");
    }

    #[test]
    fn percent_decode_spaces() {
        assert_eq!(percent_decode("/Users/matt/My%20Video.mp4"), "/Users/matt/My Video.mp4");
    }

    #[test]
    fn percent_decode_passthrough() {
        assert_eq!(percent_decode("/no/escapes/here.txt"), "/no/escapes/here.txt");
    }

    #[test]
    fn percent_decode_invalid_escape_preserved() {
        assert_eq!(percent_decode("/path/with/%zz/oops"), "/path/with/%zz/oops");
    }

    #[test]
    fn percent_decode_utf8_multibyte() {
        // "中" is U+4E2D → UTF-8 0xE4 0xB8 0xAD.
        assert_eq!(percent_decode("/dir/%E4%B8%AD.txt"), "/dir/中.txt");
    }

    #[test]
    fn percent_decode_trailing_percent_no_oob() {
        // Trailing `%` without two hex digits must not panic and must
        // pass through untouched.
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("abc%2"), "abc%2");
    }

    #[test]
    fn percent_decode_lowercase_hex() {
        assert_eq!(percent_decode("/a%2fb"), "/a/b");
    }

    #[test]
    fn sniff_empty_is_txt() {
        assert_eq!(sniff_text_format(""), "txt");
        assert_eq!(sniff_text_format("   \n  \t"), "txt");
    }

    #[test]
    fn sniff_xml_must_have_closing_tag() {
        // Self-closing-only ("<a/>") still has no `</`, so it falls back.
        // We require a real closing tag to avoid flagging "<not xml>".
        assert_eq!(sniff_text_format("<not really xml>"), "txt");
    }

    #[test]
    fn temp_path_has_prefix_and_extension() {
        let p = temp_path("png");
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("swift-shifter-"), "got: {name}");
        assert!(name.ends_with(".png"), "got: {name}");
        assert_eq!(p.parent().unwrap(), std::env::temp_dir());
    }

    #[test]
    fn temp_path_is_unique_across_calls() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..32 {
            let p = temp_path("txt");
            assert!(seen.insert(p), "temp_path produced a duplicate");
        }
    }

    #[test]
    fn write_text_paste_writes_temp_for_plain_text() {
        let r = write_text_paste("hello world").unwrap();
        assert!(r.is_temp);
        let path = std::path::PathBuf::from(&r.path);
        assert!(path.exists());
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("txt"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_text_paste_uses_sniffed_extension_for_json() {
        let r = write_text_paste(r#"{"a": 1}"#).unwrap();
        assert!(r.is_temp);
        let path = std::path::PathBuf::from(&r.path);
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("json"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_text_paste_uses_sniffed_extension_for_toml() {
        let r = write_text_paste("[s]\nk = 1\n").unwrap();
        let path = std::path::PathBuf::from(&r.path);
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("toml"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_text_paste_returns_existing_path_for_file_url() {
        let target = temp_path("txt");
        std::fs::write(&target, b"existing").unwrap();
        let url = format!("file://{}", target.to_string_lossy());
        let r = write_text_paste(&url).unwrap();
        assert!(!r.is_temp, "should return existing path, not write temp");
        assert_eq!(std::path::PathBuf::from(&r.path), target);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn write_text_paste_decodes_percent_escapes_in_file_url() {
        // Build a real file whose name contains a space; the URL form
        // must percent-encode the space, and the shortcut must decode it.
        let dir = std::env::temp_dir();
        let target = dir.join("swift-shifter-spacey name.txt");
        std::fs::write(&target, b"hi").unwrap();
        let url = format!(
            "file://{}/swift-shifter-spacey%20name.txt",
            dir.to_string_lossy().trim_end_matches('/'),
        );
        let r = write_text_paste(&url).unwrap();
        assert!(!r.is_temp);
        assert_eq!(std::path::PathBuf::from(&r.path), target);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn write_text_paste_returns_existing_path_for_bare_absolute_path() {
        let target = temp_path("txt");
        std::fs::write(&target, b"existing").unwrap();
        let r = write_text_paste(target.to_str().unwrap()).unwrap();
        assert!(!r.is_temp);
        assert_eq!(std::path::PathBuf::from(&r.path), target);
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn write_text_paste_does_not_match_path_when_text_has_newlines() {
        // A multi-line paste that happens to *start* with an existing
        // absolute path must NOT be coerced into a file reference.
        let target = temp_path("txt");
        std::fs::write(&target, b"x").unwrap();
        let pasted = format!("{}\nmore lines\n", target.to_str().unwrap());
        let r = write_text_paste(&pasted).unwrap();
        assert!(r.is_temp, "multi-line text must be written to a temp file");
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&r.path);
    }

    #[test]
    fn write_text_paste_falls_through_when_file_url_target_missing() {
        // A `file://` URL whose target doesn't exist must fall through
        // to the temp-file path, not error and not return a stale path.
        let r = write_text_paste("file:///definitely/not/a/real/path-xyzzy").unwrap();
        assert!(r.is_temp);
        let _ = std::fs::remove_file(&r.path);
    }

    #[test]
    fn is_our_temp_file_accepts_real_temp_path() {
        let p = temp_path("png");
        std::fs::write(&p, b"x").unwrap();
        assert!(is_our_temp_file(&p));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn is_our_temp_file_rejects_missing_prefix() {
        let p = std::env::temp_dir().join("evil.png");
        assert!(!is_our_temp_file(&p));
    }

    #[test]
    fn is_our_temp_file_rejects_path_outside_temp_dir() {
        // Filename has the right prefix but lives outside temp_dir.
        let p = std::path::PathBuf::from("/etc/swift-shifter-evil.txt");
        assert!(!is_our_temp_file(&p));
    }

    #[test]
    fn is_our_temp_file_rejects_relative_path() {
        let p = std::path::PathBuf::from("swift-shifter-foo.txt");
        assert!(!is_our_temp_file(&p));
    }

    // ─── parse_file_url_string / parse_file_url_bytes ────────────────────

    #[test]
    fn parse_file_url_string_existing_file() {
        let target = temp_path("txt");
        std::fs::write(&target, b"x").unwrap();
        let url = format!("file://{}", target.to_string_lossy());
        assert_eq!(
            parse_file_url_string(&url),
            Some(target.to_string_lossy().into_owned()),
        );
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn parse_file_url_string_missing_file_returns_none() {
        assert_eq!(parse_file_url_string("file:///nope/xyzzy"), None);
    }

    #[test]
    fn parse_file_url_string_handles_trailing_nul_and_whitespace() {
        // NSPasteboard sometimes returns the URL NUL-terminated, and
        // some apps include trailing newlines. Both must be tolerated.
        let target = temp_path("txt");
        std::fs::write(&target, b"x").unwrap();
        let messy = format!("  file://{}\n\0\0  ", target.to_string_lossy());
        assert_eq!(
            parse_file_url_string(&messy),
            Some(target.to_string_lossy().into_owned()),
        );
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn parse_file_url_string_rejects_non_file_url() {
        assert_eq!(parse_file_url_string("https://example.com/foo"), None);
        assert_eq!(parse_file_url_string("not a url"), None);
    }

    #[test]
    fn parse_file_url_bytes_plain_utf8() {
        let target = temp_path("txt");
        std::fs::write(&target, b"x").unwrap();
        let bytes = format!("file://{}", target.to_string_lossy()).into_bytes();
        assert_eq!(
            parse_file_url_bytes(&bytes),
            Some(target.to_string_lossy().into_owned()),
        );
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn parse_file_url_bytes_finds_url_in_binary_wrapper() {
        // Simulate the case where NSPasteboard hands back a binary
        // plist / archived NSURL with the file URL embedded somewhere
        // in the middle. We must still extract it.
        let target = temp_path("txt");
        std::fs::write(&target, b"x").unwrap();
        let url = format!("file://{}", target.to_string_lossy());
        let mut wrapped: Vec<u8> = Vec::new();
        wrapped.extend_from_slice(b"bplist00\x00\x01\x02junk");
        wrapped.extend_from_slice(url.as_bytes());
        wrapped.extend_from_slice(b"\x00\x03\x04more junk");
        assert_eq!(
            parse_file_url_bytes(&wrapped),
            Some(target.to_string_lossy().into_owned()),
        );
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn parse_file_url_bytes_returns_none_for_non_url_bytes() {
        assert_eq!(parse_file_url_bytes(b"\x00\x01\x02\x03random binary"), None);
        assert_eq!(parse_file_url_bytes(b""), None);
    }

    // ─── dispatch_paste priority ordering ────────────────────────────────

    fn img_bytes() -> (Vec<u8>, &'static str) { (vec![0xFF, 0xD8, 0xFF], "jpg") }
    fn pdf_bytes() -> (Vec<u8>, &'static str) { (b"%PDF-1.4\n".to_vec(), "pdf") }
    fn rtf_bytes() -> (Vec<u8>, &'static str) { (b"{\\rtf1}".to_vec(), "rtf") }

    fn assert_existing_path(o: PasteOutcome, expected: &str) {
        match o {
            PasteOutcome::ExistingPath(p) => assert_eq!(p, expected),
            _ => panic!("expected ExistingPath"),
        }
    }
    fn assert_bytes(o: PasteOutcome, expected_ext: &str) {
        match o {
            PasteOutcome::Bytes(_, e) => assert_eq!(e, expected_ext),
            _ => panic!("expected Bytes"),
        }
    }
    fn assert_text(o: PasteOutcome) {
        assert!(matches!(o, PasteOutcome::Text), "expected Text outcome");
    }
    fn assert_rgba(o: PasteOutcome) {
        assert!(matches!(o, PasteOutcome::ReadRgbaImage), "expected ReadRgbaImage");
    }
    fn assert_empty(o: PasteOutcome) {
        assert!(matches!(o, PasteOutcome::Empty), "expected Empty");
    }

    // The user's reported bug: copying a JPG/PDF/GIF/WAV file in Finder
    // must surface the file path, never fall through to text. With
    // file_path = Some, dispatch must return ExistingPath regardless of
    // what other types macOS auto-promoted onto the pasteboard.
    #[test]
    fn macos_finder_file_copy_takes_priority_over_everything() {
        let r = ClipboardReads {
            file_path: Some("/Users/x/foo.jpg".into()),
            // Finder ALSO puts a text representation (the filename) on
            // the pasteboard — has_text being true must NOT win.
            has_text: true,
            has_image: true,
            ..Default::default()
        };
        assert_existing_path(dispatch_paste_macos(r), "/Users/x/foo.jpg");
    }

    #[test]
    fn macos_raw_image_beats_text_and_rich_text() {
        let r = ClipboardReads {
            raw_image: Some(img_bytes()),
            has_text: true,
            rich_text: Some(rtf_bytes()),
            ..Default::default()
        };
        assert_bytes(dispatch_paste_macos(r), "jpg");
    }

    #[test]
    fn macos_rich_doc_beats_text() {
        // Reproduces the WAV/PDF case: a rich-doc UTI is on the
        // pasteboard alongside a text description — bytes must win.
        let r = ClipboardReads {
            rich_doc: Some(pdf_bytes()),
            has_text: true,
            ..Default::default()
        };
        assert_bytes(dispatch_paste_macos(r), "pdf");
    }

    #[test]
    fn macos_text_preferred_over_image_when_both_present() {
        // Browser/Mail/Notes copies come with both a TIFF rendering and
        // plain text — the user almost always wants the text.
        let r = ClipboardReads {
            has_text: true,
            has_image: true,
            ..Default::default()
        };
        assert_text(dispatch_paste_macos(r));
    }

    #[test]
    fn macos_image_used_when_no_text() {
        // TIFF-only paste (Notes/Preview "Copy") with no text: read RGBA.
        let r = ClipboardReads { has_image: true, ..Default::default() };
        assert_rgba(dispatch_paste_macos(r));
    }

    #[test]
    fn macos_rich_text_only_as_last_resort() {
        let r = ClipboardReads {
            rich_text: Some(rtf_bytes()),
            ..Default::default()
        };
        assert_bytes(dispatch_paste_macos(r), "rtf");
    }

    #[test]
    fn macos_empty_when_nothing_present() {
        assert_empty(dispatch_paste_macos(ClipboardReads::default()));
    }

    #[test]
    fn macos_text_wins_over_rich_text_when_both_present() {
        // RTF is fallback ONLY — when plain text exists it takes precedence.
        let r = ClipboardReads {
            has_text: true,
            rich_text: Some(rtf_bytes()),
            ..Default::default()
        };
        assert_text(dispatch_paste_macos(r));
    }

    #[test]
    fn other_file_path_takes_priority() {
        let r = ClipboardReads {
            file_path: Some("/tmp/foo.jpg".into()),
            ..Default::default()
        };
        assert_existing_path(dispatch_paste_other(r), "/tmp/foo.jpg");
    }

    #[test]
    fn other_raw_image_beats_rgba_fallback() {
        let r = ClipboardReads {
            raw_image: Some(img_bytes()),
            ..Default::default()
        };
        assert_bytes(dispatch_paste_other(r), "jpg");
    }

    #[test]
    fn other_falls_through_to_rgba_image() {
        // Non-mac fallback always probes Tauri read_image() / read_text()
        // last because we can't ask for "what's on the clipboard" up front.
        assert_rgba(dispatch_paste_other(ClipboardReads::default()));
    }

    // ─── Real NSPasteboard integration tests (macOS only) ────────────
    //
    // These tests touch the *actual* general pasteboard. They restore
    // it at the end so they don't permanently clobber the user's
    // clipboard, but they DO interact with shared OS state — run them
    // serialised. They're the only way to verify our objc2 plumbing
    // talks to NSPasteboard correctly; the dispatch tests above only
    // exercise pure logic.
    //
    // Marked `#[serial]` would be ideal; we rely on cargo test's
    // default single-threaded behaviour for `--test-threads=1` runs and
    // accept transient failures otherwise (the helpers re-write before
    // each read).

    #[cfg(target_os = "macos")]
    fn pb_clear_and_set_string(uti: &str, content: &str) {
        use objc2::msg_send;
        use objc2::rc::autoreleasepool;
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2_foundation::NSString;
        autoreleasepool(|_| unsafe {
            let pb_class = AnyClass::get(c"NSPasteboard").unwrap();
            let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
            assert!(!pb.is_null(), "generalPasteboard returned nil");
            let _: i64 = msg_send![pb, clearContents];
            let type_str = NSString::from_str(uti);
            let value_str = NSString::from_str(content);
            let _ok: bool = msg_send![pb, setString: &*value_str, forType: &*type_str];
        });
    }

    #[cfg(target_os = "macos")]
    fn pb_clear() {
        use objc2::msg_send;
        use objc2::rc::autoreleasepool;
        use objc2::runtime::{AnyClass, AnyObject};
        autoreleasepool(|_| unsafe {
            let pb_class = AnyClass::get(c"NSPasteboard").unwrap();
            let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
            let _: i64 = msg_send![pb, clearContents];
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pasteboard_round_trip_utf8_string() {
        // Sanity check: if `pasteboard_data_for_uti` is broken at the
        // objc2 layer, every macOS paste falls through to text. This
        // test fails *loudly* in that case.
        pb_clear_and_set_string("public.utf8-plain-text", "hello-clipboard-test-xyzzy");
        let bytes = pasteboard_data_for_uti("public.utf8-plain-text")
            .expect("pasteboard_data_for_uti returned None — objc2 layer is broken");
        assert_eq!(std::str::from_utf8(&bytes).unwrap(), "hello-clipboard-test-xyzzy");
        pb_clear();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pasteboard_string_for_uti_round_trip() {
        pb_clear_and_set_string("public.utf8-plain-text", "string-rt-test");
        let s = pasteboard_string_for_uti("public.utf8-plain-text")
            .expect("stringForType: returned nil");
        assert_eq!(s, "string-rt-test");
        pb_clear();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn read_file_path_finds_file_url_via_string_form() {
        // The user's reported bug: copying a file should resolve to the
        // file path. Simulate the Finder copy by setting a `public.file-url`
        // string entry pointing at a real temp file.
        let target = temp_path("txt");
        std::fs::write(&target, b"x").unwrap();
        let url = format!("file://{}", target.to_string_lossy());
        pb_clear_and_set_string("public.file-url", &url);
        let got = read_file_path_from_pasteboard()
            .expect("read_file_path_from_pasteboard returned None");
        assert_eq!(
            std::path::PathBuf::from(&got).canonicalize().unwrap(),
            target.canonicalize().unwrap(),
        );
        pb_clear();
        let _ = std::fs::remove_file(&target);
    }

    /// Write a file URL via `NSPasteboardItem` + `writeObjects:` — the
    /// modern API path Finder uses. Lets us verify the reader handles
    /// what real Finder copies look like, not just the legacy
    /// `setString:forType:` shape.
    #[cfg(target_os = "macos")]
    fn pb_write_file_url_as_finder_does(url: &str) {
        use objc2::msg_send;
        use objc2::rc::autoreleasepool;
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2_foundation::NSString;
        autoreleasepool(|_| unsafe {
            let pb_class = AnyClass::get(c"NSPasteboard").unwrap();
            let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
            let _: i64 = msg_send![pb, clearContents];

            let item_class = AnyClass::get(c"NSPasteboardItem").unwrap();
            let item: *mut AnyObject = msg_send![item_class, alloc];
            let item: *mut AnyObject = msg_send![item, init];

            let url_str = NSString::from_str(url);
            let uti = NSString::from_str("public.file-url");
            let _ok: bool = msg_send![item, setString: &*url_str, forType: &*uti];

            let array_class = AnyClass::get(c"NSArray").unwrap();
            let arr: *mut AnyObject = msg_send![array_class, arrayWithObject: item];
            let _ok: bool = msg_send![pb, writeObjects: arr];
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn read_file_path_finds_finder_style_write_objects() {
        let target = temp_path("txt");
        std::fs::write(&target, b"x").unwrap();
        let url = format!("file://{}", target.to_string_lossy());
        pb_write_file_url_as_finder_does(&url);
        let got = read_file_path_from_pasteboard()
            .expect("Finder-style writeObjects: file URL not detected");
        assert_eq!(
            std::path::PathBuf::from(&got).canonicalize().unwrap(),
            target.canonicalize().unwrap(),
        );
        pb_clear();
        let _ = std::fs::remove_file(&target);
    }

    /// Diagnostic: dumps what's currently on the system pasteboard.
    /// Run with: `cargo test diagnose_current_pasteboard -- --ignored
    /// --nocapture --test-threads=1`. Copy a file in Finder *before*
    /// running this; the output tells us exactly which UTI Finder is
    /// vending and what shape the data is in.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn diagnose_current_pasteboard() {
        println!("=== Available pasteboard types ===");
        for t in pasteboard_available_types() {
            println!("- {t}");
            if let Some(s) = pasteboard_string_for_uti(&t) {
                let preview: String = s.chars().take(120).collect();
                println!("    stringForType: {preview:?}");
            }
            if let Some(d) = pasteboard_data_for_uti(&t) {
                println!("    dataForType:   {} bytes", d.len());
                if d.len() <= 200 {
                    println!("    raw lossy:     {:?}", String::from_utf8_lossy(&d));
                }
            }
        }
        println!("=== read_file_path_from_pasteboard() = {:?} ===",
                 read_file_path_from_pasteboard());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pasteboard_available_types_includes_what_we_set() {
        pb_clear_and_set_string("public.utf8-plain-text", "x");
        let types = pasteboard_available_types();
        assert!(
            types.iter().any(|t| t == "public.utf8-plain-text"),
            "available types missing what we set: {types:?}",
        );
        pb_clear();
    }
}
