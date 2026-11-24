//! Runs the `gh` CLI. Never goes through a shell.

use std::fmt;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::thread;

#[derive(Debug, Clone, PartialEq)]
pub struct GhError(pub String);

impl fmt::Display for GhError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub type Result<T> = std::result::Result<T, GhError>;

/// How much stdout to keep in memory.
#[derive(Debug, Clone, Copy)]
pub enum Keep {
    /// Fail when output is larger than this (used for JSON).
    All(usize),
    /// Keep the beginning (diffs).
    Head(usize),
    /// Keep the end (logs).
    Tail(usize),
}

pub const JSON: Keep = Keep::All(16 << 20);
pub const DIFF: Keep = Keep::Head(2 << 20);
pub const LOG: Keep = Keep::Tail(2 << 20);

/// A fully built `gh` invocation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cmd {
    pub label: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub guard: Option<Guard>,
    /// The open document shows the change, so load it again.
    pub reload: bool,
}

/// Text that must still be on GitHub, so an edit never overwrites a newer one.
#[derive(Debug, Clone, PartialEq)]
pub struct Guard {
    /// Prints the current text as a JSON string.
    pub args: Vec<String>,
    pub expected: String,
}

/// Run a mutating command once its guard, if any, holds.
pub fn execute(cmd: &Cmd) -> Result<String> {
    if let Some(guard) = &cmd.guard {
        let now: String = serde_json::from_str(&run(&guard.args, None, JSON)?)
            .map_err(|e| GhError(format!("unexpected gh response: {e}")))?;
        if now != guard.expected {
            return Err(GhError(
                "changed on GitHub meanwhile; nothing was saved. Copy your text and open it again"
                    .into(),
            ));
        }
    }
    run(&cmd.args, cmd.stdin.as_deref(), JSON)
}

pub fn run(args: &[String], stdin: Option<&str>, keep: Keep) -> Result<String> {
    let mut child = Command::new("gh")
        .args(args)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_SPINNER_DISABLED", "1")
        .env("NO_COLOR", "1")
        .env_remove("GH_FORCE_TTY")
        .env_remove("CLICOLOR_FORCE")
        .env_remove("GH_REPO")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => {
                GhError("GitHub CLI `gh` not found; install it from https://cli.github.com".into())
            }
            _ => GhError(format!("cannot start gh: {e}")),
        })?;

    let mut out = child
        .stdout
        .take()
        .ok_or_else(|| GhError("gh stdout missing".into()))?;
    let err = child
        .stderr
        .take()
        .ok_or_else(|| GhError("gh stderr missing".into()))?;
    let errors = thread::spawn(move || {
        let mut err = err;
        let mut kept = Vec::new();
        // Keep 16 KiB, then drain the rest so gh never blocks on a full pipe.
        let read = err
            .by_ref()
            .take(16 << 10)
            .read_to_end(&mut kept)
            .and_then(|_| io::copy(&mut err, &mut io::sink()));
        read.map(|_| kept)
    });

    let write = match (stdin, child.stdin.take()) {
        (Some(text), Some(mut pipe)) => pipe.write_all(text.as_bytes()),
        _ => Ok(()),
    };
    let output = collect(&mut out, keep);
    drop(out); // gh gets SIGPIPE if we stopped reading early
    let status = child
        .wait()
        .map_err(|e| GhError(format!("waiting for gh: {e}")))?;
    let stderr = errors
        .join()
        .map_err(|_| GhError("stderr reader panicked".into()))?
        .map_err(|e| GhError(format!("reading gh stderr: {e}")))?;

    let (bytes, truncated) = output.map_err(|e| GhError(format!("reading gh output: {e}")))?;
    let cut_short = truncated && matches!(keep, Keep::Head(_));
    if !status.success() && !cut_short {
        let message = String::from_utf8_lossy(&stderr).trim().to_string();
        return Err(GhError(if message.is_empty() {
            format!("gh failed with {status}")
        } else {
            message
        }));
    }
    write.map_err(|e| GhError(format!("sending input to gh: {e}")))?;

    let text = String::from_utf8_lossy(&bytes).into_owned();
    match (truncated, keep) {
        (true, Keep::All(n)) => Err(GhError(format!("gh returned more than {n} bytes"))),
        (true, Keep::Head(n)) => Ok(format!("{text}\n… truncated after {} MiB", n >> 20)),
        (true, Keep::Tail(n)) => Ok(format!("… showing the last {} MiB\n{text}", n >> 20)),
        (false, _) => Ok(text),
    }
}

fn collect(from: &mut impl Read, keep: Keep) -> io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    match keep {
        Keep::All(n) | Keep::Head(n) => {
            from.by_ref().take(n as u64 + 1).read_to_end(&mut kept)?;
            let truncated = kept.len() > n;
            kept.truncate(n);
            Ok((kept, truncated))
        }
        Keep::Tail(n) => {
            let mut truncated = false;
            let mut chunk = [0u8; 16 << 10];
            loop {
                let len = from.read(&mut chunk)?;
                if len == 0 {
                    break;
                }
                kept.extend_from_slice(&chunk[..len]);
                if kept.len() > n * 2 {
                    kept.drain(..kept.len() - n);
                    truncated = true;
                }
            }
            if kept.len() > n {
                kept.drain(..kept.len() - n);
                truncated = true;
            }
            Ok((kept, truncated))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_head_and_tail() {
        let data: Vec<u8> = (0..100).collect();
        assert_eq!(
            collect(&mut &data[..], Keep::Head(3)).unwrap(),
            (vec![0, 1, 2], true)
        );
        assert_eq!(
            collect(&mut &data[..], Keep::Tail(2)).unwrap(),
            (vec![98, 99], true)
        );
        assert_eq!(
            collect(&mut &data[..], Keep::All(100)).unwrap(),
            (data, false)
        );
    }
}
