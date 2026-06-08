#[cfg(test)]
mod tests {
    #[test]
    fn test_find_pymupdf4llm_python_does_not_panic() {
        let _result = crate::converter::document::find_pymupdf4llm_python();
    }

    #[test]
    fn test_merge_pdfs_requires_at_least_two_inputs() {
        let result = crate::converter::document::merge_pdfs(&[], None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least 2"));

        let result = crate::converter::document::merge_pdfs(&["/tmp/a.pdf".to_string()], None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("at least 2"));
    }

    #[test]
    fn test_trim_output_path_appends_trim_suffix() {
        let out =
            crate::converter::media::trim_output_path("/home/user/interview.mp3", None).unwrap();
        assert_eq!(
            out.file_name().unwrap().to_str().unwrap(),
            "interview-trim.mp3"
        );
        assert_eq!(out.parent().unwrap(), std::path::Path::new("/home/user"));
    }

    #[test]
    fn test_trim_output_path_uses_output_dir() {
        let out = crate::converter::media::trim_output_path(
            "/home/user/clip.mp4",
            Some("/tmp/output"),
        )
        .unwrap();
        assert_eq!(out.file_name().unwrap().to_str().unwrap(), "clip-trim.mp4");
        assert!(out.starts_with("/tmp/output"));
    }

    #[test]
    fn test_csv_does_not_offer_toml() {
        // CSV is always an array of records; TOML has no top-level array-of-tables.
        let formats = crate::converter::detect_output_formats("/tmp/data.csv").unwrap();
        assert!(!formats.contains(&"toml".to_string()), "csv should not offer toml");
        assert!(formats.contains(&"json".to_string()));
        assert!(formats.contains(&"yaml".to_string()));
    }

    #[test]
    fn test_data_array_to_toml_errors_clearly() {
        let dir = std::env::temp_dir();
        let input = dir.join(format!("ss_arr_{}.json", std::process::id()));
        std::fs::write(&input, r#"[{"a":1},{"a":2}]"#).unwrap();
        let result = crate::converter::data::convert_data(
            input.to_str().unwrap(),
            "toml",
            Some(dir.to_str().unwrap()),
        );
        let _ = std::fs::remove_file(&input);
        let err = result.expect_err("array → toml must fail");
        assert!(
            err.contains("no top-level array"),
            "expected a clear top-level-array message, got: {err}"
        );
    }

    #[test]
    fn test_find_typst_binary_does_not_panic() {
        let _ = crate::converter::document::find_typst_binary();
    }

    /// Locks the exact direct-output surface before the graph refactor.
    /// macOS values (HEIC offered) — see platform test for the non-macOS delta.
    #[test]
    #[cfg(target_os = "macos")]
    fn test_detect_output_formats_snapshot() {
        use crate::converter::detect_output_formats as d;
        let cases: &[(&str, &[&str])] = &[
            ("/x.png",  &["jpg","webp","avif","gif","bmp","tiff","heic","pdf"]),
            ("/x.jpg",  &["png","webp","avif","gif","bmp","tiff","heic","pdf"]),
            ("/x.jpeg", &["png","webp","avif","gif","bmp","tiff","heic","pdf"]),
            ("/x.webp", &["png","jpg","avif","gif","bmp","tiff","heic","pdf"]),
            ("/x.gif",  &["png","jpg","webp","avif","bmp","tiff","heic","pdf"]),
            ("/x.bmp",  &["png","jpg","webp","avif","gif","tiff","heic","pdf"]),
            ("/x.tiff", &["png","jpg","webp","avif","gif","bmp","heic","pdf"]),
            // .tif normalizes to tiff in the graph, so the pointless tif->tiff
            // identity conversion is no longer offered (intentional delta).
            ("/x.tif",  &["png","jpg","webp","avif","gif","bmp","heic","pdf"]),
            ("/x.avif", &["png","jpg","webp","gif","bmp","tiff","heic","pdf"]),
            ("/x.heic", &["jpg","png","tiff","gif","bmp","pdf"]),
            ("/x.heif", &["jpg","png","tiff","gif","bmp","pdf"]),
            ("/x.mp4",  &["mov","mkv","webm","avi","gif"]),
            ("/x.mov",  &["mp4","mkv","webm","avi","gif"]),
            ("/x.mkv",  &["mp4","mov","webm","avi","gif"]),
            ("/x.webm", &["mp4","mov","mkv","avi","gif"]),
            ("/x.avi",  &["mp4","mov","mkv","webm","gif"]),
            ("/x.mp3",  &["aac","flac","ogg","wav","opus","m4a"]),
            ("/x.aac",  &["mp3","flac","ogg","wav","opus","m4a"]),
            ("/x.flac", &["mp3","aac","ogg","wav","opus","m4a"]),
            ("/x.ogg",  &["mp3","aac","flac","wav","opus","m4a"]),
            ("/x.wav",  &["mp3","aac","flac","ogg","opus","m4a"]),
            ("/x.opus", &["mp3","aac","flac","ogg","wav","m4a"]),
            ("/x.m4a",  &["mp3","aac","flac","ogg","wav","opus"]),
            ("/x.json", &["yaml","toml","csv"]),
            ("/x.yaml", &["json","toml","csv"]),
            ("/x.yml",  &["json","toml","csv"]),
            ("/x.toml", &["json","yaml","csv"]),
            ("/x.csv",  &["json","yaml"]),
            ("/x.md",   &["txt","html","pdf","tex","typst"]),
            ("/x.markdown", &["txt","html","pdf","tex","typst"]),
            ("/x.txt",  &["md","html","pdf","tex","typst"]),
            ("/x.tex",  &["md","html","pdf","typst"]),
            ("/x.latex",&["md","html","pdf","typst"]),
            ("/x.typst",&["md","html","pdf","tex"]),
            ("/x.epub", &["pdf","mobi","md","html"]),
            ("/x.mobi", &["epub","pdf","html","md"]),
            ("/x.pdf",  &["epub","mobi","html","md"]),
        ];
        for (path, expected) in cases {
            let mut got = d(path).unwrap();
            let mut want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
            got.sort();
            want.sort();
            assert_eq!(got, want, "direct formats for {path} changed");
        }
        assert!(d("/x.xyz").is_err(), "unknown extension must error");
    }
}
