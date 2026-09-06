//! Wrapper around system `ssh` binary for interactive shells and remote command execution.
use anyhow::Context;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// Build common SSH command-line arguments.
pub fn build_ssh_args(ip: &str, port: u16, key_path: &Path) -> Vec<String> {
    vec![
        "-i".to_string(),
        key_path.to_string_lossy().to_string(),
        "-p".to_string(),
        port.to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        "IdentitiesOnly=yes".to_string(),
        "-o".to_string(),
        "LogLevel=ERROR".to_string(),
        format!("ubuntu@{ip}"),
    ]
}

/// Execute an interactive SSH shell, inheriting stdio.
pub fn exec_interactive_shell(ip: &str, port: u16, key_path: &Path) -> anyhow::Result<ExitStatus> {
    let args = build_ssh_args(ip, port, key_path);
    Command::new("ssh")
        .args(&args)
        .status()
        .context("failed to execute system `ssh`; please ensure OpenSSH is installed")
}

/// Execute a remote command over SSH, inheriting stdio.
pub fn exec_remote_command(
    ip: &str,
    port: u16,
    key_path: &Path,
    command: &str,
) -> anyhow::Result<ExitStatus> {
    let mut args = build_ssh_args(ip, port, key_path);
    args.push(command.to_string());
    Command::new("ssh")
        .args(&args)
        .status()
        .context("failed to execute system `ssh`; please ensure OpenSSH is installed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn builds_expected_ssh_arguments() {
        let key = PathBuf::from("/home/user/.config/qecs/keys/id_qecs");
        let args = build_ssh_args("1.2.3.4", 443, &key);
        assert_eq!(args[0], "-i");
        assert_eq!(args[1], "/home/user/.config/qecs/keys/id_qecs");
        assert_eq!(args[2], "-p");
        assert_eq!(args[3], "443");
        assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        assert_eq!(args.last().unwrap(), "ubuntu@1.2.3.4");
    }
}
