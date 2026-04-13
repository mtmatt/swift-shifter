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
}
