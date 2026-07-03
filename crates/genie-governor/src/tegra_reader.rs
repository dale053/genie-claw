use std::time::Duration;

use genie_common::subprocess;
use genie_common::tegrastats::{self, TegraSnapshot};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

/// Deadline for reaping the `tegrastats` child after its stdout stream
/// ends. By that point the process is expected to already be exiting; this
/// only guards against it somehow lingering instead of a genuine hang.
const TEGRASTATS_REAP_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawn `tegrastats` as a child process, parse each line, and broadcast
/// the latest snapshot via a watch channel.
///
/// Returns a receiver that always holds the most recent snapshot.
/// The sender is moved into the spawned task.
///
/// On non-Jetson systems (dev), returns None and logs a warning.
pub async fn spawn(interval_ms: u64) -> Option<watch::Receiver<TegraSnapshot>> {
    let mut command = Command::new("tegrastats");
    command
        .arg("--interval")
        .arg(interval_ms.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let result = subprocess::spawn_killable(&mut command);

    let mut child = match result {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "tegrastats not available (not running on Jetson?)");
            return None;
        }
    };

    let stdout = child.stdout.take()?;
    let reader = BufReader::new(stdout);

    // Initial dummy snapshot.
    let initial = TegraSnapshot {
        timestamp_ms: 0,
        ram_used_mb: 0,
        ram_total_mb: 8192,
        swap_used_mb: 0,
        swap_total_mb: 0,
        gpu_freq_pct: 0,
        cpu_loads: vec![],
        gpu_temp_c: None,
        cpu_temp_c: None,
        power_mw: None,
    };

    let (tx, rx) = watch::channel(initial);

    tokio::spawn(async move {
        let mut lines = reader.lines();
        let mut parse_errors: u32 = 0;

        while let Ok(Some(line)) = lines.next_line().await {
            let ts = crate::store::now_ms();
            match tegrastats::parse_line(&line, ts) {
                Ok(snap) => {
                    parse_errors = 0;
                    if tx.send(snap).is_err() {
                        break; // All receivers dropped.
                    }
                }
                Err(e) => {
                    parse_errors += 1;
                    if parse_errors <= 3 {
                        tracing::warn!(error = %e, "tegrastats parse error");
                    }
                }
            }
        }

        // If tegrastats exits, try to reap the child.
        let _ = subprocess::wait_with_timeout(&mut child, TEGRASTATS_REAP_TIMEOUT).await;
        tracing::warn!("tegrastats process exited");
    });

    Some(rx)
}
