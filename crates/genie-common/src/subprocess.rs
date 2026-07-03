//! Timeout-guarded subprocess execution, shared by every crate that shells
//! out to an external binary (`pdftotext`, `piper`, `whisper-cli`, `aplay`,
//! `ffmpeg`, `sox`, `arecord`, `curl`, `systemctl`, `tegrastats`, `df`, …).
//!
//! Every caller of this module used to spawn its own `Command` and `.await`
//! its exit with no deadline at all — a hung child (a stuck `pdftotext` on
//! a pathological PDF, a wedged `piper`/`whisper-cli`, an unresponsive
//! `systemctl`) left the calling task blocked forever, with the worst case
//! being a `pdftotext` call in genie-core's own startup path that, if hung,
//! prevents the HTTP server and voice loop from ever starting.
//!
//! Every command run through this module is spawned with `kill_on_drop`,
//! so a timeout (which drops the in-flight wait future) reaps the child
//! process instead of leaking an orphan that keeps running in the
//! background after the caller has already moved on.

use std::process::{ExitStatus, Output};
use std::time::Duration;

use tokio::process::{Child, Command};

/// Why a timeout-guarded subprocess call failed.
#[derive(Debug)]
pub enum SubprocessError {
    /// The command did not exit within the given deadline. The child has
    /// already been killed (via `kill_on_drop`) by the time this is
    /// returned.
    Timeout(Duration),
    /// Spawning the command, or reading/waiting on it, failed at the OS
    /// level (e.g. the binary isn't installed).
    Io(std::io::Error),
}

impl std::fmt::Display for SubprocessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubprocessError::Timeout(d) => write!(f, "subprocess timed out after {d:?}"),
            SubprocessError::Io(e) => write!(f, "subprocess io error: {e}"),
        }
    }
}

impl std::error::Error for SubprocessError {}

impl From<std::io::Error> for SubprocessError {
    fn from(e: std::io::Error) -> Self {
        SubprocessError::Io(e)
    }
}

/// Run `command` to completion, capturing stdout/stderr, bounded by
/// `timeout`. The common case: a one-shot call with no need to interact
/// with the child while it runs (`pdftotext`, `sox`, `arecord`, `curl`,
/// `systemctl`, `df`, …).
pub async fn run_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<Output, SubprocessError> {
    command.kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(SubprocessError::Timeout(timeout)),
    }
}

/// Run `command` to completion without capturing stdout/stderr (they're
/// inherited from the parent), bounded by `timeout`. For callers that only
/// care about the exit status (e.g. `systemctl is-active --quiet`).
pub async fn status_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> Result<ExitStatus, SubprocessError> {
    command.kill_on_drop(true);
    match tokio::time::timeout(timeout, command.status()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(SubprocessError::Timeout(timeout)),
    }
}

/// Spawn `command` with `kill_on_drop` set, for callers that need to write
/// to stdin or otherwise interact with the child before waiting on it
/// (e.g. piping text into `piper`'s stdin before waiting for synthesized
/// audio on stdout).
pub fn spawn_killable(command: &mut Command) -> std::io::Result<Child> {
    command.kill_on_drop(true).spawn()
}

/// Wait for an already-spawned child to exit, bounded by `timeout`,
/// without collecting its output (the caller already consumed stdout/
/// stderr, or doesn't care about them — e.g. waiting on `aplay` playback).
pub async fn wait_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> Result<ExitStatus, SubprocessError> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(SubprocessError::Timeout(timeout)),
    }
}

/// Wait for an already-spawned child to exit, collecting its output,
/// bounded by `timeout`. Mirrors `Child::wait_with_output`, but a timeout
/// kills the child (via `kill_on_drop`, set at spawn time by
/// [`spawn_killable`]) instead of awaiting forever.
pub async fn wait_with_output_and_timeout(
    child: Child,
    timeout: Duration,
) -> Result<Output, SubprocessError> {
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(SubprocessError::Timeout(timeout)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_with_timeout_succeeds_for_a_fast_command() {
        let mut cmd = Command::new("true");
        let status = status_with_timeout(&mut cmd, Duration::from_secs(5))
            .await
            .expect("`true` must succeed");
        assert!(status.success());
    }

    #[tokio::test]
    async fn status_with_timeout_kills_a_hung_command_and_returns_timeout() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let start = std::time::Instant::now();
        let err = status_with_timeout(&mut cmd, Duration::from_millis(100))
            .await
            .expect_err("a 60s sleep must time out against a 100ms deadline");
        assert!(matches!(err, SubprocessError::Timeout(_)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn run_with_timeout_succeeds_for_a_fast_command() {
        let mut cmd = Command::new("true");
        let output = run_with_timeout(&mut cmd, Duration::from_secs(5))
            .await
            .expect("`true` must succeed");
        assert!(output.status.success());
    }

    #[tokio::test]
    async fn run_with_timeout_kills_a_hung_command_and_returns_timeout() {
        // `sleep 60` never exits within our short deadline.
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let start = std::time::Instant::now();
        let err = run_with_timeout(&mut cmd, Duration::from_millis(100))
            .await
            .expect_err("a 60s sleep must time out against a 100ms deadline");
        assert!(matches!(err, SubprocessError::Timeout(_)));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not wait anywhere near the full 60s sleep"
        );
    }

    #[tokio::test]
    async fn run_with_timeout_surfaces_missing_binary_as_io_error() {
        let mut cmd = Command::new("definitely-not-a-real-binary-xyz");
        let err = run_with_timeout(&mut cmd, Duration::from_secs(5))
            .await
            .expect_err("a missing binary must fail");
        assert!(matches!(err, SubprocessError::Io(_)));
    }

    #[tokio::test]
    async fn wait_with_timeout_kills_a_hung_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let mut child = spawn_killable(&mut cmd).expect("spawn must succeed");
        let start = std::time::Instant::now();
        let err = wait_with_timeout(&mut child, Duration::from_millis(100))
            .await
            .expect_err("a 60s sleep must time out");
        assert!(matches!(err, SubprocessError::Timeout(_)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn wait_with_output_and_timeout_kills_a_hung_child() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let child = spawn_killable(&mut cmd).expect("spawn must succeed");
        let start = std::time::Instant::now();
        let err = wait_with_output_and_timeout(child, Duration::from_millis(100))
            .await
            .expect_err("a 60s sleep must time out");
        assert!(matches!(err, SubprocessError::Timeout(_)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn wait_with_output_and_timeout_succeeds_for_a_fast_child() {
        let mut cmd = Command::new("echo");
        cmd.arg("hello").stdout(std::process::Stdio::piped());
        let child = spawn_killable(&mut cmd).expect("spawn must succeed");
        let output = wait_with_output_and_timeout(child, Duration::from_secs(5))
            .await
            .expect("echo must succeed");
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }
}
