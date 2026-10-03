//! Command-line tools Parquetry runs (uv, the AWS CLI): finding them, and
//! stopping what they started.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Find `name` (without `.exe`) on PATH or where installers usually put it. Apps
/// opened from the Finder don't get the shell's PATH, so the usual places are
/// searched too.
pub fn find(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".cargo").join("bin"));
    }
    if cfg!(windows) {
        if let Some(local) = dirs::data_local_dir() {
            dirs.push(local.join("Programs").join(name));
        }
        dirs.push(PathBuf::from(r"C:\Program Files\Amazon\AWSCLIV2"));
    } else {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    }
    dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.is_file())
}

/// A command that runs without a console window (Windows) in its own process
/// group (Unix), so `stop` can end it along with whatever it starts.
pub fn command(program: &std::path::Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command
}

/// Stop process `pid` (started with `command`) and the processes it started.
pub fn stop(pid: u32) {
    #[cfg(unix)]
    let mut command = {
        let mut c = Command::new("kill");
        c.args(["-TERM", &format!("-{pid}")]);
        c
    };
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt as _;
        let mut c = Command::new("taskkill");
        c.args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000);
        c
    };
    let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
}
