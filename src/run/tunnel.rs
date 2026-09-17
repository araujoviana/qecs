//! Zero-config web tunnels and OpenSSH ControlMaster management.
//!
//! Automatically detects web services listening on remote TCP ports
//! (e.g. Gradio, Streamlit, Jupyter, FastAPI) via `/proc/net/tcp` inspection,
//! and dynamically forwards them to `http://localhost:<port>` on the user's laptop.

use std::collections::HashSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;
use colored::Colorize;

/// System or infrastructure ports to exclude from auto-forwarding.
pub const SYSTEM_PORTS: &[u16] = &[
    22,   // SSH daemon
    53,   // DNS (systemd-resolved)
    68,   // DHCP client
    111,  // rpcbind
    123,  // NTP
    323,  // Chrony NTP
    853,  // DNS over TLS
    5353, // mDNS
    5355, // LLMNR
];

/// Identify well-known machine learning and web framework ports.
pub fn guess_service_name(port: u16) -> Option<&'static str> {
    match port {
        7860 => Some("Gradio"),
        8501 => Some("Streamlit"),
        8888 => Some("Jupyter"),
        8000 => Some("HTTP / FastAPI"),
        8080 => Some("HTTP"),
        5000 => Some("Flask"),
        3000 => Some("Node / React"),
        5173 => Some("Vite"),
        6006 => Some("TensorBoard"),
        8051 => Some("Dash"),
        8787 => Some("Dask Dashboard"),
        9090 => Some("Prometheus"),
        _ => None,
    }
}

/// Check if a port belongs to system/infrastructure services.
pub fn is_system_port(port: u16) -> bool {
    port < 1024 || SYSTEM_PORTS.contains(&port)
}

/// Parse listening TCP ports from Linux `/proc/net/tcp` or `/proc/net/tcp6` file contents.
///
/// In `/proc/net/tcp`, column 1 (`local_address`) contains `<hex_ip>:<hex_port>`
/// and column 3 (`st`) contains the connection state where `0A` represents `TCP_LISTEN`.
pub fn parse_proc_net_tcp(content: &str) -> Vec<u16> {
    let mut ports = HashSet::new();

    for line in content.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Need at least: sl, local_address, rem_address, st
        if parts.len() < 4 {
            continue;
        }

        let local_addr = parts[1];
        let state = parts[3];

        // 0A is TCP_LISTEN
        if state != "0A" {
            continue;
        }

        if let Some((_ip_hex, port_hex)) = local_addr.split_once(':')
            && let Ok(port) = u16::from_str_radix(port_hex, 16)
            && !is_system_port(port)
        {
            ports.insert(port);
        }
    }

    let mut result: Vec<u16> = ports.into_iter().collect();
    result.sort_unstable();
    result
}

/// Test whether a local TCP port is free to bind on 127.0.0.1.
pub fn is_local_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Find an available local port, trying `preferred` first, then scanning upward,
/// and finally falling back to an OS-assigned ephemeral port.
pub fn find_available_local_port(preferred: u16) -> Option<u16> {
    if is_local_port_available(preferred) {
        return Some(preferred);
    }

    for p in (preferred + 1)..=(preferred + 100) {
        if is_local_port_available(p) {
            return Some(p);
        }
    }

    // Fall back to OS-assigned ephemeral port
    TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok().map(|a| a.port()))
}

/// Format a rich terminal banner announcing a newly forwarded web tunnel.
pub fn format_tunnel_banner(
    remote_port: u16,
    local_port: u16,
    service_hint: Option<&str>,
) -> String {
    let label = if let Some(svc) = service_hint {
        format!("Web UI detected ({svc})")
    } else {
        "Web service detected".to_string()
    };

    let url = format!("http://localhost:{local_port}");
    let remote_info = if local_port == remote_port {
        format!("remote port {remote_port}")
    } else {
        format!("remote port {remote_port} -> local {local_port}")
    };

    let top = "  ┌────────────────────────────────────────────────────────────┐".green();
    let bottom = "  └────────────────────────────────────────────────────────────┘".green();
    let line1 = format!(
        "  │  {} {}: {}  │",
        "➜".cyan().bold(),
        label.bold(),
        url.cyan().bold().underline()
    );
    let line2 =
        format!("  │    Forwarded from {remote_info} via encrypted SSH tunnel     │").dimmed();

    format!("\n{top}\n{line1}\n{line2}\n{bottom}\n")
}

/// OpenSSH ControlMaster session manager for multiplexing dynamic port forwards.
#[derive(Debug, Clone)]
pub struct ControlMasterSession {
    pub socket_path: PathBuf,
    pub ip: String,
    pub port: u16,
    pub key_path: PathBuf,
    pub proxy_command: Option<String>,
}

/// Resolve a safe, short, user-isolated UNIX domain socket path for OpenSSH ControlMaster.
/// Guarantees path length stays well below Darwin (104) and Linux (108) limits.
pub fn safe_control_socket_path(vm_name: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let base_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("qecs")
        });

    let sock_dir = base_dir.join("socks");
    let _ = std::fs::create_dir_all(&sock_dir);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700));
    }

    let mut hasher = Sha256::new();
    hasher.update(vm_name.as_bytes());
    let hash_prefix = &hex::encode(hasher.finalize())[..12];

    sock_dir.join(format!("c-{hash_prefix}.sock"))
}

impl ControlMasterSession {
    /// Construct a new session descriptor with a safe short socket path.
    pub fn new(
        vm_name: &str,
        ip: &str,
        port: u16,
        key_path: &Path,
        proxy_command: Option<&str>,
    ) -> Self {
        let socket_path = safe_control_socket_path(vm_name);

        Self {
            socket_path,
            ip: ip.to_string(),
            port,
            key_path: key_path.to_path_buf(),
            proxy_command: proxy_command.map(String::from),
        }
    }

    /// Arguments to inject into SSH command invocations to attach to or establish this ControlMaster.
    pub fn ssh_control_args(&self) -> Vec<String> {
        vec![
            "-o".to_string(),
            "ControlMaster=auto".to_string(),
            "-o".to_string(),
            format!("ControlPath={}", self.socket_path.display()),
            "-o".to_string(),
            "ControlPersist=180".to_string(),
        ]
    }

    /// Dynamically forward a remote port to an available local port using `-O forward`.
    pub fn forward_port(&self, remote_port: u16) -> anyhow::Result<u16> {
        let local_port = find_available_local_port(remote_port)
            .ok_or_else(|| anyhow::anyhow!("no local ports available for forwarding"))?;

        let mut cmd = Command::new("ssh");
        cmd.arg("-S")
            .arg(&self.socket_path)
            .arg("-O")
            .arg("forward")
            .arg("-L")
            .arg(format!("{local_port}:127.0.0.1:{remote_port}"))
            .arg("dummy");

        let status = cmd
            .status()
            .context("requesting OpenSSH port forward via ControlMaster")?;

        if !status.success() {
            anyhow::bail!("OpenSSH ControlMaster rejected port forward for port {remote_port}");
        }

        Ok(local_port)
    }

    /// Cleanly terminate the ControlMaster connection and close all forwarded tunnels.
    pub fn close(&self) {
        if self.socket_path.exists() {
            let mut cmd = Command::new("ssh");
            cmd.arg("-S")
                .arg(&self.socket_path)
                .arg("-O")
                .arg("exit")
                .arg("dummy");
            let _ = cmd.output();
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

/// Spawns a background task that periodically inspects remote `/proc/net/tcp` via the
/// established ControlMaster and auto-forwards any newly listening web service ports.
/// Returns a one-shot sender to terminate the watcher when the job concludes.
pub fn spawn_port_watcher(session: ControlMasterSession) -> tokio::sync::oneshot::Sender<()> {
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        let mut forwarded = HashSet::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(1500));
        // Skip first immediate tick to let connection establish
        interval.tick().await;

        loop {
            tokio::select! {
                _ = &mut rx => break,
                _ = interval.tick() => {
                    if !session.socket_path.exists() {
                        continue;
                    }

                    let mut cmd = tokio::process::Command::new("ssh");
                    cmd.arg("-S")
                        .arg(&session.socket_path)
                        .arg("dummy")
                        .arg("cat /proc/net/tcp /proc/net/tcp6 2>/dev/null");

                    if let Ok(output) = cmd.output().await
                        && output.status.success()
                    {
                        let text = String::from_utf8_lossy(&output.stdout);
                        let detected_ports = parse_proc_net_tcp(&text);
                        for p in detected_ports {
                            if !forwarded.contains(&p)
                                && let Ok(local_port) = session.forward_port(p)
                            {
                                forwarded.insert(p);
                                let banner =
                                    format_tunnel_banner(p, local_port, guess_service_name(p));
                                eprintln!("{banner}");
                            }
                        }
                    }
                }
            }
        }
    });

    tx
}

impl Drop for ControlMasterSession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_net_tcp_listening_ports() {
        let fixture = r#"  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode                                                     
   0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 18621 1 0000000000000000 100 0 0 10 0                   
   1: 0100007F:0035 00000000:0000 0A 00000000:00000000 00:00000000 00000000   101        0 15729 1 0000000000000000 100 0 0 10 0                   
   2: 00000000:1EB4 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 38291 1 0000000000000000 100 0 0 10 0                   
   3: 0100007F:22B8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 38292 1 0000000000000000 100 0 0 10 0                   
   4: 00000000:1F90 00000000:0000 01 00000000:00000000 00:00000000 00000000  1000        0 38293 1 0000000000000000 100 0 0 10 0                   
"#;

        let ports = parse_proc_net_tcp(fixture);
        // 0x0016 = 22 (excluded: system port)
        // 0x0035 = 53 (excluded: system port)
        // 0x1EB4 = 7860 (included: Gradio)
        // 0x22B8 = 8888 (included: Jupyter)
        // 0x1F90 = 8080 (excluded: state 01 TCP_ESTABLISHED, not 0A)
        assert_eq!(ports, vec![7860, 8888]);
    }

    #[test]
    fn guess_service_name_maps_known_ports() {
        assert_eq!(guess_service_name(7860), Some("Gradio"));
        assert_eq!(guess_service_name(8501), Some("Streamlit"));
        assert_eq!(guess_service_name(8888), Some("Jupyter"));
        assert_eq!(guess_service_name(8000), Some("HTTP / FastAPI"));
        assert_eq!(guess_service_name(5173), Some("Vite"));
        assert_eq!(guess_service_name(9999), None);
    }

    #[test]
    fn identifies_system_ports() {
        assert!(is_system_port(22));
        assert!(is_system_port(53));
        assert!(is_system_port(80)); // < 1024
        assert!(is_system_port(443)); // < 1024
        assert!(!is_system_port(7860));
        assert!(!is_system_port(8501));
    }

    #[test]
    fn finds_available_local_port() {
        let p = find_available_local_port(18765).expect("free port");
        assert!(p >= 18765);
    }

    #[test]
    fn formats_banner_nicely() {
        let banner = format_tunnel_banner(7860, 7860, Some("Gradio"));
        assert!(banner.contains("http://localhost:7860"));
        assert!(banner.contains("Gradio"));
        assert!(banner.contains("remote port 7860"));
    }
}
