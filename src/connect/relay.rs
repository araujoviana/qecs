//! Pluggable reverse-tunnel relay adapters for strict firewalls and campus proxies.

use crate::config::RelayConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relay {
    None,
    Bore {
        server: String,
        port: u16,
        token: Option<String>,
    },
    Cloudflare {
        hostname: String,
    },
    Custom {
        proxy_command: String,
    },
}

impl Relay {
    pub fn from_config(cfg: Option<&RelayConfig>) -> Self {
        let Some(c) = cfg else {
            return Relay::None;
        };

        match c.r#type.as_str() {
            "bore" => Relay::Bore {
                server: c.server.clone().unwrap_or_else(|| "bore.pub".to_string()),
                port: c.port.unwrap_or(7835),
                token: c.token.clone(),
            },
            "cloudflare" => Relay::Cloudflare {
                hostname: c.server.clone().unwrap_or_default(),
            },
            "custom" => {
                if let Some(ref cmd) = c.proxy_command {
                    Relay::Custom {
                        proxy_command: cmd.clone(),
                    }
                } else {
                    Relay::None
                }
            }
            _ => Relay::None,
        }
    }

    /// Resolve OpenSSH ProxyCommand string for targeting an instance.
    pub fn proxy_command(&self, ip: &str, port: u16) -> Option<String> {
        match self {
            Relay::None => None,
            Relay::Bore {
                server,
                port: rport,
                ..
            } => Some(format!("nc {server} {rport}")),
            Relay::Cloudflare { hostname } => {
                let target = if hostname.is_empty() {
                    ip
                } else {
                    hostname.as_str()
                };
                Some(format!("cloudflared access ssh --hostname {target}"))
            }
            Relay::Custom { proxy_command } => Some(
                proxy_command
                    .replace("%h", ip)
                    .replace("%p", &port.to_string()),
            ),
        }
    }

    /// Generate optional cloud-init shell script and runcmd integration.
    pub fn cloudinit_write_file(&self) -> Option<String> {
        match self {
            Relay::Bore {
                server,
                port,
                token,
            } => {
                let token_flag = match token {
                    Some(t) => format!("--secret '{t}'"),
                    None => "".to_string(),
                };
                Some(format!(
                    r#"  - path: /usr/local/bin/qecs-relay-start.sh
    permissions: "0755"
    content: |
      #!/bin/bash
      set -u
      LOG="/var/log/qecs-relay.log"
      mkdir -p /run/qecs

      if ! command -v bore >/dev/null 2>&1; then
          ARCH=$(uname -m)
          if [ "$ARCH" = "x86_64" ]; then
              curl -fsSL https://github.com/ekzhang/bore/releases/download/v0.5.2/bore-v0.5.2-x86_64-unknown-linux-musl.tar.gz | tar -xz -C /usr/local/bin 2>> "$LOG" || true
          fi
      fi

      if command -v bore >/dev/null 2>&1; then
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-relay] Starting bore reverse tunnel to {server}:{port}..." >> "$LOG"
          nohup bore local 22 --to {server} --port {port} {token_flag} > "$LOG" 2>&1 &
          touch /run/qecs/relay.ready
      fi
"#
                ))
            }
            Relay::Cloudflare { .. } | Relay::Custom { .. } | Relay::None => None,
        }
    }

    pub fn cloudinit_runcmd(&self) -> Option<String> {
        match self {
            Relay::Bore { .. } => Some("  - [bash, /usr/local/bin/qecs-relay-start.sh]\n".into()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bore_relay_config() {
        let cfg = RelayConfig {
            r#type: "bore".into(),
            server: Some("relay.example.com".into()),
            port: Some(9999),
            token: Some("secret123".into()),
            proxy_command: None,
        };
        let relay = Relay::from_config(Some(&cfg));
        assert!(matches!(relay, Relay::Bore { .. }));
        assert_eq!(
            relay.proxy_command("10.0.0.1", 22).as_deref(),
            Some("nc relay.example.com 9999")
        );
        let script = relay.cloudinit_write_file().unwrap();
        assert!(script.contains("bore local 22 --to relay.example.com --port 9999"));
        assert!(script.contains("--secret 'secret123'"));
    }

    #[test]
    fn parses_custom_relay_with_placeholder_replacement() {
        let cfg = RelayConfig {
            r#type: "custom".into(),
            server: None,
            port: None,
            token: None,
            proxy_command: Some("ssh -W %h:%p bastion.school.edu".into()),
        };
        let relay = Relay::from_config(Some(&cfg));
        assert_eq!(
            relay.proxy_command("192.168.0.42", 443).as_deref(),
            Some("ssh -W 192.168.0.42:443 bastion.school.edu")
        );
        assert_eq!(relay.cloudinit_write_file(), None);
    }

    #[test]
    fn none_relay_yields_no_proxy_or_cloudinit() {
        let relay = Relay::from_config(None);
        assert_eq!(relay.proxy_command("1.2.3.4", 22), None);
        assert_eq!(relay.cloudinit_write_file(), None);
        assert_eq!(relay.cloudinit_runcmd(), None);
    }
}
