//! Subprocess helper used by every collector and runner.
//!
//! The Flask app called `subprocess.run(..., capture_output=True, timeout=N)`
//! in a dozen places; this is the same contract: capture both streams, never
//! inherit stdin, return the exit code, and turn a timeout into rc 124 (the
//! shell convention) instead of an exception.
//!
//! Pipes are drained by two reader threads, so a child producing more than a
//! pipe buffer cannot deadlock while the parent polls `try_wait`.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    pub rc: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn ok(&self) -> bool {
        self.rc == 0
    }

    /// stdout followed by stderr — exactly how the Python collectors combined
    /// smartctl output before parsing it.
    pub fn combined(&self) -> String {
        let mut text = self.stdout.clone();
        text.push_str(&self.stderr);
        text
    }

    pub fn trimmed(&self) -> String {
        self.combined().trim().to_string()
    }
}

/// Run a command. `timeout` of zero means "no timeout".
pub fn run(
    argv: &[&str],
    cwd: Option<&Path>,
    env: Option<&[(String, String)]>,
    timeout: Duration,
) -> CmdOutput {
    let Some((program, args)) = argv.split_first() else {
        return CmdOutput {
            rc: 2,
            stdout: String::new(),
            stderr: "empty command".into(),
        };
    };

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    if let Some(pairs) = env {
        for (key, value) in pairs {
            command.env(key, value);
        }
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return CmdOutput {
                rc: 127,
                stdout: String::new(),
                stderr: format!("failed to spawn {}: {e}", program),
            }
        }
    };

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let stdout_reader = thread::spawn(move || read_to_string(stdout));
    let stderr_reader = thread::spawn(move || read_to_string(stderr));

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if !timeout.is_zero() && Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = child.kill();
                return CmdOutput {
                    rc: 1,
                    stdout: String::new(),
                    stderr: format!("wait failed: {e}"),
                };
            }
        }
    }

    let status = child.wait().ok();
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    let rc = if timed_out {
        124
    } else {
        status.and_then(|s| s.code()).unwrap_or(-1)
    };

    CmdOutput { rc, stdout, stderr }
}

pub fn run_simple(argv: &[&str]) -> CmdOutput {
    run(argv, None, None, Duration::from_secs(30))
}

fn read_to_string(mut reader: impl Read) -> String {
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Write a line to a log file, creating parent directories if needed.
pub fn log_write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_stdout_and_exit_code() {
        let out = run_simple(&["sh", "-c", "printf hello; exit 3"]);
        assert_eq!(out.rc, 3);
        assert_eq!(out.stdout, "hello");
        assert!(out.stderr.is_empty());
        assert!(!out.ok());
    }

    #[test]
    fn captures_stderr() {
        let out = run_simple(&["sh", "-c", "printf oops >&2; exit 1"]);
        assert_eq!(out.stderr, "oops");
        assert_eq!(out.combined(), "oops");
    }

    #[test]
    fn timeout_becomes_rc_124() {
        let started = Instant::now();
        let out = run(&["sleep", "5"], None, None, Duration::from_millis(150));
        assert_eq!(out.rc, 124);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn missing_binary_is_reported() {
        let out = run_simple(&["definitely-not-a-binary-xyz"]);
        assert_eq!(out.rc, 127);
        assert!(out.stderr.contains("failed to spawn"));
    }

    #[test]
    fn large_output_does_not_deadlock() {
        // 200 KB is well past the 64 KB pipe buffer.
        let out = run_simple(&["sh", "-c", "yes x | head -c 200000"]);
        assert_eq!(out.rc, 0);
        assert_eq!(out.stdout.len(), 200_000);
    }

    #[test]
    fn env_and_cwd_are_applied() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            &["sh", "-c", "printf %s \"$SC_TEST\"; pwd"],
            Some(dir.path()),
            Some(&[("SC_TEST".to_string(), "value".to_string())]),
            Duration::from_secs(5),
        );
        assert_eq!(out.rc, 0);
        assert!(out.stdout.starts_with("value"));
        assert!(out.stdout.contains(dir.path().to_str().unwrap()));
    }
}
