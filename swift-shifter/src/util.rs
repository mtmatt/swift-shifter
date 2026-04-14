/// Suppress the flash of a console window when a GUI app on Windows spawns
/// a subprocess.  A no-op on macOS and Linux.
///
/// Usage:
/// ```rust
/// let mut cmd = tokio::process::Command::new("winget");
/// crate::no_window!(cmd);
/// cmd.args([...]).status().await?;
/// ```
#[macro_export]
macro_rules! no_window {
    ($cmd:expr) => {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt as _;
            $cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
    };
}
