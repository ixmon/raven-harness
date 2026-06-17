use std::path::Path;
use std::process::Command;
use tokio::time::{timeout, Duration};

/// Run a shell command with the given workspace as cwd.
/// Returns combined stdout + stderr, truncated for sanity.
pub async fn exec(command: &str, workspace: &Path) -> String {
    if command.trim().is_empty() {
        return "⚠️ Empty command".to_string();
    }

    // Basic safety: block a few obviously dangerous patterns
    let dangerous = ["rm -rf /", "mkfs", ":(){ :|:& };:", "dd if=/dev"];
    for d in dangerous {
        if command.contains(d) {
            return format!("⛔ Refused to run potentially destructive command: {}", command);
        }
    }

    let cmd = command.to_string();
    let ws = workspace.to_path_buf();

    let fut = tokio::task::spawn_blocking(move || {
        let output = Command::new("/bin/bash")
            .arg("-c")
            .arg(&cmd)
            .current_dir(&ws)
            .env("NO_COLOR", "1")
            .output();

        match output {
            Ok(o) => {
                let mut result = String::new();
                result.push_str(&String::from_utf8_lossy(&o.stdout));
                if !o.stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str(&String::from_utf8_lossy(&o.stderr));
                }
                result
            }
            Err(e) => format!("(exec error: {})", e),
        }
    });

    match timeout(Duration::from_secs(60), fut).await {
        Ok(Ok(out)) => {
            let mut trimmed = out.trim().to_string();
            if trimmed.is_empty() {
                trimmed = "(no output)".to_string();
            }
            if trimmed.len() > 8000 {
                trimmed.truncate(8000);
                trimmed.push_str("\n... (truncated)");
            }
            trimmed
        }
        Ok(Err(e)) => format!("(task join error: {})", e),
        Err(_) => "(command timed out after 60s)".to_string(),
    }
}
