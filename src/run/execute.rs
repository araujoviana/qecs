//! Remote execution engine for `qecs run`.
//!
//! Supports streaming attached execution with exit code capture, as well as detached
//! background jobs logging to `/home/ubuntu/job.log`.

use std::path::Path;
use std::process::Stdio;

use anyhow::Context;

/// Build the composite remote script to execute in `remote_dir`.
pub fn build_script(
    remote_dir: &str,
    setup_cmds: &[String],
    run_cmd: &str,
    trailing_args: &[String],
) -> String {
    let mut script = String::new();
    script.push_str("mkdir -p /run/qecs && touch /run/qecs/job.lock\n");
    script.push_str("trap 'rm -f /run/qecs/job.lock' EXIT\n");
    script.push_str("exec 2> >(tee -a /run/qecs/job.stderr >&2)\n");
    script.push_str("set -a; [ -f /run/qecs/job.env ] && source /run/qecs/job.env; set +a\n");
    script.push_str(&format!("cd '{remote_dir}' || exit 1\n"));

    for cmd in setup_cmds {
        script.push_str(cmd);
        script.push('\n');
    }

    let full_run_cmd = if trailing_args.is_empty() {
        run_cmd.to_string()
    } else {
        let quoted_args = trailing_args
            .iter()
            .map(|a| shlex_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{run_cmd} {quoted_args}")
    };

    script.push_str(&full_run_cmd);
    script.push('\n');
    script
}

/// Execute a job in attached mode, streaming stdout and stderr live to the local terminal.
/// Returns the remote process exit code.
#[allow(clippy::too_many_arguments)]
pub fn execute_job_attached(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    remote_dir: &str,
    setup_cmds: &[String],
    run_cmd: &str,
    trailing_args: &[String],
    pty: Option<bool>,
) -> anyhow::Result<i32> {
    let script = build_script(remote_dir, setup_cmds, run_cmd, trailing_args);
    let remote_cmd = format!("bash -c {arg}", arg = shlex_quote(&script));

    let use_pty = pty.unwrap_or_else(|| {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    });

    let status =
        crate::connect::build_ssh_command_ext(ip, port, key_path, proxy_command, Some(use_pty))
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
    trailing_args: &[String],
    output_subdir: &str,
) -> String {
    let full_run_cmd = if trailing_args.is_empty() {
        run_cmd.to_string()
    } else {
        let quoted_args = trailing_args
            .iter()
            .map(|a| shlex_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{run_cmd} {quoted_args}")
    };

    let script = format!(
        "mkdir -p /run/qecs && touch /run/qecs/job.lock\n\
         trap 'rm -f /run/qecs/job.lock' EXIT\n\
         exec 2> >(tee -a /run/qecs/job.stderr >&2)\n\
         set -a; [ -f /run/qecs/job.env ] && source /run/qecs/job.env; set +a\n\
         cd '{remote_dir}' || exit 1\n\
         {setup}\n\
         {full_run_cmd}\n\
         echo $? > /home/ubuntu/job.exit\n",
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
    trailing_args: &[String],
    output_subdir: &str,
) -> anyhow::Result<()> {
    let setup_runner_cmd = build_detached_launcher(
        remote_dir,
        setup_cmds,
        run_cmd,
        trailing_args,
        output_subdir,
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

/// Stage environment variables into `/run/qecs/job.env` on the remote VM.
pub fn stage_env_vars(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    env_vars: &[(String, String)],
) -> anyhow::Result<()> {
    if env_vars.is_empty() {
        return Ok(());
    }

    let mut env_content = String::new();
    for (k, v) in env_vars {
        env_content.push_str(&format!("{}={}\n", k, shlex_quote(v)));
    }

    let cmd = format!(
        "mkdir -p /run/qecs && cat << 'EOF' > /run/qecs/job.env\n{env_content}EOF\nchmod 0600 /run/qecs/job.env"
    );

    let status = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&cmd)
        .status()
        .context("staging environment variables over SSH")?;

    if !status.success() {
        anyhow::bail!("failed to stage environment variables: exit status {status}");
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
        let trailing = vec!["--epochs".into(), "50".into()];
        let script = build_script(
            "/home/ubuntu/workspace",
            &setup,
            "python3 main.py",
            &trailing,
        );

        assert!(script.contains("mkdir -p /run/qecs && touch /run/qecs/job.lock\n"));
        assert!(script.contains("trap 'rm -f /run/qecs/job.lock' EXIT\n"));
        assert!(script.contains("exec 2> >(tee -a /run/qecs/job.stderr >&2)\n"));
        assert!(
            script
                .contains("set -a; [ -f /run/qecs/job.env ] && source /run/qecs/job.env; set +a\n")
        );
        assert!(script.contains("cd '/home/ubuntu/workspace' || exit 1\n"));
        assert!(script.contains("pip install -r requirements.txt\n"));
        assert!(script.contains("export FOO=bar\n"));
        assert!(script.ends_with("python3 main.py '--epochs' '50'\n"));
    }

    #[test]
    fn detached_launcher_records_output_dir_marker_before_nohup() {
        let launcher = build_detached_launcher(
            "/home/ubuntu/workspace",
            &["pip install -r requirements.txt".into()],
            "python3 main.py",
            &[],
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
        let launcher = build_detached_launcher("/home/ubuntu/workspace", &[], "true", &[], "out");
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
