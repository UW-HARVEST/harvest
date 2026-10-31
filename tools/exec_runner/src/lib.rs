use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

/// Runs a binary with the given arguments and optional stdin, enforcing a timeout.
/// Returns the full `Output` (stdout, stderr, exit status) on success.
pub fn run_with_timeout(
    binary: &Path,
    argv: &[String],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = spawn(binary, argv)?;
    write_stdin(&mut child, stdin, binary)?;
    finish(child, timeout)
}

fn spawn(binary: &Path, argv: &[String]) -> Result<Child, Box<dyn std::error::Error>> {
    Command::new(binary)
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("exec_runner: failed to spawn {}: {e}", binary.display()).into())
}

fn write_stdin(
    child: &mut Child,
    data: Option<&str>,
    binary: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(data) = data else {
        drop(child.stdin.take());
        return Ok(());
    };
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("exec_runner: could not open stdin for {}", binary.display()))?;
    stdin
        .write_all(data.as_bytes())
        .map_err(|e| format!("exec_runner: stdin write failed: {e}").into())
}

fn finish(mut child: Child, timeout: Duration) -> Result<Output, Box<dyn std::error::Error>> {
    match child.wait_timeout(timeout)? {
        Some(_) => child
            .wait_with_output()
            .map_err(|e| format!("exec_runner: failed to read output: {e}").into()),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("exec_runner: timed out after {}s", timeout.as_secs()).into())
        }
    }
}
