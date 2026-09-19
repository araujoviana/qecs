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
    build_ssh_args_ext(ip, port, key_path, proxy_command, None)
}

/// Build common SSH command-line arguments with optional PTY allocation.
pub fn build_ssh_args_ext(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    pty: Option<bool>,
) -> Vec<String> {
    build_ssh_args_full(ip, port, key_path, proxy_command, pty, None)
}

/// Build common SSH command-line arguments with optional PTY and ControlMaster socket.
pub fn build_ssh_args_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    pty: Option<bool>,
    control_socket: Option<&Path>,
) -> Vec<String> {
    // Host-key verification stays on (trust-on-first-use), but in a qecs-owned
    // known_hosts file so ephemeral VMs churning a recycled elastic-IP pool
    // never wedge the user's ~/.ssh/known_hosts. BatchMode keeps a failed key
    // auth from blocking a non-interactive `qecs run` on a password prompt; the
    // keepalives stop a long silent attached job from being dropped by NAT.
    let known_hosts = crate::keys::qecs_known_hosts_path();
    let mut args = vec![
        "-i".to_string(),
        key_path.to_string_lossy().to_string(),
        "-p".to_string(),
        port.to_string(),
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        format!("UserKnownHostsFile={}", known_hosts.to_string_lossy()),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        "ConnectTimeout=10".to_string(),
        "-o".to_string(),
        "ServerAliveInterval=15".to_string(),
        "-o".to_string(),
        "ServerAliveCountMax=8".to_string(),
        "-o".to_string(),
        "IdentitiesOnly=yes".to_string(),
        "-o".to_string(),
        "LogLevel=ERROR".to_string(),
    ];

    if let Some(enable_pty) = pty {
        if enable_pty {
            args.insert(0, "-t".to_string());
        } else {
            args.insert(0, "-T".to_string());
        }
    }

    if let Some(sock) = control_socket {
        args.push("-o".to_string());
        args.push("ControlMaster=auto".to_string());
        args.push("-o".to_string());
        args.push(format!("ControlPath={}", sock.display()));
        args.push("-o".to_string());
        args.push("ControlPersist=180".to_string());
    }

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
    build_ssh_command_ext(ip, port, key_path, proxy_command, None)
}

/// Build a configured `std::process::Command` ready to execute or customize, with optional PTY.
pub fn build_ssh_command_ext(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    pty: Option<bool>,
) -> Command {
    build_ssh_command_full(ip, port, key_path, proxy_command, pty, None)
}

/// Build a configured `std::process::Command` with optional PTY and ControlMaster socket.
pub fn build_ssh_command_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    pty: Option<bool>,
    control_socket: Option<&Path>,
) -> Command {
    let args = build_ssh_args_full(ip, port, key_path, proxy_command, pty, control_socket);
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
        assert_eq!(args.last().unwrap(), "ubuntu@1.2.3.4");
    }

    #[test]
    fn ssh_args_are_hardened_for_ephemeral_non_interactive_use() {
        let key = PathBuf::from("/k");
        let args = build_ssh_args("1.2.3.4", 22, &key, None).join(" ");
        // never block a script on a password prompt
        assert!(args.contains("BatchMode=yes"));
        // keep trust-on-first-use host-key verification, but in a qecs-owned
        // known_hosts file so recycled elastic IPs never wedge ~/.ssh/known_hosts
        assert!(args.contains("StrictHostKeyChecking=accept-new"));
        assert!(args.contains("UserKnownHostsFile="));
        assert!(!args.contains("UserKnownHostsFile=/dev/null"));
        assert!(args.contains("qecs/known_hosts"));
        // don't hang forever on a black-holed port; keep long attached jobs alive
        assert!(args.contains("ConnectTimeout="));
        assert!(args.contains("ServerAliveInterval="));
    }

    #[test]
    fn builds_ssh_arguments_with_proxy_command() {
        let key = PathBuf::from("/home/user/.config/qecs/keys/id_qecs");
        let args = build_ssh_args("1.2.3.4", 22, &key, Some("nc -X 5 -x 127.0.0.1:1080 %h %p"));
        assert!(args.contains(&"ProxyCommand=nc -X 5 -x 127.0.0.1:1080 %h %p".to_string()));
        assert_eq!(args.last().unwrap(), "ubuntu@1.2.3.4");
    }

    #[test]
    fn builds_ssh_arguments_with_pty_options() {
        let key = PathBuf::from("/k");
        let args_pty = build_ssh_args_ext("1.2.3.4", 22, &key, None, Some(true));
        assert_eq!(args_pty[0], "-t");

        let args_no_pty = build_ssh_args_ext("1.2.3.4", 22, &key, None, Some(false));
        assert_eq!(args_no_pty[0], "-T");

        let args_default = build_ssh_args_ext("1.2.3.4", 22, &key, None, None);
        assert_ne!(args_default[0], "-t");
        assert_ne!(args_default[0], "-T");
    }

    #[test]
    fn ssh_never_injects_automatic_x11_forwarding_even_with_display() {
        let key = PathBuf::from("/k");
        unsafe {
            std::env::set_var("DISPLAY", ":0");
        }
        let args = build_ssh_args_ext("1.2.3.4", 22, &key, None, Some(true));
        assert!(!args.contains(&"-Y".to_string()));
        assert!(!args.contains(&"ForwardX11Trusted=yes".to_string()));
    }
}
