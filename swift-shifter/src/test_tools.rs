//! The outside tools tests check output with (#114).

/// `found`, what looking up the tool `name` gave, or `None` where it isn't
/// installed and the check is skipped. CI installs every tool a test needs,
/// so there a missing one fails the test rather than skipping its check.
pub fn installed<T>(name: &str, found: Option<T>) -> Option<T> {
    if found.is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "{name} isn't installed, and CI checks with it"
        );
        eprintln!("skipping: {name} not found");
    }
    found
}
