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
}
