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

/// Execute a job in detached mode. The remote job runs under nohup and logs to
/// `/home/ubuntu/job.log`, writing its exit code to `/home/ubuntu/job.exit` on completion.
pub fn execute_job_detached(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    remote_dir: &str,
    setup_cmds: &[String],
    run_cmd: &str,
) -> anyhow::Result<()> {
    let script = format!(
        "mkdir -p /run/qecs && touch /run/qecs/job.lock\ntrap 'rm -f /run/qecs/job.lock' EXIT\ncd '{remote_dir}' || exit 1\n{setup}\n{run_cmd}\necho $? > /home/ubuntu/job.exit\n",
        setup = setup_cmds.join("\n")
    );

    let setup_runner_cmd = format!(
        "cat << 'EOF' > /home/ubuntu/run-job.sh\n{script}EOF\nchmod +x /home/ubuntu/run-job.sh && nohup bash /home/ubuntu/run-job.sh > /home/ubuntu/job.log 2>&1 &"
    );

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
    fn quotes_shell_arguments_with_single_quotes() {
        assert_eq!(shlex_quote("echo hello"), "'echo hello'");
        assert_eq!(shlex_quote("echo 'hello'"), "'echo '\\''hello'\\'''");
    }
}
