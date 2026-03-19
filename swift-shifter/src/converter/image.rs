use std::path::{Path, PathBuf};

fn output_path(input: &str, ext: &str) -> PathBuf {
    let p = Path::new(input);
    let stem = p.file_stem().unwrap_or_default();
    let dir = p.parent().unwrap_or(Path::new("."));
    dir.join(format!("{}.{}", stem.to_string_lossy(), ext))
}

pub fn convert_image(path: &str, target_format: &str) -> Result<String, String> {
    let img = image::open(path).map_err(|e| format!("Failed to open image: {e}"))?;
    let out = output_path(path, target_format);
    let fmt = match target_format {
        "png" => image::ImageFormat::Png,
        "jpg" | "jpeg" => image::ImageFormat::Jpeg,
        "webp" => image::ImageFormat::WebP,
        "bmp" => image::ImageFormat::Bmp,
        "tiff" | "tif" => image::ImageFormat::Tiff,
        "gif" => image::ImageFormat::Gif,
        other => return Err(format!("Unknown image format: {other}")),
    };
    img.save_with_format(&out, fmt)
        .map_err(|e| format!("Failed to save image: {e}"))?;
    Ok(out.to_string_lossy().to_string())
}

pub fn convert_to_avif(path: &str) -> Result<String, String> {
    use ravif::{Encoder, Img};
    use rgb::FromSlice;

    let img = image::open(path)
        .map_err(|e| format!("Failed to open image: {e}"))?
        .to_rgba8();
    let (width, height) = img.dimensions();
    let pixels = img.into_raw();
    let rgba_pixels = pixels.as_rgba();

    let enc = Encoder::new()
        .with_quality(80.0)
        .with_speed(6);

    let result = enc
        .encode_rgba(Img::new(rgba_pixels, width as usize, height as usize))
        .map_err(|e| format!("AVIF encoding failed: {e}"))?;

    let out = output_path(path, "avif");
    std::fs::write(&out, result.avif_file).map_err(|e| format!("Failed to write AVIF: {e}"))?;
    Ok(out.to_string_lossy().to_string())
}
