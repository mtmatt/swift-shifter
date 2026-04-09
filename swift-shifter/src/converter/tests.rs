#[cfg(test)]
mod tests {
    #[test]
    fn test_find_pymupdf4llm_python_does_not_panic() {
        // Returns Some(path) if pymupdf4llm is installed, None otherwise. Never panics.
        let _result = crate::converter::document::find_pymupdf4llm_python();
    }
}
