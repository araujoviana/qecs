//! Probing TCP and SSH identification banners on candidate ports (22, 443).
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Probe a host and port to verify that an SSH daemon is responding.
pub async fn probe_ssh_port(ip: &str, port: u16, timeout_duration: Duration) -> bool {
    let addr = format!("{ip}:{port}");
    let connect_fut = TcpStream::connect(&addr);
    let Ok(Ok(mut stream)) = timeout(timeout_duration, connect_fut).await else {
        return false;
    };

    // Verify SSH banner (e.g. "SSH-2.0-OpenSSH...")
    let mut buf = [0u8; 64];
    let read_fut = stream.read(&mut buf);
    match timeout(Duration::from_millis(1500), read_fut).await {
        Ok(Ok(n)) if n > 0 => {
            let banner = String::from_utf8_lossy(&buf[..n]);
            banner.starts_with("SSH-")
        }
        _ => false,
    }
}

/// Resolve the working SSH port for an IP (probing 22 then 443, honoring cached port).
/// If direct ports 22 and 443 are unreachable but a relay is configured, returns 22:
/// the VM's own sshd port, which the relay's `ProxyCommand` tunnels the connection to.
pub async fn resolve_connection_port_with_relay(
    ip: &str,
    cached_port: Option<u16>,
    relay: Option<&crate::connect::Relay>,
) -> anyhow::Result<u16> {
    let probe_timeout = Duration::from_secs(3);

    // 1. Try cached port first if known (a cached relay port is 22, so this is safe)
    if let Some(port) = cached_port
        && probe_ssh_port(ip, port, probe_timeout).await
    {
        return Ok(port);
    }

    // 2. Try default port 22
    if probe_ssh_port(ip, 22, probe_timeout).await {
        return Ok(22);
    }

    // 3. Try alternative port 443 (configured in cloud-init)
    if probe_ssh_port(ip, 443, probe_timeout).await {
        return Ok(443);
    }

    // 4. Fallback to reverse tunnel / relay if configured. The ProxyCommand handles
    //    reaching the relay; ssh still targets the VM's real sshd on 22.
    if let Some(r) = relay
        && !matches!(r, crate::connect::Relay::None)
    {
        return Ok(22);
    }

    anyhow::bail!(
        "could not connect to SSH on {ip} (ports 22 and 443 both unreachable; host may still be booting or blocked by a firewall)"
    )
}

/// Resolve the working SSH port for an IP (probing 22 then 443, honoring cached port).
pub async fn resolve_connection_port(ip: &str, cached_port: Option<u16>) -> anyhow::Result<u16> {
    resolve_connection_port_with_relay(ip, cached_port, None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn probe_detects_ssh_banner() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.write_all(b"SSH-2.0-OpenSSH_9.6p1 Ubuntu\r\n").await;
            }
        });

        assert!(probe_ssh_port("127.0.0.1", port, Duration::from_millis(500)).await);
    }

    #[tokio::test]
    async fn probe_rejects_non_ssh_banner() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n").await;
            }
        });

        assert!(!probe_ssh_port("127.0.0.1", port, Duration::from_millis(500)).await);
    }

    #[tokio::test]
    async fn probe_fails_on_closed_port() {
        // Find an unused port and probe it immediately
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        assert!(!probe_ssh_port("127.0.0.1", port, Duration::from_millis(100)).await);
    }

    #[tokio::test]
    async fn resolve_falls_back_to_relay_dialing_the_vm_sshd_port_not_the_relay_port() {
        let relay = crate::connect::Relay::Bore {
            server: "bore.pub".into(),
            port: 2200,
            token: None,
        };

        // 192.0.2.1 (TEST-NET-1) is unroutable, so direct 22/443 both fail and the
        // relay path is taken. The returned port is the VM's own sshd port (22),
        // which the ProxyCommand tunnels to -- not the relay's listen port.
        let res = resolve_connection_port_with_relay("192.0.2.1", None, Some(&relay)).await;
        assert_eq!(res.unwrap(), 22);
    }

    #[tokio::test]
    async fn resolve_fails_when_direct_ports_fail_and_no_relay() {
        let res = resolve_connection_port_with_relay("192.0.2.1", None, None).await;
        assert!(res.is_err());
    }
}
