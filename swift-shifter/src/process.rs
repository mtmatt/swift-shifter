//! Subprocess constructors. Spawn external tools through these rather than
//! `Command::new` so the release build (a GUI-subsystem app on Windows) does
//! not flash a console window for every child process.

use std::ffi::OsStr;

/// Windows `CREATE_NO_WINDOW` process creation flag.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `tokio::process::Command::new` without a console window on Windows.
pub fn async_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)] // only mutated on Windows
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// `std::process::Command::new` without a console window on Windows.
pub fn sync_command(program: impl AsRef<OsStr>) -> std::process::Command {
    #[allow(unused_mut)] // only mutated on Windows
    let mut cmd = std::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
