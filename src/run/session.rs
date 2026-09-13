//! Session resilience engine: wrapping interactive jobs in remote `tmux` sessions
//! for network drop immunity, in-flight detach (`Ctrl+B d`), and reattachment.

use std::path::Path;

use anyhow::Context;

pub const DEFAULT_SESSION_NAME: &str = "qecs-job";
pub const JOB_EXIT_FILE: &str = "/home/ubuntu/job.exit";
pub const JOB_SCRIPT_FILE: &str = "/home/ubuntu/run-job.sh";

/// Build the remote command that ensures `tmux` is available, creates the session,
/// and launches the staged job script in the background before attaching.
pub fn build_tmux_launch_script(session_name: &str, script_content: &str) -> String {
    format!(
        "command -v tmux >/dev/null 2>&1 || (sudo apt-get update -qq && sudo apt-get install -y -qq tmux)\n\
         cat << 'EOF' > {script_file}\n{script_content}EOF\n\
         chmod +x {script_file}\n\
         tmux kill-session -t '{session}' 2>/dev/null || true\n\
         tmux new-session -d -s '{session}' -x 200 -y 50 'bash {script_file}; echo $? > {exit_file}'\n\
         tmux set-option -t '{session}' mouse on 2>/dev/null || true\n\
         tmux set-option -t '{session}' status off 2>/dev/null || true",
        script_file = JOB_SCRIPT_FILE,
        exit_file = JOB_EXIT_FILE,
        session = session_name,
    )
}

/// Build the SSH command string to attach to an existing `tmux` session.
pub fn build_tmux_attach_cmd(session_name: &str) -> String {
    format!("tmux attach-session -t '{session_name}'")
}

/// Build a smart reattach command that reattaches if running or displays exit status if finished.
pub fn build_smart_attach_cmd(session_name: &str) -> String {
    format!(
        "if tmux has-session -t '{session}' 2>/dev/null; then \
             tmux attach-session -t '{session}'; \
         else \
             if [ -f {exit_file} ]; then \
                 echo \"Job on '{session}' already finished with exit code $(cat {exit_file}).\"; \
             else \
                 echo \"No active job session found for '{session}'.\"; \
             fi; \
         fi",
        session = session_name,
        exit_file = JOB_EXIT_FILE,
    )
}

/// Check if the remote `tmux` session is still running.
pub fn is_session_running(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    session_name: &str,
) -> anyhow::Result<bool> {
    let check_cmd = format!("tmux has-session -t '{session_name}' 2>/dev/null");
    let status = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&check_cmd)
        .status()
        .context("checking remote tmux session")?;

    Ok(status.success())
}

/// Read the exit code recorded in `/home/ubuntu/job.exit` if the job has completed.
pub fn read_remote_exit_code(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
) -> anyhow::Result<Option<i32>> {
    let read_cmd = format!("[ -f {JOB_EXIT_FILE} ] && cat {JOB_EXIT_FILE} || exit 42");
    let output = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&read_cmd)
        .output()
        .context("reading remote job exit file")?;

    if output.status.success() {
        let txt = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Ok(code) = txt.parse::<i32>() {
            return Ok(Some(code));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_script_contains_tmux_and_setup() {
        let script = build_tmux_launch_script("test-session", "python3 train.py\n");
        assert!(script.contains("command -v tmux"));
        assert!(script.contains("test-session"));
        assert!(script.contains(JOB_SCRIPT_FILE));
        assert!(script.contains(JOB_EXIT_FILE));
        assert!(script.contains("tmux set-option -t 'test-session' status off"));
    }

    #[test]
    fn attach_cmd_references_session() {
        let cmd = build_tmux_attach_cmd("my-job");
        assert_eq!(cmd, "tmux attach-session -t 'my-job'");
    }

    #[test]
    fn smart_attach_handles_both_running_and_finished() {
        let cmd = build_smart_attach_cmd("demo");
        assert!(cmd.contains("tmux has-session -t 'demo'"));
        assert!(cmd.contains("tmux attach-session -t 'demo'"));
        assert!(cmd.contains(JOB_EXIT_FILE));
    }
}
