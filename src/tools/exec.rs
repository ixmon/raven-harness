use std::path::Path;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};

/// Run a shell command with the given workspace as cwd.
/// Returns combined stdout + stderr, truncated for sanity.
pub async fn exec(command: &str, workspace: &Path) -> String {
    const EXEC_OUTPUT_TRUNCATE: usize = 8000;
    const EXEC_TIMEOUT_SECS: u64 = 60;
    if command.trim().is_empty() {
        return "⚠️ Empty command".to_string();
    }

    // Basic safety: block a few obviously dangerous patterns
    let dangerous = ["rm -rf /", "mkfs", ":(){ :|:& };:", "dd if=/dev"];
    for d in dangerous {
        if command.contains(d) {
            return format!(
                "⛔ Refused to run potentially destructive command: {}",
                command
            );
        }
    }

    let cmd = command.to_string();
    let ws = workspace.to_path_buf();

    // Use tokio::process + explicit kill for proper child cleanup on timeout (glm.md priority)
    #[cfg(unix)]
    let child = match TokioCommand::new("/bin/bash")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&ws)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("(exec spawn error: {})", e),
    };

    #[cfg(windows)]
    let child = match TokioCommand::new("cmd")
        .arg("/C")
        .arg(&cmd)
        .current_dir(&ws)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return format!("(exec spawn error: {})", e),
    };

    let output_res = timeout(
        Duration::from_secs(EXEC_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await;

    match output_res {
        Ok(Ok(output)) => {
            let mut result = String::new();
            result.push_str(&String::from_utf8_lossy(&output.stdout));
            if !output.stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&String::from_utf8_lossy(&output.stderr));
            }
            let mut trimmed = result.trim().to_string();
            if trimmed.is_empty() {
                trimmed = "(no output)".to_string();
            }
            if trimmed.len() > EXEC_OUTPUT_TRUNCATE {
                trimmed.truncate(EXEC_OUTPUT_TRUNCATE);
                trimmed.push_str("\n... (truncated)");
            }
            trimmed
        }
        Ok(Err(e)) => format!("(exec wait error: {})", e),
        Err(_) => {
            // Timeout: the wait future was dropped by the timeout.
            // kill_on_drop(true) makes the Child kill itself on drop.
            // (We cannot access `child` here because it was moved into the future.)
            "(command timed out after 60s — child process killed)".to_string()
        }
    }
}
