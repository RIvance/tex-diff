use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Revision {
    Empty,
    Index,
    Worktree,
    Commit(String),
}

impl Revision {
    pub fn label(&self) -> String {
        match self {
            Self::Empty => "empty tree".into(),
            Self::Index => "index".into(),
            Self::Worktree => "working tree".into(),
            Self::Commit(id) => id.chars().take(12).collect(),
        }
    }
}

pub struct Repository {
    pub root: PathBuf,
}

#[derive(Debug)]
struct Entry {
    mode: String,
    oid: String,
    path: PathBuf,
}

impl Repository {
    pub fn discover(cwd: &Path) -> Result<Self> {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("could not run git; install Git and run inside a repository")?;
        ensure!(
            out.status.success(),
            "not inside a Git working tree: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(Self {
            root: path_from_bytes(out.stdout.strip_suffix(b"\n").unwrap_or(&out.stdout))?,
        })
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.root);
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let out = self
            .command()
            .args(args)
            .output()
            .context("could not run git")?;
        ensure!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(out.stdout)
    }

    fn resolve(&self, name: &str) -> Result<String> {
        self.run(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{name}^{{tree}}"),
        ])?;
        Ok(
            String::from_utf8(self.run(&["rev-parse", "--verify", "--end-of-options", name])?)?
                .trim()
                .into(),
        )
    }

    pub fn select(&self, staged: bool, names: &[String]) -> Result<(Revision, Revision)> {
        ensure!(names.len() <= 2, "specify at most two revisions");
        if staged {
            ensure!(
                names.len() <= 1,
                "--staged accepts at most one base revision"
            );
            ensure!(
                !names.first().is_some_and(|s| s.contains("..")),
                "revision ranges cannot be combined with --staged"
            );
            let old = if let Some(name) = names.first() {
                Revision::Commit(self.resolve(name)?)
            } else if self
                .command()
                .args(["rev-parse", "--verify", "HEAD"])
                .output()?
                .status
                .success()
            {
                Revision::Commit(self.resolve("HEAD")?)
            } else {
                Revision::Empty
            };
            return Ok((old, Revision::Index));
        }
        match names {
            [] => Ok((Revision::Index, Revision::Worktree)),
            [range] if range.contains("...") => {
                let (a, b) = range.split_once("...").unwrap();
                ensure!(!b.contains(".."), "invalid revision range: {range}");
                let a = self.resolve(if a.is_empty() { "HEAD" } else { a })?;
                let b = self.resolve(if b.is_empty() { "HEAD" } else { b })?;
                let base = String::from_utf8(self.run(&["merge-base", &a, &b])?)?
                    .trim()
                    .to_owned();
                Ok((Revision::Commit(base), Revision::Commit(b)))
            }
            [range] if range.contains("..") => {
                let (a, b) = range.split_once("..").unwrap();
                ensure!(!b.contains(".."), "invalid revision range: {range}");
                Ok((
                    Revision::Commit(self.resolve(if a.is_empty() { "HEAD" } else { a })?),
                    Revision::Commit(self.resolve(if b.is_empty() { "HEAD" } else { b })?),
                ))
            }
            [name] => Ok((Revision::Commit(self.resolve(name)?), Revision::Worktree)),
            [a, b] => Ok((
                Revision::Commit(self.resolve(a)?),
                Revision::Commit(self.resolve(b)?),
            )),
            _ => unreachable!(),
        }
    }

    pub fn relative_path(&self, cwd: &Path, path: &Path) -> Result<PathBuf> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            cwd.join(path)
        };
        let mut normalized = PathBuf::new();
        for part in path.components() {
            match part {
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::CurDir => {}
                p => normalized.push(p.as_os_str()),
            }
        }
        let relative = normalized
            .strip_prefix(&self.root)
            .context("main file must be inside the Git repository")?
            .to_owned();
        check_path(&relative)?;
        Ok(relative)
    }

    pub fn snapshot(&self, revision: &Revision, target: &Path) -> Result<()> {
        fs::create_dir_all(target)?;
        match revision {
            Revision::Empty => Ok(()),
            Revision::Worktree => self.copy_worktree(target),
            _ => {
                let entries = match revision {
                    Revision::Index => {
                        parse_entries(&self.run(&["ls-files", "--stage", "-z"])?, true)?
                    }
                    Revision::Commit(id) => parse_entries(
                        &self.run(&["ls-tree", "-r", "-z", "--full-tree", id])?,
                        false,
                    )?,
                    _ => unreachable!(),
                };
                self.copy_blobs(entries, target)
            }
        }
    }

    fn copy_worktree(&self, target: &Path) -> Result<()> {
        // Include non-ignored untracked dependencies referenced by a tracked main.
        let paths = self.run(&[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])?;
        let unique = paths
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(path_from_bytes)
            .collect::<Result<BTreeSet<_>>>()?;
        for path in unique {
            check_path(&path)?;
            let source = self.root.join(&path);
            match fs::symlink_metadata(&source) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).with_context(|| format!("reading {}", source.display())),
                Ok(meta) if meta.is_dir() => bail!(
                    "Git submodule {} is not supported; store its compiled assets in the parent repository",
                    path.display()
                ),
                Ok(_) => {}
            }
            let resolved = fs::canonicalize(&source)
                .with_context(|| format!("resolving {}", source.display()))?;
            ensure!(
                resolved.starts_with(&self.root),
                "{} points outside the repository; snapshots require repository-local dependencies",
                path.display()
            );
            let dest = target.join(&path);
            fs::create_dir_all(dest.parent().unwrap())?;
            fs::copy(source, dest).with_context(|| format!("copying {}", path.display()))?;
        }
        Ok(())
    }

    fn copy_blobs(&self, entries: Vec<Entry>, target: &Path) -> Result<()> {
        let mut child = self
            .command()
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("starting git cat-file")?;
        let result = (|| -> Result<()> {
            let mut input = child.stdin.take().unwrap();
            let mut output = BufReader::new(child.stdout.take().unwrap());
            for entry in entries {
                ensure!(
                    entry.mode != "160000",
                    "Git submodule {} is not supported",
                    entry.path.display()
                );
                check_path(&entry.path)?;
                writeln!(input, "{}", entry.oid)?;
                input.flush()?;
                let mut header = String::new();
                output.read_line(&mut header)?;
                let fields: Vec<_> = header.split_whitespace().collect();
                ensure!(
                    fields.len() == 3 && fields[1] == "blob",
                    "invalid blob response: {}",
                    header.trim()
                );
                let mut data = vec![0; fields[2].parse::<usize>()?];
                output.read_exact(&mut data)?;
                let mut newline = [0];
                output.read_exact(&mut newline)?;
                ensure!(newline[0] == b'\n', "invalid blob terminator");
                let dest = target.join(&entry.path);
                fs::create_dir_all(dest.parent().unwrap())?;
                if entry.mode == "120000" {
                    let link = path_from_bytes(&data)?;
                    ensure!(
                        !link.is_absolute(),
                        "absolute symlink {} cannot be reproduced in a snapshot",
                        entry.path.display()
                    );
                    let mut depth = entry.path.parent().unwrap().components().count();
                    for part in link.components() {
                        match part {
                            Component::ParentDir => {
                                ensure!(
                                    depth > 0,
                                    "symlink {} escapes the repository",
                                    entry.path.display()
                                );
                                depth -= 1;
                            }
                            Component::Normal(_) => depth += 1,
                            Component::CurDir => {}
                            _ => bail!("invalid symlink {}", entry.path.display()),
                        }
                    }

                    #[cfg(unix)]
                    std::os::unix::fs::symlink(link, dest)?;

                    #[cfg(not(unix))]
                    bail!("symlink snapshots require a Unix platform");
                } else {
                    fs::write(&dest, data)?;

                    #[cfg(unix)]
                    if entry.mode == "100755" {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(dest, fs::Permissions::from_mode(0o755))?;
                    }
                }
            }
            drop(input);
            Ok(())
        })();
        if result.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        result?;
        ensure!(status.success(), "git cat-file failed");
        Ok(())
    }
}

fn check_path(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path.components().all(|c| matches!(c, Component::Normal(_))),
        "unsafe repository path: {}",
        path.display()
    );
    Ok(())
}

fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }

    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(std::str::from_utf8(bytes)?))
    }
}

fn parse_entries(bytes: &[u8], index: bool) -> Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for record in bytes.split(|b| *b == 0).filter(|b| !b.is_empty()) {
        let tab = record
            .iter()
            .position(|b| *b == b'\t')
            .context("invalid Git entry")?;
        let fields: Vec<_> = std::str::from_utf8(&record[..tab])?
            .split_whitespace()
            .collect();
        ensure!(fields.len() == 3, "invalid Git metadata");
        if index {
            ensure!(
                fields[2] == "0",
                "the index has unresolved merge conflicts; resolve them before generating a diff"
            );
        }
        entries.push(Entry {
            mode: fields[0].into(),
            oid: fields[if index { 1 } else { 2 }].into(),
            path: path_from_bytes(&record[tab + 1..])?,
        });
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "../tests/unit/git.rs"]
mod tests;
