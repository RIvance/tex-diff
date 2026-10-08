use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const LOG_TAIL_BYTES: u64 = 32 * 1024;

const LOG_TAIL_LINES: usize = 18;

const LOG_SCAN_BYTES: u64 = 8 * 1024 * 1024;

const MAX_LOG_ERRORS: usize = 6;

/// Run a command with captured diagnostics and a deadline for its process group.
pub fn run_logged(mut cmd: Command, log: &Path, timeout: Duration) -> Result<()> {
    // The source worker already belongs to a group controlled by its parent.
    // Keep BibTeX in that group so a parent timeout kills the whole subtree.
    let own_group = std::env::args_os()
        .nth(1)
        .is_none_or(|arg| arg != "__source_compare");
    let output = File::create(log)?;
    cmd.stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if own_group {
            cmd.process_group(0);
        }
    }
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "could not start {:?}; check that it is installed",
            cmd.get_program()
        )
    })?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    // A failed worker can exit before its subprocesses do.
                    terminate(&mut child, own_group);
                    anyhow::bail!(
                        "{:?} failed ({status}); see {}\n{}",
                        cmd.get_program(),
                        log.display(),
                        log_diagnostics(log)
                    );
                }
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => {
                terminate(&mut child, own_group);
                return Err(error).with_context(|| format!("waiting for {:?}", cmd.get_program()));
            }
        }
        if start.elapsed() >= timeout {
            terminate(&mut child, own_group);
            anyhow::bail!(
                "{:?} exceeded {} seconds; see {}",
                cmd.get_program(),
                timeout.as_secs(),
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn terminate(child: &mut Child, own_group: bool) {
    #[cfg(unix)]
    if own_group {
        // The command created its own group before spawning, so its PID is
        // also the group ID. Kill descendants before reaping the child.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }

    #[cfg(not(unix))]
    let _ = own_group;

    let _ = child.kill();
    let _ = child.wait();
}

/// Compile a document with latexmk without project rc files or shell escape.
pub fn compile(
    snapshot: &Path,
    main: &Path,
    build: &Path,
    engine: &str,
    timeout: Duration,
) -> Result<PathBuf> {
    fs::create_dir_all(build)?;
    let source = snapshot.join(main);
    ensure!(
        source.is_file(),
        "main document does not exist: {}",
        main.display()
    );
    let mut cmd = Command::new("latexmk");
    let source_directory = source
        .parent()
        .context("main document has no parent directory")?;
    cmd.current_dir(source_directory)
        .args([
            "-norc",
            match engine {
                "xelatex" => "-xelatex",
                "lualatex" => "-lualatex",
                _ => "-pdf",
            },
            "-interaction=nonstopmode",
            "-file-line-error",
            "-latexoption=-no-shell-escape",
        ])
        .arg(format!("-outdir={}", build.display()))
        .arg(&source);
    run_logged(cmd, &build.join("build.log"), timeout)?;
    let pdf = build
        .join(
            main.file_name()
                .context("main document must have a filename")?,
        )
        .with_extension("pdf");
    ensure!(
        pdf.is_file(),
        "LaTeX reported success but did not produce {}",
        pdf.display()
    );
    Ok(pdf)
}

fn tex_error(line: &str) -> bool {
    if line.starts_with("! ") {
        return true;
    }
    line.match_indices(": ").any(|(end, _)| {
        line[..end].rsplit_once(':').is_some_and(|(file, number)| {
            !file.is_empty()
                && !number.is_empty()
                && number.bytes().all(|byte| byte.is_ascii_digit())
        })
    })
}

fn log_diagnostics(path: &Path) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let mut diagnostics = Vec::new();
    let mut wrapped = String::new();
    let mut context = 0;
    let mut errors = 0;
    // TeX errors can precede pages of warnings. Scan with bounded memory and
    // output, retaining error locations and the following source context.
    for line in BufReader::new(file).take(LOG_SCAN_BYTES).split(b'\n') {
        let Ok(bytes) = line else { break };
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim_end_matches('\r');
        let starts_path = line.starts_with('/')
            || line.starts_with("./")
            || line.starts_with("../")
            || line.as_bytes().get(1) == Some(&b':');
        let joined = format!("{wrapped}{line}");
        let error = if tex_error(line) {
            Some(line)
        } else if tex_error(&joined) {
            Some(joined.as_str())
        } else {
            None
        };
        if let Some(error) = error {
            if errors == MAX_LOG_ERRORS {
                diagnostics.push("Further errors are in the full log.".to_owned());
                break;
            }
            if errors > 0 {
                diagnostics.push(String::new());
            }
            diagnostics.push(error.chars().take(2048).collect());
            errors += 1;
            context = 5;
            wrapped.clear();
        } else {
            if context > 0 {
                if starts_path {
                    context = 0;
                } else {
                    diagnostics.push(line.chars().take(2048).collect());
                    context = if line.is_empty() { 0 } else { context - 1 };
                }
            }
            // TeX wraps long file paths, sometimes even within a line number.
            // A new path also separates diagnostics from package-loading noise.
            if line.is_empty() || starts_path || wrapped.len() + line.len() > 2048 {
                wrapped.clear();
            }
            if wrapped.len() + line.len() <= 2048 {
                wrapped.push_str(line);
            }
        }
    }
    if errors == 0 {
        log_tail(path)
    } else {
        diagnostics.join("\n")
    }
}

fn log_tail(path: &Path) -> String {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(LOG_TAIL_BYTES)))?;
        let mut bytes = Vec::new();
        file.take(LOG_TAIL_BYTES).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let bytes = read().unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .rev()
        .take(LOG_TAIL_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "../tests/unit/build.rs"]
mod tests;
