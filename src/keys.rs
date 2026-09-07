//! Dedicated SSH keypair management for qecs VMs (~/.config/qecs/keys/id_qecs).
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct KeyPairPaths {
    pub private_key: PathBuf,
    pub public_key: PathBuf,
}

/// Default directory for qecs keys: `~/.config/qecs/keys`.
pub fn default_key_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("qecs")
        .join("keys")
}

/// qecs-owned `known_hosts` file: `~/.config/qecs/known_hosts`.
///
/// Kept separate from the user's `~/.ssh/known_hosts` so that ephemeral VMs
/// churning through a recycled elastic-IP pool never wedge the user's real
/// host-key database, while still giving trust-on-first-use verification
/// (a later connection to a changed key on the same address fails closed).
pub fn qecs_known_hosts_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("qecs")
        .join("known_hosts")
}

/// Remove any entries for `ip` from the given `known_hosts` file.
pub fn remove_known_host_from_path(path: &Path, ip: &str) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let content = std::fs::read_to_string(path)?;
    let mut modified = false;
    let new_lines: Vec<&str> = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return true;
            }
            let first_token = trimmed.split_whitespace().next().unwrap_or_default();
            let matches_ip = first_token == ip
                || first_token.starts_with(&format!("{ip},"))
                || first_token.starts_with(&format!("[{ip}]:"));
            if matches_ip {
                modified = true;
                false
            } else {
                true
            }
        })
        .collect();

    if modified {
        let mut out = new_lines.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(path, out)?;
    }
    Ok(())
}

/// Remove any entries for `ip` from `~/.config/qecs/known_hosts` so recycled IPs
/// never cause a "REMOTE HOST IDENTIFICATION HAS CHANGED" verification failure.
pub fn remove_known_host(ip: &str) -> std::io::Result<()> {
    remove_known_host_from_path(&qecs_known_hosts_path(), ip)
}

/// Ensure that the dedicated `id_qecs` ed25519 keypair exists in `key_dir`.
/// If missing, generate it via `ssh-keygen`. Returns the paths and the public key content.
pub fn ensure_keypair(key_dir: Option<&Path>) -> anyhow::Result<(KeyPairPaths, String)> {
    let dir = match key_dir {
        Some(d) => d.to_path_buf(),
        None => default_key_dir(),
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating key directory {}", dir.display()))?;

    let priv_path = dir.join("id_qecs");
    let pub_path = dir.join("id_qecs.pub");

    if !priv_path.exists() || !pub_path.exists() {
        // If one is missing while the other exists, clean up to avoid key mismatch
        let _ = std::fs::remove_file(&priv_path);
        let _ = std::fs::remove_file(&pub_path);

        let status = Command::new("ssh-keygen")
            .arg("-t")
            .arg("ed25519")
            .arg("-N")
            .arg("")
            .arg("-C")
            .arg("qecs")
            .arg("-f")
            .arg(&priv_path)
            .status()
            .context("failed to execute ssh-keygen; please ensure OpenSSH is installed")?;

        if !status.success() {
            anyhow::bail!("ssh-keygen failed with exit code: {:?}", status.code());
        }
    }

    let pub_content = std::fs::read_to_string(&pub_path)
        .with_context(|| format!("reading public key {}", pub_path.display()))?
        .trim()
        .to_string();

    Ok((
        KeyPairPaths {
            private_key: priv_path,
            public_key: pub_path,
        },
        pub_content,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_ed25519_keypair_in_dir() {
        let temp = tempfile::tempdir().unwrap();
        let (paths, pub_key) = ensure_keypair(Some(temp.path())).unwrap();

        assert!(paths.private_key.exists());
        assert!(paths.public_key.exists());
        assert!(pub_key.starts_with("ssh-ed25519 "));
        assert!(pub_key.ends_with("qecs"));

        // Second run reuses existing keypair
        let (paths2, pub_key2) = ensure_keypair(Some(temp.path())).unwrap();
        assert_eq!(paths.private_key, paths2.private_key);
        assert_eq!(pub_key, pub_key2);
    }

    #[test]
    fn remove_known_host_removes_matching_ips_and_ports() {
        let temp = tempfile::tempdir().unwrap();
        let kh = temp.path().join("known_hosts");
        let initial = "\
# comment
110.238.107.84 ssh-ed25519 AAAAC3_OLD_KEY
[110.238.107.84]:443 ssh-ed25519 AAAAC3_OLD_KEY_443
1.2.3.4 ssh-ed25519 AAAAC3_OTHER_HOST
";
        std::fs::write(&kh, initial).unwrap();

        remove_known_host_from_path(&kh, "110.238.107.84").unwrap();

        let updated = std::fs::read_to_string(&kh).unwrap();
        assert!(!updated.contains("110.238.107.84"));
        assert!(updated.contains("1.2.3.4 ssh-ed25519 AAAAC3_OTHER_HOST"));
        assert!(updated.contains("# comment"));
    }
}
