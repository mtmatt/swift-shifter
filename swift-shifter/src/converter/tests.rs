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
}
