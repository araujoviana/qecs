//! Remote execution engine for `qecs run`.
//!
//! Supports streaming attached execution with exit code capture, as well as detached
//! background jobs logging to `/home/ubuntu/job.log`.

use std::path::Path;
use std::process::Stdio;

use anyhow::Context;

/// Build the composite remote script to execute in `remote_dir`.
pub fn build_script(remote_dir: &str, setup_cmds: &[String], run_cmd: &str) -> String {
    let mut script = String::new();
    script.push_str("mkdir -p /run/qecs && touch /run/qecs/job.lock\n");
    script.push_str("trap 'rm -f /run/qecs/job.lock' EXIT\n");
    script.push_str(&format!("cd '{remote_dir}' || exit 1\n"));

    for cmd in setup_cmds {
        script.push_str(cmd);
        script.push('\n');
    }

    script.push_str(run_cmd);
    script.push('\n');
    script
}

/// Execute a job in attached mode, streaming stdout and stderr live to the local terminal.
/// Returns the remote process exit code.
pub fn execute_job_attached(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    remote_dir: &str,
    setup_cmds: &[String],
    run_cmd: &str,
) -> anyhow::Result<i32> {
    let script = build_script(remote_dir, setup_cmds, run_cmd);
    let remote_cmd = format!("bash -c {arg}", arg = shlex_quote(&script));

    let status = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&remote_cmd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("executing remote job over SSH")?;

    let code = status.code().unwrap_or(1);
    Ok(code)
}

/// Marker file the detached launcher drops so `qecs wait` knows which directory
/// to pull artifacts from (the recipe's `output_dir`, which is not otherwise
/// persisted for a detached job).
pub const DETACHED_OUTPUT_MARKER: &str = "/home/ubuntu/.qecs-output-dir";

/// Build the remote command that stages `run-job.sh`, records the output dir
/// marker, and launches the job under `nohup`.
pub fn build_detached_launcher(
    remote_dir: &str,
    setup_cmds: &[String],
    run_cmd: &str,
    output_subdir: &str,
) -> String {
    let script = format!(
        "mkdir -p /run/qecs && touch /run/qecs/job.lock\ntrap 'rm -f /run/qecs/job.lock' EXIT\ncd '{remote_dir}' || exit 1\n{setup}\n{run_cmd}\necho $? > /home/ubuntu/job.exit\n",
        setup = setup_cmds.join("\n")
    );

    format!(
        "printf '%s' {marker_val} > {marker}\n\
         cat << 'EOF' > /home/ubuntu/run-job.sh\n{script}EOF\n\
         chmod +x /home/ubuntu/run-job.sh && nohup bash /home/ubuntu/run-job.sh > /home/ubuntu/job.log 2>&1 &",
        marker = DETACHED_OUTPUT_MARKER,
        marker_val = shlex_quote(output_subdir),
    )
}

/// Execute a job in detached mode. The remote job runs under nohup and logs to
/// `/home/ubuntu/job.log`, writing its exit code to `/home/ubuntu/job.exit` on completion.
#[allow(clippy::too_many_arguments)] // SSH target + job spec; a RemoteTarget bundle is a later cleanup
pub fn execute_job_detached(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    remote_dir: &str,
    setup_cmds: &[String],
    run_cmd: &str,
    output_subdir: &str,
) -> anyhow::Result<()> {
    let setup_runner_cmd = build_detached_launcher(remote_dir, setup_cmds, run_cmd, output_subdir);

    let status = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&setup_runner_cmd)
        .status()
        .context("spawning detached remote job over SSH")?;

    if !status.success() {
        anyhow::bail!("failed to launch detached job over SSH: exit status {status}");
    }

    Ok(())
}

/// Quote a string safely for POSIX shell argument passing.
fn shlex_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_composite_script() {
        let setup = vec![
            "pip install -r requirements.txt".into(),
            "export FOO=bar".into(),
        ];
        let script = build_script("/home/ubuntu/workspace", &setup, "python3 main.py");

        assert!(script.contains("mkdir -p /run/qecs && touch /run/qecs/job.lock\n"));
        assert!(script.contains("trap 'rm -f /run/qecs/job.lock' EXIT\n"));
        assert!(script.contains("cd '/home/ubuntu/workspace' || exit 1\n"));
        assert!(script.contains("pip install -r requirements.txt\n"));
        assert!(script.contains("export FOO=bar\n"));
        assert!(script.ends_with("python3 main.py\n"));
    }

    #[test]
    fn detached_launcher_records_output_dir_marker_before_nohup() {
        let launcher = build_detached_launcher(
            "/home/ubuntu/workspace",
            &["pip install -r requirements.txt".into()],
            "python3 main.py",
            "results",
        );
        // wait needs to learn the recipe's output_dir; it is written to the marker
        // file before the job is backgrounded.
        assert!(launcher.contains(DETACHED_OUTPUT_MARKER));
        assert!(launcher.contains("'results'"));
        let marker_pos = launcher.find(DETACHED_OUTPUT_MARKER).unwrap();
        let nohup_pos = launcher.find("nohup").unwrap();
        assert!(
            marker_pos < nohup_pos,
            "marker must be written before nohup"
        );
    }

    #[test]
    fn detached_launcher_still_stages_run_job_and_writes_exit_code() {
        let launcher = build_detached_launcher("/home/ubuntu/workspace", &[], "true", "out");
        assert!(launcher.contains("cat << 'EOF' > /home/ubuntu/run-job.sh"));
        assert!(launcher.contains("echo $? > /home/ubuntu/job.exit"));
        assert!(launcher.contains("touch /run/qecs/job.lock"));
    }

    #[test]
    fn quotes_shell_arguments_with_single_quotes() {
        assert_eq!(shlex_quote("echo hello"), "'echo hello'");
        assert_eq!(shlex_quote("echo 'hello'"), "'echo '\\''hello'\\'''");
    }
}
