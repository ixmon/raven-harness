use std::path::Path;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};

const EXEC_OUTPUT_TRUNCATE: usize = 8000;

/// True when the command invokes sudo (package installs, privileged ops).
pub fn command_uses_sudo(command: &str) -> bool {
    command
        .split_whitespace()
        .any(|w| w == "sudo" || w.ends_with("/sudo"))
}

/// True for `sudo apt/dnf/yum/pacman/brew install …` style package installs.
pub fn is_privileged_package_install(command: &str) -> bool {
    if !command_uses_sudo(command) {
        return false;
    }
    parse_package_install(command).is_some()
}

/// Force non-interactive sudo so the TUI never gets a password prompt on /dev/tty.
pub fn normalize_sudo_non_interactive(command: &str) -> String {
    if !command_uses_sudo(command) {
        return command.to_string();
    }
    if command.contains("sudo -n") || command.contains("sudo -n ") {
        return command.to_string();
    }
    command.replacen("sudo ", "sudo -n ", 1)
}

fn is_apt_flag(token: &str) -> bool {
    matches!(
        token,
        "-y"
            | "--yes"
            | "-q"
            | "-qq"
            | "-s"
            | "--quiet"
            | "--assume-yes"
            | "--no-install-recommends"
            | "--no-install-suggests"
    ) || token.starts_with("--")
}

/// Extract package names from `apt install`, `dnf install`, etc.
fn parse_package_install(command: &str) -> Option<Vec<String>> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        let is_install = tokens[i] == "install"
            && i > 0
            && matches!(
                tokens[i - 1],
                "apt" | "apt-get" | "dnf" | "yum" | "pacman" | "zypper"
            );
        let is_pacman_sync = tokens[i] == "-S"
            && i > 0
            && tokens[i - 1] == "pacman";
        if is_install || is_pacman_sync {
            i += 1;
            let mut pkgs = Vec::new();
            while i < tokens.len() {
                let t = tokens[i];
                if t == "&&" || t == ";" || t == "|" {
                    break;
                }
                if t == "sudo" || t.contains('/') || is_apt_flag(t) {
                    i += 1;
                    continue;
                }
                pkgs.push(t.to_string());
                i += 1;
            }
            if !pkgs.is_empty() {
                return Some(pkgs);
            }
        }
        i += 1;
    }
    None
}

async fn debian_package_installed(pkg: &str, workspace: &Path) -> bool {
    let cmd = format!(
        "dpkg-query -W -f='${{Status}}' {pkg} 2>/dev/null | grep -q 'install ok installed'"
    );
    let (code, _) = run_shell_command(&cmd, workspace, 15).await;
    code == Some(0)
}

/// Before sudo package installs, check what is already on the system without sudo.
async fn preflight_package_install(
    command: &str,
    workspace: &Path,
) -> Option<(Option<i32>, String)> {
    let packages = parse_package_install(command)?;
    let mut missing = Vec::new();
    let mut installed = Vec::new();
    for pkg in &packages {
        if debian_package_installed(pkg, workspace).await {
            installed.push(pkg.as_str());
        } else {
            missing.push(pkg.clone());
        }
    }

    if missing.is_empty() {
        return Some((
            Some(0),
            format!(
                "✓ All requested packages are already installed ({}). \
                 Skipped sudo install — re-run your build/link probe before asking for sudo again.",
                installed.join(", ")
            ),
        ));
    }

    if !installed.is_empty() {
        return Some((
            None,
            format!(
                "ℹ️ Already installed: {}. Still missing: {}. \
                 Re-run non-sudo probes (pkg-config, dpkg -l, ldconfig -p) to confirm what you need. \
                 If install is required, ask the user to run `apt install {}` in their own terminal — \
                 the TUI cannot enter a sudo password.",
                installed.join(", "),
                missing.join(", "),
                missing.join(" ")
            ),
        ));
    }

    None
}

fn sudo_failure_hint(output: &str) -> Option<String> {
    let lower = output.to_lowercase();
    if lower.contains("a password is required")
        || lower.contains("password is required")
        || lower.contains("no tty present")
        || lower.contains("sorry, try again")
        || lower.contains("sudo: a terminal is required")
    {
        Some(
            "⛔ sudo cannot run interactively in the Raven TUI (no password entry). \
             Ask the user to run the install command in their own terminal, or verify the dependency \
             is already available with non-sudo probes (dpkg -l, pkg-config, which, ldconfig -p)."
                .to_string(),
        )
    } else {
        None
    }
}

async fn run_shell_command(
    command: &str,
    workspace: &Path,
    timeout_secs: u64,
) -> (Option<i32>, String) {
    if command.trim().is_empty() {
        return (None, "⚠️ Empty command".to_string());
    }

    let cmd = command.to_string();
    let ws = workspace.to_path_buf();

    #[cfg(unix)]
    let child = match TokioCommand::new("/bin/bash")
        .arg("-c")
        .arg(&cmd)
        .current_dir(&ws)
        .env("NO_COLOR", "1")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("SUDO_ASKPASS", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (None, format!("(exec spawn error: {e})")),
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
        Err(e) => return (None, format!("(exec spawn error: {e})")),
    };

    let output_res = timeout(
        Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await;

    match output_res {
        Ok(Ok(output)) => {
            let code = output.status.code();
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
            if code != Some(0) {
                trimmed = format!("(exit code {:?})\n{trimmed}", code);
            }
            if trimmed.len() > EXEC_OUTPUT_TRUNCATE {
                let safe = super::safe_truncate(&trimmed, EXEC_OUTPUT_TRUNCATE);
                trimmed = safe.to_string() + "\n... (truncated)";
            }
            (code, trimmed)
        }
        Ok(Err(e)) => (None, format!("(exec wait error: {e})")),
        Err(_) => (
            None,
            format!("(command timed out after {timeout_secs}s — child process killed)"),
        ),
    }
}

/// Run a shell command; returns exit code (if wait succeeded) and combined output.
pub async fn exec_with_exit_code(
    command: &str,
    workspace: &Path,
    timeout_secs: Option<u64>,
) -> (Option<i32>, String) {
    let timeout_secs = timeout_secs.unwrap_or(120);
    if command.trim().is_empty() {
        return (None, "⚠️ Empty command".to_string());
    }

    let dangerous = ["rm -rf /", "mkfs", ":(){ :|:& };:", "dd if=/dev"];
    for d in dangerous {
        if command.contains(d) {
            return (
                None,
                format!("⛔ Refused to run potentially destructive command: {command}"),
            );
        }
    }

    if command_uses_sudo(command) {
        if let Some(short) = preflight_package_install(command, workspace).await {
            return short;
        }
    }

    let cmd = normalize_sudo_non_interactive(command);
    let (code, mut output) = run_shell_command(&cmd, workspace, timeout_secs).await;

    if command_uses_sudo(&cmd) {
        if let Some(hint) = sudo_failure_hint(&output) {
            output = format!("{output}\n\n{hint}");
        }
    }

    (code, output)
}

/// Run a shell command with the given workspace as cwd.
/// Returns combined stdout + stderr, truncated for sanity.
pub async fn exec(command: &str, workspace: &Path, timeout_secs: Option<u64>) -> String {
    exec_with_exit_code(command, workspace, timeout_secs)
        .await
        .1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sudo_commands() {
        assert!(command_uses_sudo("sudo apt install libsdl2-dev"));
        assert!(!command_uses_sudo("apt install libsdl2-dev"));
    }

    #[test]
    fn normalizes_sudo_to_non_interactive() {
        assert_eq!(
            normalize_sudo_non_interactive("sudo apt install -y foo"),
            "sudo -n apt install -y foo"
        );
        assert_eq!(
            normalize_sudo_non_interactive("sudo -n apt install foo"),
            "sudo -n apt install foo"
        );
    }

    #[test]
    fn parses_apt_install_packages() {
        let pkgs = parse_package_install("sudo apt-get install -y libsdl2-dev pkg-config").unwrap();
        assert_eq!(pkgs, vec!["libsdl2-dev", "pkg-config"]);
    }

    #[test]
    fn detects_privileged_package_install() {
        assert!(is_privileged_package_install("sudo apt install foo"));
        assert!(!is_privileged_package_install("sudo systemctl restart foo"));
    }

    #[test]
    fn sudo_failure_hint_matches_password_errors() {
        assert!(sudo_failure_hint("sudo: a password is required").is_some());
        assert!(sudo_failure_hint("all good").is_none());
    }
}