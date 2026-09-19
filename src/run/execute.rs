//! Remote execution engine for `qecs run`.
//!
//! Supports streaming attached execution with exit code capture, as well as detached
//! background jobs logging to `/home/ubuntu/job.log`.

use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use colored::Colorize;

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
/// In interactive PTY mode, execution is wrapped in a resilient remote `tmux` session,
/// shielding the job from network drops and supporting in-flight detachment (`Ctrl+B d`).
/// In non-PTY mode, execution streams directly with zero multiplexer overhead.
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
    control_socket: Option<&Path>,
) -> anyhow::Result<i32> {
    let script = build_script(remote_dir, setup_cmds, run_cmd, trailing_args);

    let use_pty = pty.unwrap_or_else(|| {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    });

    if use_pty {
        let launch_cmd = crate::run::session::build_tmux_launch_script(
            crate::run::session::DEFAULT_SESSION_NAME,
            &script,
        );

        let stage_status = crate::connect::build_ssh_command_full(
            ip,
            port,
            key_path,
            proxy_command,
            Some(false),
            control_socket,
        )
        .arg(format!("bash -c {arg}", arg = shlex_quote(&launch_cmd)))
        .status()
        .context("launching resilient tmux session over SSH")?;

        if !stage_status.success() {
            anyhow::bail!("failed to initialize remote session on VM");
        }

        let attach_cmd =
            crate::run::session::build_tmux_attach_cmd(crate::run::session::DEFAULT_SESSION_NAME);

        let _ = crate::connect::build_ssh_command_full(
            ip,
            port,
            key_path,
            proxy_command,
            Some(true),
            control_socket,
        )
        .arg(&attach_cmd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

        let is_running = crate::run::session::is_session_running(
            ip,
            port,
            key_path,
            proxy_command,
            crate::run::session::DEFAULT_SESSION_NAME,
        )
        .unwrap_or(false);

        if is_running {
            println!(
                "{}",
                "✓ Session detached. Job continues running in background."
                    .yellow()
                    .bold()
            );
            println!("  Reattach: qecs attach");
            println!("  Logs:     qecs logs --follow");
            return Ok(0);
        }

        if let Ok(Some(exit_code)) =
            crate::run::session::read_remote_exit_code(ip, port, key_path, proxy_command)
        {
            return Ok(exit_code);
        }

        Ok(0)
    } else {
        let remote_cmd = format!("bash -c {arg}", arg = shlex_quote(&script));
        let status = crate::connect::build_ssh_command_full(
            ip,
            port,
            key_path,
            proxy_command,
            Some(false),
            control_socket,
        )
        .arg(&remote_cmd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("executing remote job over SSH")?;

        let code = status.code().unwrap_or(1);
        Ok(code)
    }
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
    post_success_cmd: Option<&str>,
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

    let post_cmd = if let Some(cmd) = post_success_cmd {
        format!("[ $RET -eq 0 ] && {cmd}\n")
    } else {
        String::new()
    };

    let script = format!(
        "mkdir -p /run/qecs && touch /run/qecs/job.lock\n\
         trap 'TRAP_RET=$?; [ -f /home/ubuntu/job.exit ] || echo $TRAP_RET > /home/ubuntu/job.exit; rm -f /run/qecs/job.lock' EXIT\n\
         exec 2> >(tee -a /run/qecs/job.stderr >&2)\n\
         set -a; [ -f /run/qecs/job.env ] && source /run/qecs/job.env; set +a\n\
         cd '{remote_dir}' || exit 1\n\
         {setup}\n\
         {full_run_cmd}\n\
         RET=$?\n\
         echo $RET > /home/ubuntu/job.exit\n\
         {post_cmd}",
        setup = setup_cmds.join("\n")
    );

    let b64_script = crate::cloudinit::base64_encode(script.as_bytes());
    format!(
        "printf '%s' {marker_val} > {marker}\n\
         printf '%s' '{b64_script}' | base64 -d > /home/ubuntu/run-job.sh\n\
         chmod +x /home/ubuntu/run-job.sh && nohup bash /home/ubuntu/run-job.sh > /home/ubuntu/job.log 2>&1 &",
        marker = DETACHED_OUTPUT_MARKER,
        marker_val = shlex_quote(output_subdir),
    )
}

/// Execute a job in detached mode. The remote job runs under nohup and logs to
/// `/home/ubuntu/job.log`, writing its exit code to `/home/ubuntu/job.exit` on completion.
#[allow(clippy::too_many_arguments)]
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
    post_success_cmd: Option<&str>,
) -> anyhow::Result<()> {
    execute_job_detached_full(
        ip,
        port,
        key_path,
        proxy_command,
        None,
        remote_dir,
        setup_cmds,
        run_cmd,
        trailing_args,
        output_subdir,
        post_success_cmd,
    )
}

/// Execute a job in detached mode with optional ControlMaster socket.
#[allow(clippy::too_many_arguments)]
pub fn execute_job_detached_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    control_socket: Option<&Path>,
    remote_dir: &str,
    setup_cmds: &[String],
    run_cmd: &str,
    trailing_args: &[String],
    output_subdir: &str,
    post_success_cmd: Option<&str>,
) -> anyhow::Result<()> {
    let setup_runner_cmd = build_detached_launcher(
        remote_dir,
        setup_cmds,
        run_cmd,
        trailing_args,
        output_subdir,
        post_success_cmd,
    );

    let status = crate::connect::build_ssh_command_full(
        ip,
        port,
        key_path,
        proxy_command,
        None,
        control_socket,
    )
    .arg(&setup_runner_cmd)
    .status()
    .context("spawning detached remote job over SSH")?;

    if !status.success() {
        anyhow::bail!("failed to launch detached job over SSH: exit status {status}");
    }

    Ok(())
}

/// Execute a raw command over SSH non-interactively (e.g. for cache uploads or housekeeping).
pub fn run_remote_command(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    command: &str,
) -> anyhow::Result<std::process::ExitStatus> {
    run_remote_command_full(ip, port, key_path, proxy_command, None, command)
}

/// Execute a raw command over SSH non-interactively with optional ControlMaster socket.
pub fn run_remote_command_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    control_socket: Option<&Path>,
    command: &str,
) -> anyhow::Result<std::process::ExitStatus> {
    crate::connect::build_ssh_command_full(ip, port, key_path, proxy_command, None, control_socket)
        .arg(command)
        .status()
        .context("running remote command over SSH")
}

/// Stage environment variables into `/run/qecs/job.env` on the remote VM.
pub fn stage_env_vars(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    env_vars: &[(String, String)],
) -> anyhow::Result<()> {
    stage_env_vars_full(ip, port, key_path, proxy_command, None, env_vars)
}

/// Stage environment variables with optional ControlMaster socket.
pub fn stage_env_vars_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    control_socket: Option<&Path>,
    env_vars: &[(String, String)],
) -> anyhow::Result<()> {
    if env_vars.is_empty() {
        return Ok(());
    }

    let mut env_content = String::new();
    for (k, v) in env_vars {
        if !is_valid_env_key(k) {
            anyhow::bail!(
                "invalid environment variable name `{k}`: must be a valid POSIX identifier (^[a-zA-Z_][a-zA-Z0-9_]*$)"
            );
        }
        env_content.push_str(&format!("{}={}\n", k, shlex_quote(v)));
    }

    let remote_cmd =
        "mkdir -p /run/qecs && cat > /run/qecs/job.env && chmod 0600 /run/qecs/job.env";

    let mut child = crate::connect::build_ssh_command_full(
        ip,
        port,
        key_path,
        proxy_command,
        None,
        control_socket,
    )
    .arg(remote_cmd)
    .stdin(Stdio::piped())
    .spawn()
    .context("spawning SSH to stage environment variables")?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(env_content.as_bytes())
            .context("writing environment variables to SSH stdin")?;
    }

    let status = child
        .wait()
        .context("waiting for environment staging over SSH")?;

    if !status.success() {
        anyhow::bail!("failed to stage environment variables: exit status {status}");
    }

    Ok(())
}

/// Validate whether an environment variable name is a valid POSIX identifier (`^[a-zA-Z_][a-zA-Z0-9_]*$`).
pub fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
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
            None,
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
        let launcher =
            build_detached_launcher("/home/ubuntu/workspace", &[], "true", &[], "out", None);
        assert!(launcher.contains("base64 -d > /home/ubuntu/run-job.sh"));
        assert!(launcher.contains("chmod +x /home/ubuntu/run-job.sh"));
        assert!(launcher.contains("nohup bash /home/ubuntu/run-job.sh"));
    }

    #[test]
    fn detached_launcher_includes_post_success_cmd() {
        let launcher = build_detached_launcher(
            "/home/ubuntu/workspace",
            &[],
            "true",
            &[],
            "out",
            Some("curl -X PUT https://example.com/cache"),
        );
        let start = launcher.find("| base64 -d").expect("pipe to base64 -d");
        let prefix = "printf '%s' '";
        let p_start = launcher[..start].rfind(prefix).expect("printf start") + prefix.len();
        let p_end = launcher[..start].rfind('\'').expect("quote end");
        let b64 = &launcher[p_start..p_end];

        let mut decoded = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;
        for &b in b64.as_bytes() {
            let val = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            buf = (buf << 6) | (val as u32);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                decoded.push((buf >> bits) as u8);
            }
        }
        let decoded_script = String::from_utf8(decoded).expect("valid utf8");
        assert!(decoded_script.contains("[ $RET -eq 0 ] && curl -X PUT https://example.com/cache"));
    }

    #[test]
    fn quotes_shell_arguments_with_single_quotes() {
        assert_eq!(shlex_quote("echo hello"), "'echo hello'");
        assert_eq!(shlex_quote("echo 'hello'"), "'echo '\\''hello'\\'''");
    }

    #[test]
    fn test_is_valid_env_key() {
        assert!(is_valid_env_key("FOO"));
        assert!(is_valid_env_key("_FOO_123"));
        assert!(is_valid_env_key("hf_token"));
        assert!(!is_valid_env_key(""));
        assert!(!is_valid_env_key("123FOO"));
        assert!(!is_valid_env_key("FOO-BAR"));
        assert!(!is_valid_env_key("FOO;reboot"));
        assert!(!is_valid_env_key("FOO\nBAR"));
        assert!(!is_valid_env_key("FOO$(id)"));
    }
}
