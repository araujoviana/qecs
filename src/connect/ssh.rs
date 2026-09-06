//! Wrapper around system `ssh` binary for interactive shells and remote command execution.
use anyhow::Context;
use std::path::Path;
use std::process::{Command, ExitStatus};

/// Build common SSH command-line arguments.
pub fn build_ssh_args(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
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
    ];

    if let Some(proxy) = proxy_command {
        args.push("-o".to_string());
        args.push(format!("ProxyCommand={proxy}"));
    }

    args.push(format!("ubuntu@{ip}"));
    args
}

/// Build a configured `std::process::Command` ready to execute or customize.
pub fn build_ssh_command(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
) -> Command {
    let args = build_ssh_args(ip, port, key_path, proxy_command);
    let mut cmd = Command::new("ssh");
    cmd.args(&args);
    cmd
}

/// Execute an interactive SSH shell, inheriting stdio.
pub fn exec_interactive_shell(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
) -> anyhow::Result<ExitStatus> {
    build_ssh_command(ip, port, key_path, proxy_command)
        .status()
        .context("failed to execute system `ssh`; please ensure OpenSSH is installed")
}

/// Execute a remote command over SSH, inheriting stdio.
pub fn exec_remote_command(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    command: &str,
) -> anyhow::Result<ExitStatus> {
    build_ssh_command(ip, port, key_path, proxy_command)
        .arg(command)
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
        let args = build_ssh_args("1.2.3.4", 443, &key, None);
        assert_eq!(args[0], "-i");
        assert_eq!(args[1], "/home/user/.config/qecs/keys/id_qecs");
        assert_eq!(args[2], "-p");
        assert_eq!(args[3], "443");
        assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        assert_eq!(args.last().unwrap(), "ubuntu@1.2.3.4");
    }

    #[test]
    fn builds_ssh_arguments_with_proxy_command() {
        let key = PathBuf::from("/home/user/.config/qecs/keys/id_qecs");
        let args = build_ssh_args("1.2.3.4", 22, &key, Some("nc -X 5 -x 127.0.0.1:1080 %h %p"));
        assert!(args.contains(&"ProxyCommand=nc -X 5 -x 127.0.0.1:1080 %h %p".to_string()));
        assert_eq!(args.last().unwrap(), "ubuntu@1.2.3.4");
    }
}
