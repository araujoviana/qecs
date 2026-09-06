//! Cloud-init rendering for ephemeral ECS instances.
//! Configures sshd on 22 + 443 and authorizes the dedicated qecs key.

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard RFC 4648 Base64 encoding without external dependencies.
pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
        out.push(B64_CHARS[(b0 >> 2) as usize] as char);
        out.push(B64_CHARS[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_CHARS[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_CHARS[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Render the base `#cloud-config` user-data for Ubuntu/Debian instances.
pub fn render_base_cloudinit(public_key: &str) -> String {
    format!(
        r#"#cloud-config
users:
  - default
  - name: ubuntu
    gecos: Ubuntu User
    sudo: "ALL=(ALL) NOPASSWD:ALL"
    shell: /bin/bash
    ssh_authorized_keys:
      - {public_key}

write_files:
  - path: /etc/ssh/sshd_config.d/qecs.conf
    permissions: "0644"
    content: |
      Port 22
      Port 443

runcmd:
  - [systemctl, restart, ssh]
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn test_cloudinit_contains_key_and_ports() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA qecs";
        let rendered = render_base_cloudinit(key);
        assert!(rendered.starts_with("#cloud-config"));
        assert!(rendered.contains(key));
        assert!(rendered.contains("Port 22"));
        assert!(rendered.contains("Port 443"));
    }
}
