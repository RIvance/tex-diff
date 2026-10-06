use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub(crate) fn open_pdf(path: &Path, selected: Option<&Path>) -> Result<()> {
    let selected = selected
        .map(Path::to_owned)
        .or_else(|| std::env::var_os("TEX_DIFF_VIEWER").map(PathBuf::from));

    #[cfg(target_os = "linux")]
    if selected.is_none()
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        eprintln!("PDF saved; no desktop display is available to open a viewer.");
        return Ok(());
    }
    let mut command = if let Some(viewer) = selected {
        Command::new(viewer)
    } else {
        #[cfg(target_os = "macos")]
        let command = Command::new("open");

        #[cfg(target_os = "windows")]
        let command = {
            let mut c = Command::new("cmd");
            c.args(["/C", "start", ""]);
            c
        };

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let command = Command::new("xdg-open");
        command
    };
    command
        .arg(fs::canonicalize(path)?)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match command.spawn() {
        Ok(mut child) => {
            std::thread::sleep(Duration::from_millis(150));
            if let Some(status) = child.try_wait()?
                && !status.success()
            {
                eprintln!("PDF saved, but the viewer exited with {status}.");
            }
        }
        Err(error) => eprintln!(
            "PDF saved, but the viewer could not start: {error}. Set --viewer to your PDF reader."
        ),
    }
    Ok(())
}
