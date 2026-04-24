use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

#[derive(serde::Serialize)]
pub struct PasteResult {
    pub path: String,
    pub is_temp: bool,
}

/// Detect image format from magic bytes and return a file extension.
fn sniff_image_format(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") { return "png"; }
    if bytes.starts_with(b"\xFF\xD8\xFF") { return "jpg"; }
    if bytes.starts_with(b"GIF8") { return "gif"; }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return "webp";
    }
    if bytes.starts_with(b"BM") { return "bmp"; }
    if bytes.starts_with(b"II\x2A\x00") || bytes.starts_with(b"MM\x00\x2A") {
        return "tif";
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        match &bytes[8..12] {
            b"avif" | b"avis" => return "avif",
            b"heic" | b"heix" | b"mif1" | b"msf1" => return "heic",
            _ => {}
        }
    }
    "png"
}

/// On Linux, read raw image bytes from the clipboard by trying xclip (X11)
/// then wl-paste (Wayland) for each MIME type in preference order.
#[cfg(target_os = "linux")]
fn read_raw_image_from_clipboard_linux() -> Option<(Vec<u8>, &'static str)> {
    use std::process::Command;

    const MIME_TYPES: &[&str] = &[
        "image/jpeg",
        "image/png",
        "image/gif",
        "image/webp",
        "image/tiff",
        "image/bmp",
    ];

    for tool in &[
        &["xclip", "-selection", "clipboard", "-t", "", "-o"][..],
        &["wl-paste", "--type", "", "--no-newline"][..],
    ] {
        for &mime in MIME_TYPES {
            let mut args: Vec<&str> = tool.to_vec();
            // Replace the placeholder empty string with the actual MIME type.
            if let Some(slot) = args.iter_mut().find(|a| a.is_empty()) {
                *slot = mime;
            }
            let bin = args.remove(0);
            if let Ok(out) = Command::new(bin).args(&args).output() {
                if out.status.success() && !out.stdout.is_empty() {
                    let ext = sniff_image_format(&out.stdout);
                    return Some((out.stdout, ext));
                }
            }
        }
    }
    None
}

/// On macOS, read raw image bytes from NSPasteboard by trying each UTI in
/// preference order. Returns the bytes and a format extension detected from
/// the magic bytes.
#[cfg(target_os = "macos")]
fn read_raw_image_from_pasteboard() -> Option<(Vec<u8>, &'static str)> {
    const UTIS: &[&str] = &[
        "public.jpeg",
        "public.png",
        "com.compuserve.gif",
        "org.webmproject.webp",
        "public.heic",
        "public.avif",
        "public.tiff",
    ];
    unsafe {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2_foundation::NSString;

        let pb_class = AnyClass::get(c"NSPasteboard")?;
        let pb: *mut AnyObject = msg_send![pb_class, generalPasteboard];
        if pb.is_null() { return None; }

        for &uti in UTIS {
            let type_str = NSString::from_str(uti);
            let data: *mut AnyObject = msg_send![pb, dataForType: &*type_str];
            if data.is_null() { continue; }
            let len: usize = msg_send![data, length];
            if len == 0 { continue; }
            let ptr: *const u8 = msg_send![data, bytes];
            if ptr.is_null() { continue; }
            let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
            let ext = sniff_image_format(&bytes);
            return Some((bytes, ext));
        }
        None
    }
}

/// Heuristically detect format of clipboard text to pick a file extension.
fn sniff_text_format(text: &str) -> &'static str {
    let trimmed = text.trim_start();

    // Valid JSON?
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        return "json";
    }

    // TOML: has [Section] header lines AND key = value assignments
    if text.lines().any(|l| {
        let t = l.trim();
        t.starts_with('[') && t.ends_with(']') && t.len() > 2
    }) && text.contains(" = ") {
        return "toml";
    }

    // YAML: majority of non-empty lines are "key: value" where key is a bare identifier
    let non_empty: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if non_empty.len() >= 2 {
        let yaml_like = non_empty.iter().filter(|&&l| {
            let t = l.trim_start();
            if let Some(pos) = t.find(':') {
                let key = &t[..pos];
                !key.is_empty()
                    && !key.contains(' ')
                    && !key.contains('"')
                    && !key.contains('/')   // exclude URLs
                    && !key.contains('.')   // exclude "example.com"
            } else {
                false
            }
        }).count();
        if yaml_like * 2 > non_empty.len() {
            return "yaml";
        }
    }

    // CSV: first line contains commas
    if text.lines().next().map(|l| l.contains(',')).unwrap_or(false) {
        return "csv";
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

#[tauri::command]
pub async fn paste_from_clipboard(app: AppHandle) -> Result<PasteResult, String> {
    let app2 = app.clone();
    tokio::task::spawn_blocking(move || {
        // 1. Try image with raw-byte format detection.
        //    macOS: NSPasteboard UTI types carry the original codec bytes.
        //    Linux: xclip/wl-paste support MIME-type negotiation.
        //    Windows: the clipboard standard only stores decoded bitmaps
        //             (CF_DIB), so we fall through to the PNG path below.
        #[cfg(target_os = "macos")]
        if let Some((raw_bytes, ext)) = read_raw_image_from_pasteboard() {
            let p = temp_path(ext);
            std::fs::write(&p, &raw_bytes).map_err(|e| e.to_string())?;
            return Ok(PasteResult { path: p.to_string_lossy().into_owned(), is_temp: true });
        }
        #[cfg(target_os = "linux")]
        if let Some((raw_bytes, ext)) = read_raw_image_from_clipboard_linux() {
            let p = temp_path(ext);
            std::fs::write(&p, &raw_bytes).map_err(|e| e.to_string())?;
            return Ok(PasteResult { path: p.to_string_lossy().into_owned(), is_temp: true });
        }
        if let Ok(img) = app2.clipboard().read_image() {
            let width = img.width();
            let height = img.height();
            let rgba = img.rgba().to_vec();
            let rb = image::RgbaImage::from_raw(width, height, rgba)
                .ok_or("Invalid clipboard image dimensions")?;
            let p = temp_path("png");
            rb.save(&p).map_err(|e| e.to_string())?;
            return Ok(PasteResult {
                path: p.to_string_lossy().into_owned(),
                is_temp: true,
            });
        }

        // 2. Try text
        let text = app2
            .clipboard()
            .read_text()
            .map_err(|e| format!("Could not read clipboard: {e}"))?;
        if text.trim().is_empty() {
            return Err("Clipboard is empty".to_string());
        }

        // 2a. If text is a valid existing file path, return it directly
        let trimmed = text.trim().to_string();
        let candidate = std::path::Path::new(&trimmed);
        if candidate.is_file() {
            return Ok(PasteResult { path: trimmed, is_temp: false });
        }

        // 2b. Sniff format, write temp file
        let ext = sniff_text_format(&text);
        let p = temp_path(ext);
        std::fs::write(&p, text.as_bytes()).map_err(|e| e.to_string())?;
        Ok(PasteResult {
            path: p.to_string_lossy().into_owned(),
            is_temp: true,
        })
    })
    .await
    .map_err(|e| format!("Clipboard task panicked: {e}"))?
}

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "tif", "avif"];
const TEXT_EXTS: &[&str] = &["json", "yaml", "yml", "toml", "csv", "txt", "md", "html", "xml"];

#[tauri::command]
pub async fn copy_file_to_clipboard(app: AppHandle, path: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let ext = std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        if IMAGE_EXTS.contains(&ext.as_str()) {
            let img = image::open(&path).map_err(|e| e.to_string())?;
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            let raw = rgba.into_raw();
            let cb_img = tauri::image::Image::new_owned(raw, w, h);
            app.clipboard().write_image(&cb_img).map_err(|e| e.to_string())?;
        } else if TEXT_EXTS.contains(&ext.as_str()) {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            app.clipboard().write_text(text).map_err(|e| e.to_string())?;
        } else {
            return Err(format!("Cannot copy .{ext} files to clipboard"));
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Clipboard task panicked: {e}"))?
}

#[tauri::command]
pub async fn remove_temp_file(path: String) -> Result<(), String> {
    std::fs::remove_file(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_format_png() {
        assert_eq!(sniff_image_format(b"\x89PNG\r\n\x1a\n"), "png");
    }

    #[test]
    fn image_format_jpeg() {
        assert_eq!(sniff_image_format(b"\xFF\xD8\xFF\xE0"), "jpg");
    }

    #[test]
    fn image_format_gif() {
        assert_eq!(sniff_image_format(b"GIF89a"), "gif");
    }

    #[test]
    fn image_format_webp() {
        let mut bytes = b"RIFF\x00\x00\x00\x00WEBP".to_vec();
        bytes.extend_from_slice(b"VP8 ");
        assert_eq!(sniff_image_format(&bytes), "webp");
    }

    #[test]
    fn image_format_tiff_le() {
        assert_eq!(sniff_image_format(b"II\x2A\x00"), "tif");
    }

    #[test]
    fn image_format_tiff_be() {
        assert_eq!(sniff_image_format(b"MM\x00\x2A"), "tif");
    }

    #[test]
    fn image_format_unknown_defaults_to_png() {
        assert_eq!(sniff_image_format(b"\x00\x00\x00\x00"), "png");
    }

    #[test]
    fn image_format_empty() {
        assert_eq!(sniff_image_format(b""), "png");
    }

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
    fn sniff_yaml() {
        assert_eq!(sniff_text_format("name: Alice\nage: 30\ncity: NYC\n"), "yaml");
    }

    #[test]
    fn sniff_csv() {
        assert_eq!(sniff_text_format("name,age,city\nAlice,30,NYC\n"), "csv");
    }

    #[test]
    fn sniff_plain_text() {
        assert_eq!(sniff_text_format("hello world"), "txt");
    }

    #[test]
    fn invalid_json_not_sniffed_as_json() {
        assert_ne!(sniff_text_format("{not valid json}"), "json");
    }
}
