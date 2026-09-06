//! Cloud-init rendering for ephemeral ECS instances.
//! Configures sshd on 22 + 443, authorizes dedicated qecs key,
//! installs autonomous `qecs-guard` daemon, and optionally installs NVIDIA drivers.

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

/// Render the complete `#cloud-config` user-data for Ubuntu instances.
/// Injects dual-port sshd, dedicated SSH authorized key, autonomous lifecycle guard,
/// systemd timer, fallback shutdown, and optionally NVIDIA GPU driver setup.
pub fn render_cloudinit(
    public_key: &str,
    ttl_secs: u64,
    idle_timeout_secs: u64,
    needs_gpu: bool,
) -> String {
    let ttl_minutes = (ttl_secs / 60).max(1);

    let gpu_write_files = if needs_gpu {
        r#"
  - path: /usr/local/bin/qecs-gpu-setup.sh
    permissions: "0755"
    content: |
      #!/bin/bash
      set -u
      LOG="/var/log/qecs-gpu-setup.log"
      mkdir -p /run/qecs
      echo "INSTALLING" > /run/qecs/gpu.status
      echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-gpu] Initializing GPU driver setup..." | tee -a "$LOG"

      # 0. Fast-path: Check if pre-baked GPU drivers are already operational
      if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi >> "$LOG" 2>&1; then
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-gpu] Pre-baked GPU drivers detected and functional." | tee -a "$LOG"
          touch /run/qecs/gpu.ready
          echo "READY" > /run/qecs/gpu.status
          exit 0
      fi

      # 1. Blacklist nouveau if loaded
      if lsmod | grep -q nouveau; then
          echo "blacklist nouveau" > /etc/modprobe.d/blacklist-nouveau.conf
          echo "options nouveau modeset=0" >> /etc/modprobe.d/blacklist-nouveau.conf
          rmmod nouveau 2>/dev/null || true
      fi

      export DEBIAN_FRONTEND=noninteractive

      # 2. Wait for package locks if background updates are running
      for _ in $(seq 1 30); do
          if ! fuser /var/lib/dpkg/lock-frontend >/dev/null 2>&1; then
              break
          fi
          echo "[qecs-gpu] Waiting for apt/dpkg lock..." >> "$LOG"
          sleep 2
      done

      # 3. Install kernel headers and dependencies
      echo "[qecs-gpu] Installing kernel headers and dependencies..." >> "$LOG"
      apt-get update >> "$LOG" 2>&1 || true
      apt-get install -y --no-install-recommends \
          linux-headers-"$(uname -r)" \
          build-essential \
          dkms \
          curl \
          gnupg \
          pciutils >> "$LOG" 2>&1 || true

      # 4. Install NVIDIA server driver
      echo "[qecs-gpu] Installing NVIDIA server driver..." >> "$LOG"
      INSTALLED=0

      if command -v ubuntu-drivers >/dev/null 2>&1; then
          ubuntu-drivers install --gpgpu >> "$LOG" 2>&1 && INSTALLED=1
      fi
      if [ "$INSTALLED" -ne 1 ]; then
          apt-get install -y --no-install-recommends nvidia-headless-535-server nvidia-utils-535-server >> "$LOG" 2>&1 && INSTALLED=1
      fi
      if [ "$INSTALLED" -ne 1 ]; then
          apt-get install -y --no-install-recommends nvidia-driver-535-server >> "$LOG" 2>&1 && INSTALLED=1
      fi
      if [ "$INSTALLED" -ne 1 ]; then
          apt-get install -y --no-install-recommends nvidia-driver-550-server >> "$LOG" 2>&1 && INSTALLED=1
      fi

      # 5. Load kernel modules
      modprobe nvidia >> "$LOG" 2>&1 || true
      modprobe nvidia_uvm >> "$LOG" 2>&1 || true

      # 6. Install nvidia-container-toolkit for Docker
      echo "[qecs-gpu] Installing nvidia-container-toolkit..." >> "$LOG"
      curl -fsSL https://nvidia.github.io/libnvidia-container/gpgkey | gpg --dearmor -o /usr/share/keyrings/nvidia-container-toolkit-keyring.gpg >> "$LOG" 2>&1 || true
      curl -s -L https://nvidia.github.io/libnvidia-container/stable/deb/nvidia-container-toolkit.list | \
          sed 's#deb https://#deb [signed-by=/usr/share/keyrings/nvidia-container-toolkit-keyring.gpg] https://#g' | \
          tee /etc/apt/sources.list.d/nvidia-container-toolkit.list >> "$LOG" 2>&1 || true
      apt-get update >> "$LOG" 2>&1 || true
      apt-get install -y nvidia-container-toolkit >> "$LOG" 2>&1 || true

      if command -v nvidia-ctk >/dev/null 2>&1; then
          nvidia-ctk runtime configure --runtime=docker >> "$LOG" 2>&1 || true
          systemctl restart docker 2>/dev/null || true
      fi

      # 7. Verification check
      echo "[qecs-gpu] Verifying with nvidia-smi..." >> "$LOG"
      if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi >> "$LOG" 2>&1; then
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-gpu] GPU operational." | tee -a "$LOG"
          touch /run/qecs/gpu.ready
          echo "READY" > /run/qecs/gpu.status
          exit 0
      else
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-gpu] ERROR: nvidia-smi failed." | tee -a "$LOG"
          touch /run/qecs/gpu.failed
          echo "FAILED" > /run/qecs/gpu.status
          exit 1
      fi

  - path: /etc/systemd/system/qecs-gpu-setup.service
    permissions: "0644"
    content: |
      [Unit]
      Description=qecs autonomous GPU driver and container toolkit installer
      After=network-online.target
      Wants=network-online.target

      [Service]
      Type=oneshot
      ExecStart=/usr/local/bin/qecs-gpu-setup.sh
      RemainAfterExit=yes
      StandardOutput=journal+console
      StandardError=journal+console

      [Install]
      WantedBy=multi-user.target
"#
    } else {
        ""
    };

    let gpu_runcmd = if needs_gpu {
        "  - [systemctl, enable, --now, qecs-gpu-setup.service]\n"
    } else {
        ""
    };

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

  - path: /usr/local/bin/qecs-guard.sh
    permissions: "0755"
    content: |
      #!/bin/bash
      set -u
      TTL_SECS={ttl_secs}
      IDLE_TIMEOUT_SECS={idle_timeout_secs}
      LOG_FILE="/var/log/qecs-spend.log"
      LOCK_FILE="/run/qecs/job.lock"
      IDLE_STATE_FILE="/run/qecs/idle_seconds"

      mkdir -p /run/qecs

      # 1. Check TTL expiration
      UPTIME_SECS=$(awk '{{print int($1)}}' /proc/uptime 2>/dev/null || echo 0)
      if [ "$UPTIME_SECS" -ge "$TTL_SECS" ]; then
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-guard] TTL reached (${{UPTIME_SECS}}s >= ${{TTL_SECS}}s). Powering off." | tee -a "$LOG_FILE"
          poweroff
          exit 0
      fi

      # 2. Check active job lock
      if [ -f "$LOCK_FILE" ]; then
          rm -f "$IDLE_STATE_FILE"
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-guard] Job lock active. Machine in use." >> "$LOG_FILE"
          exit 0
      fi

      # 3. Check active SSH sessions
      ACTIVE_SSH=$(ss -t state established '( sport = :22 or sport = :443 )' 2>/dev/null | grep -v "Recv-Q" | wc -l)
      if [ "$ACTIVE_SSH" -gt 0 ]; then
          rm -f "$IDLE_STATE_FILE"
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-guard] Active SSH sessions ($ACTIVE_SSH). Machine in use." >> "$LOG_FILE"
          exit 0
      fi

      # 4. Check GPU utilization if nvidia-smi present
      if command -v nvidia-smi >/dev/null 2>&1; then
          GPU_UTIL=$(nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits 2>/dev/null | head -n 1 | tr -d ' %')
          if [ -n "$GPU_UTIL" ] && [ "$GPU_UTIL" -ge 5 ]; then
              rm -f "$IDLE_STATE_FILE"
              echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-guard] GPU active (${{GPU_UTIL}}%). Machine in use." >> "$LOG_FILE"
              exit 0
          fi
      fi

      # 5. Machine is idle: track duration
      CURRENT_IDLE=0
      if [ -f "$IDLE_STATE_FILE" ]; then
          CURRENT_IDLE=$(cat "$IDLE_STATE_FILE" 2>/dev/null || echo 0)
      fi
      CURRENT_IDLE=$((CURRENT_IDLE + 120))
      echo "$CURRENT_IDLE" > "$IDLE_STATE_FILE"

      UPTIME_HOURS=$(awk '{{printf "%.2f", $1/3600}}' /proc/uptime 2>/dev/null || echo "0.0")
      echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-guard] Idle for ${{CURRENT_IDLE}}s (threshold: ${{IDLE_TIMEOUT_SECS}}s). Uptime: ${{UPTIME_HOURS}}h." >> "$LOG_FILE"

      if [ "$CURRENT_IDLE" -ge "$IDLE_TIMEOUT_SECS" ]; then
          echo "$(date -u +'%Y-%m-%dT%H:%M:%SZ') [qecs-guard] Idle timeout reached. Powering off." | tee -a "$LOG_FILE"
          poweroff
      fi

  - path: /etc/systemd/system/qecs-guard.service
    permissions: "0644"
    content: |
      [Unit]
      Description=qecs autonomous lifecycle and budget guard
      After=network.target

      [Service]
      Type=oneshot
      ExecStart=/usr/local/bin/qecs-guard.sh

  - path: /etc/systemd/system/qecs-guard.timer
    permissions: "0644"
    content: |
      [Unit]
      Description=Run qecs-guard every 2 minutes

      [Timer]
      OnBootSec=2min
      OnUnitActiveSec=2min
      AccuracySec=10s

      [Install]
      WantedBy=timers.target
{gpu_write_files}
runcmd:
  - [systemctl, restart, ssh]
  - [mkdir, -p, /run/qecs]
  - [systemctl, daemon-reload]
  - [systemctl, enable, --now, qecs-guard.timer]
{gpu_runcmd}  - shutdown -h +{ttl_minutes} "qecs hard TTL guard"
"#
    )
}

/// Backward-compatible baseline cloudinit helper.
pub fn render_base_cloudinit(public_key: &str) -> String {
    render_cloudinit(public_key, 7200, 1200, false)
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
    fn test_cloudinit_contains_guard_and_systemd_timer() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA qecs";
        let rendered = render_cloudinit(key, 3600, 600, false);

        assert!(rendered.starts_with("#cloud-config"));
        assert!(rendered.contains(key));
        assert!(rendered.contains("Port 22"));
        assert!(rendered.contains("Port 443"));

        // Guard script
        assert!(rendered.contains("/usr/local/bin/qecs-guard.sh"));
        assert!(rendered.contains("TTL_SECS=3600"));
        assert!(rendered.contains("IDLE_TIMEOUT_SECS=600"));
        assert!(rendered.contains("/run/qecs/job.lock"));

        // Systemd files
        assert!(rendered.contains("/etc/systemd/system/qecs-guard.service"));
        assert!(rendered.contains("/etc/systemd/system/qecs-guard.timer"));
        assert!(rendered.contains("qecs-guard.timer"));

        // Hard fallback shutdown
        assert!(rendered.contains("shutdown -h +60"));

        // No GPU files when disabled
        assert!(!rendered.contains("qecs-gpu-setup.sh"));
        assert!(!rendered.contains("qecs-gpu-setup.service"));
    }

    #[test]
    fn test_cloudinit_contains_gpu_setup_when_enabled() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA qecs";
        let rendered = render_cloudinit(key, 7200, 1200, true);

        // Contains GPU setup script and systemd service
        assert!(rendered.contains("/usr/local/bin/qecs-gpu-setup.sh"));
        assert!(rendered.contains("/etc/systemd/system/qecs-gpu-setup.service"));
        assert!(rendered.contains("qecs-gpu-setup.service"));
        assert!(rendered.contains("nvidia-container-toolkit"));
        assert!(rendered.contains("/run/qecs/gpu.ready"));
        assert!(rendered.contains("/run/qecs/gpu.failed"));
        assert!(rendered.contains("nvidia-smi"));
    }

    #[test]
    fn test_cloudinit_scripts_pass_bash_syntax_check() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA qecs";
        let rendered = render_cloudinit(key, 7200, 1200, true);

        let mut pos = 0;
        let mut script_count = 0;
        while let Some(start) = rendered[pos..].find("#!/bin/bash") {
            let abs_start = pos + start;
            let end = rendered[abs_start..]
                .find("\n  - path:")
                .unwrap_or(rendered[abs_start..].len());
            let script_raw = &rendered[abs_start..abs_start + end];
            let script: String = script_raw
                .lines()
                .map(|l| l.trim_start())
                .collect::<Vec<_>>()
                .join("\n");

            let mut child = Command::new("bash")
                .arg("-n")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to execute bash -n");

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(script.as_bytes()).unwrap();
            }

            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "bash -n failed on script:\n{script}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            script_count += 1;
            pos = abs_start + end;
        }

        assert_eq!(
            script_count, 2,
            "should validate both guard and gpu scripts"
        );
    }
}
