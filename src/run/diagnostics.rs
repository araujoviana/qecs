//! Post-mortem failure diagnostics and root-cause analysis for `qecs run`.
//!
//! Inspects remote kernel logs (`dmesg`), spend logs, exit codes, and stderr patterns
//! to produce actionable Cargo-style diagnostic reports before VM teardown.

use std::path::Path;

use crate::error::DiagnosticReport;
use crate::presets::Preset;

/// Probe remote failure cause and build an actionable diagnostic report.
#[allow(clippy::too_many_arguments)]
pub fn inspect_failure(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    exit_code: i32,
    stderr_tail: &[String],
    preset: Preset,
    flavor: &str,
    vm_name: &str,
) -> Option<DiagnosticReport> {
    let target = format!(
        "Remote VM: {vm_name} (flavor: {flavor}, preset: {})",
        preset.as_str()
    );
    // 1. Check local stderr patterns first if provided
    let stderr_joined = stderr_tail.join("\n");
    if !stderr_joined.is_empty()
        && let Some(mut report) = diagnose_from_stderr(&stderr_joined)
    {
        report.target = target;
        return Some(report);
    }

    // 2. Query remote kernel, spend logs, and remote job.stderr via single SSH command
    let (dmesg, spend_log, remote_stderr) =
        fetch_remote_crash_logs(ip, port, key_path, proxy_command);

    // 3. Check remote stderr patterns if local stderr was empty
    if stderr_joined.is_empty()
        && !remote_stderr.is_empty()
        && let Some(mut report) = diagnose_from_stderr(&remote_stderr)
    {
        report.target = target;
        return Some(report);
    }

    // 4. Analyze exit code and remote logs
    diagnose_from_dmesg_or_code(exit_code, &dmesg, &spend_log, preset, &target)
}

/// Fetch dmesg, spend log, and remote stderr snippets over SSH in a single command.
fn fetch_remote_crash_logs(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
) -> (String, String, String) {
    let composite_cmd = "echo '===DMESG==='; dmesg -T 2>/dev/null | grep -iE 'oom-killer|killed process|segfault' | tail -n 8; echo '===SPEND==='; tail -n 10 /var/log/qecs-spend.log 2>/dev/null || true; echo '===STDERR==='; tail -n 100 /run/qecs/job.stderr 2>/dev/null || true";
    let output = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(composite_cmd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let mut dmesg = String::new();
    let mut spend = String::new();
    let mut stderr = String::new();
    let mut current_section = 0; // 1: dmesg, 2: spend, 3: stderr

    for line in output.lines() {
        if line.contains("===DMESG===") {
            current_section = 1;
        } else if line.contains("===SPEND===") {
            current_section = 2;
        } else if line.contains("===STDERR===") {
            current_section = 3;
        } else {
            match current_section {
                1 => {
                    dmesg.push_str(line);
                    dmesg.push('\n');
                }
                2 => {
                    spend.push_str(line);
                    spend.push('\n');
                }
                3 => {
                    stderr.push_str(line);
                    stderr.push('\n');
                }
                _ => {}
            }
        }
    }

    (
        dmesg.trim().to_string(),
        spend.trim().to_string(),
        stderr.trim().to_string(),
    )
}

/// Inspect stderr output for deterministic failure signatures.
pub fn diagnose_from_stderr(stderr: &str) -> Option<DiagnosticReport> {
    // A. CUDA Out of Memory
    if stderr.contains("torch.cuda.OutOfMemoryError")
        || stderr.contains("CUDA out of memory")
        || stderr.contains("CUBLAS_STATUS_ALLOC_FAILED")
        || stderr.contains("RuntimeError: Out of memory trying to allocate")
    {
        let details = stderr
            .lines()
            .find(|l| {
                l.contains("OutOfMemoryError")
                    || l.contains("out of memory")
                    || l.contains("CUBLAS_STATUS_ALLOC_FAILED")
            })
            .map(|l| l.trim().to_string());

        return Some(DiagnosticReport {
            code: "E-CUDA-OOM",
            title: "CUDA Out of Memory (GPU device memory exhausted)".into(),
            target: String::new(),
            details,
            note: Some("The PyTorch/CUDA workload attempted to allocate more VRAM than available on the device.".into()),
            recommendation: Some("Reduce batch size, enable gradient checkpointing, or use a larger GPU preset:".into()),
            command_suggestion: Some("qecs run --preset beefy (4x V100 128 GB VRAM)".into()),
        });
    }

    // B. Missing shared library (.so)
    if let Some(lib) = extract_missing_shared_lib(stderr) {
        let pkg = map_lib_to_apt_pkg(&lib);
        return Some(DiagnosticReport {
            code: "E-MISSING-LIB",
            title: format!("Missing system shared library `{lib}`"),
            target: String::new(),
            details: Some(format!("Error while loading shared libraries: {lib}")),
            note: Some(
                "Dynamic linker failed to locate a required native C/C++ shared library.".into(),
            ),
            recommendation: Some(format!("Install package `{pkg}` on the instance:")),
            command_suggestion: Some(format!(
                "Add 'sudo apt-get update && sudo apt-get install -y {pkg}' to qecs.toml [setup]"
            )),
        });
    }

    // C. Missing Python module
    if let Some(module) = extract_missing_python_module(stderr) {
        return Some(DiagnosticReport {
            code: "E-MISSING-MODULE",
            title: format!("Missing Python module `{module}`"),
            target: String::new(),
            details: Some(format!("ModuleNotFoundError: No module named '{module}'")),
            note: Some(format!(
                "The Python interpreter cannot find package `{module}` in the environment."
            )),
            recommendation: Some(format!(
                "Add `{module}` to requirements.txt or pyproject.toml"
            )),
            command_suggestion: None,
        });
    }

    // D. Disk space full
    if stderr.contains("No space left on device") || stderr.contains("ENOSPC") {
        return Some(DiagnosticReport {
            code: "E-ENOSPC",
            title: "Remote instance storage exhausted (No space left on device)".into(),
            target: String::new(),
            details: Some("Filesystem write failed with ENOSPC.".into()),
            note: Some("The root or workspace volume ran out of free disk space.".into()),
            recommendation: Some(
                "Select a preset with larger default disk, or increase storage in qecs.toml."
                    .into(),
            ),
            command_suggestion: None,
        });
    }

    None
}

/// Diagnose based on POSIX exit code, dmesg, and spend log.
pub fn diagnose_from_dmesg_or_code(
    exit_code: i32,
    dmesg: &str,
    spend_log: &str,
    preset: Preset,
    target: &str,
) -> Option<DiagnosticReport> {
    match exit_code {
        137 => {
            // Check dmesg for OOM killer
            if !dmesg.is_empty() && (dmesg.contains("oom-killer") || dmesg.contains("Killed process") || dmesg.contains("Out of memory")) {
                let rec = match preset {
                    Preset::Normal => "Re-run with higher memory preset: qecs run --preset ram (128 GB RAM)",
                    Preset::Compute => "Re-run with higher memory preset: qecs run --preset ram",
                    Preset::Gpu => "Workload exhausted system RAM. Re-run with: qecs run --preset beefy or --preset ram",
                    _ => "Select a custom high-memory flavor: qecs run --flavor m7.8xlarge.8",
                };
                return Some(DiagnosticReport {
                    code: "E137",
                    title: "Remote process terminated by Linux Out-Of-Memory (OOM) Killer".into(),
                    target: target.to_string(),
                    details: Some(dmesg.trim().to_string()),
                    note: Some("Workload allocated more memory than the VM's physical RAM limit.".into()),
                    recommendation: Some(rec.into()),
                    command_suggestion: Some(if preset == Preset::Normal { "qecs run --preset ram".into() } else { "qecs run --flavor m7.4xlarge.8".into() }),
                });
            }

            // Check spend log for hard TTL expiration
            if spend_log.contains("TTL reached") {
                return Some(DiagnosticReport {
                    code: "E-TTL-EXPIRED",
                    title: "Remote VM terminated by autonomous qecs-guard hard TTL ceiling".into(),
                    target: target.to_string(),
                    details: Some(spend_log.lines().filter(|l| l.contains("TTL reached")).collect::<Vec<_>>().join("\n")),
                    note: Some("The job exceeded the instance's configured Time-To-Live.".into()),
                    recommendation: Some("Increase the job TTL:".into()),
                    command_suggestion: Some("qecs run --ttl 4h ...".into()),
                });
            }

            // Generic 137
            Some(DiagnosticReport {
                code: "E137",
                title: "Remote process killed with SIGKILL (exit code 137)".into(),
                target: target.to_string(),
                details: None,
                note: Some("Commonly caused by Linux OOM killer when physical memory is exhausted.".into()),
                recommendation: Some("Try running on a higher memory machine preset:".into()),
                command_suggestion: Some("qecs run --preset ram".into()),
            })
        }
        139 => {
            let details = if !dmesg.is_empty() && dmesg.contains("segfault") {
                Some(dmesg.trim().to_string())
            } else {
                None
            };
            Some(DiagnosticReport {
                code: "E139",
                title: "Remote process crashed with Segmentation Fault (SIGSEGV)".into(),
                target: target.to_string(),
                details,
                note: Some("Often caused by binary ABI incompatibility, mismatched CUDA libraries, or corrupted C extensions.".into()),
                recommendation: Some("Verify compiled dependencies match the target Linux environment (Ubuntu 22.04 x86_64).".into()),
                command_suggestion: None,
            })
        }
        126 => Some(DiagnosticReport {
            code: "E126",
            title: "Permission denied executing command or entrypoint".into(),
            target: target.to_string(),
            details: None,
            note: Some("The target binary or script does not have executable permissions.".into()),
            recommendation: Some("Ensure file has executable bit set: chmod +x <script>".into()),
            command_suggestion: None,
        }),
        127 => Some(DiagnosticReport {
            code: "E127",
            title: "Command or binary not found on remote machine".into(),
            target: target.to_string(),
            details: None,
            note: Some("A binary referenced in the setup or run recipe is not installed on the base image.".into()),
            recommendation: Some("Add installation of the missing tool to setup commands in qecs.toml".into()),
            command_suggestion: None,
        }),
        _ => None,
    }
}

/// Extract library name from "cannot open shared object file: libXYZ.so"
fn extract_missing_shared_lib(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        if line.contains("cannot open shared object file") {
            for part in line.split(':') {
                let candidate = part.trim();
                if candidate.starts_with("lib") && candidate.contains(".so") {
                    return Some(candidate.to_string());
                }
            }
        }
    }
    None
}

/// Map known shared library names to their providing Debian/Ubuntu apt package.
fn map_lib_to_apt_pkg(lib: &str) -> &'static str {
    if lib.starts_with("libGL.so") {
        "libgl1"
    } else if lib.starts_with("libglib-2.0.so") {
        "libglib2.0-0"
    } else if lib.starts_with("libSM.so") {
        "libsm6"
    } else if lib.starts_with("libXrender.so") {
        "libxrender1"
    } else if lib.starts_with("libXext.so") {
        "libxext6"
    } else if lib.starts_with("libsndfile.so") {
        "libsndfile1"
    } else if lib.starts_with("libpq.so") {
        "libpq-dev"
    } else if lib.starts_with("libssl.so") || lib.starts_with("libcrypto.so") {
        "libssl-dev"
    } else if lib.starts_with("libffi.so") {
        "libffi-dev"
    } else if lib.starts_with("libcuda.so") {
        "nvidia-headless-535-server (or use --preset gpu)"
    } else {
        "the required package via: apt-file search <library>"
    }
}

/// Extract Python module name from "ModuleNotFoundError: No module named 'xyz'"
fn extract_missing_python_module(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        if let Some(pos) = line.find("ModuleNotFoundError: No module named ") {
            let rest = &line[pos + "ModuleNotFoundError: No module named ".len()..];
            let trimmed = rest.trim();
            if (trimmed.starts_with('\'') && trimmed.len() > 2)
                || (trimmed.starts_with('"') && trimmed.len() > 2)
            {
                let quote = trimmed.chars().next().unwrap();
                if let Some(end_quote) = trimmed[1..].find(quote) {
                    return Some(trimmed[1..=end_quote].to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cuda_oom() {
        let stderr = "Traceback (most recent call last):\n  File 'train.py', line 45\ntorch.cuda.OutOfMemoryError: CUDA out of memory. Tried to allocate 2.00 GiB\n";
        let diag = diagnose_from_stderr(stderr).expect("should diagnose cuda oom");
        assert_eq!(diag.code, "E-CUDA-OOM");
        assert!(diag.title.contains("CUDA Out of Memory"));
        assert!(diag.command_suggestion.unwrap().contains("--preset beefy"));
    }

    #[test]
    fn detects_missing_shared_lib() {
        let stderr = "python3: error while loading shared libraries: libGL.so.1: cannot open shared object file: No such file or directory\n";
        let diag = diagnose_from_stderr(stderr).expect("should diagnose missing lib");
        assert_eq!(diag.code, "E-MISSING-LIB");
        assert!(diag.title.contains("libGL.so.1"));
        assert!(diag.command_suggestion.unwrap().contains("libgl1"));
    }

    #[test]
    fn detects_missing_python_module() {
        let stderr =
            "Traceback (most recent call last):\nModuleNotFoundError: No module named 'fastapi'\n";
        let diag = diagnose_from_stderr(stderr).expect("should diagnose missing module");
        assert_eq!(diag.code, "E-MISSING-MODULE");
        assert!(diag.title.contains("fastapi"));
        assert!(diag.recommendation.unwrap().contains("fastapi"));
    }

    #[test]
    fn detects_disk_full() {
        let stderr = "tar: /home/ubuntu/workspace: Wrote only 4096 of 10240 bytes: No space left on device\n";
        let diag = diagnose_from_stderr(stderr).expect("should diagnose disk full");
        assert_eq!(diag.code, "E-ENOSPC");
    }

    #[test]
    fn detects_oom_from_dmesg_on_137() {
        let dmesg =
            "[Sat Sep 12 23:14:02] Out of memory: Killed process 3819 (python3) total-vm:17845MB\n";
        let diag = diagnose_from_dmesg_or_code(137, dmesg, "", Preset::Normal, "test-vm")
            .expect("should diagnose oom");
        assert_eq!(diag.code, "E137");
        assert!(diag.title.contains("Out-Of-Memory"));
        assert!(diag.recommendation.unwrap().contains("--preset ram"));
    }

    #[test]
    fn detects_ttl_from_spend_log_on_137() {
        let spend =
            "2026-09-12T23:00:00Z [qecs-guard] TTL reached (3600s >= 3600s). Powering off.\n";
        let diag = diagnose_from_dmesg_or_code(137, "", spend, Preset::Normal, "test-vm")
            .expect("should diagnose ttl");
        assert_eq!(diag.code, "E-TTL-EXPIRED");
        assert!(diag.command_suggestion.unwrap().contains("--ttl"));
    }

    #[test]
    fn detects_segfault_on_139() {
        let diag = diagnose_from_dmesg_or_code(139, "", "", Preset::Normal, "test-vm")
            .expect("should diagnose segfault");
        assert_eq!(diag.code, "E139");
        assert!(diag.title.contains("Segmentation Fault"));
    }
}
