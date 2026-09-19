//! `qecs wait` command: awaits completion of a detached job, pulls output, and cleans up.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use colored::Colorize;

use crate::cli::WaitArgs;
use crate::commands::shell::resolve_target_vm;
use crate::connect;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::endpoints::Service;
use crate::hwc::iam;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::keys;
use crate::run::sync;
use crate::telemetry::TelemetryExt;

pub async fn cmd_wait(ctx: &Ctx, args: WaitArgs) -> anyhow::Result<()> {
    let (paths, _pub_key) = keys::ensure_keypair(None)?;
    let (store, mut vm) = resolve_target_vm(ctx, Some(&args.job)).await?;

    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

    let relay = connect::Relay::from_config(ctx.config.relay.as_ref())?;
    let port =
        connect::resolve_connection_port_with_relay(&ip, vm.connect_port, Some(&relay)).await?;
    if vm.connect_port != Some(port) {
        vm.connect_port = Some(port);
        let _ = store.upsert(vm.clone());
    }

    let proxy_cmd = relay.proxy_command(&ip, port);

    let pb = crate::ui::spinner(format!(
        "Waiting for job `{}` on `{}`...",
        args.job, vm.name
    ));

    // Poll for remote job.exit file. A detached job cannot outlive its VM's TTL,
    // so bound the wait by the remaining TTL rather than a flat hour that would
    // abandon a long training run. Refresh the job lock on every poll so the
    // on-box guard never idle-powers-off in the gap between the job's own lock
    // trap firing and this command pulling the artifacts.
    let poll_cmd = "mkdir -p /run/qecs && touch /run/qecs/job.lock; \
                    if [ -f /home/ubuntu/job.exit ]; then \
                        cat /home/ubuntu/job.exit; \
                    elif pgrep -f 'run-job.sh' >/dev/null 2>&1; then \
                        echo 'RUNNING'; \
                    else \
                        echo 'DEAD'; \
                    fi";
    let start = Instant::now();
    let timeout = Duration::from_secs(crate::lifecycle::wait_deadline_secs(
        &vm.created_at,
        vm.ttl_secs,
    ));
    let mut interval = Duration::from_secs(3);
    let mut dead_checks = 0;

    let exit_code = loop {
        let output =
            connect::build_ssh_command(&ip, port, &paths.private_key, proxy_cmd.as_deref())
                .arg(poll_cmd)
                .output();

        if let Ok(out) = output
            && out.status.success()
            && !out.stdout.is_empty()
        {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(code) = text.parse::<i32>() {
                break code;
            } else if text == "DEAD" {
                dead_checks += 1;
                // Require 2 consecutive checks to avoid any race condition at startup
                if dead_checks >= 2 {
                    pb.finish_and_clear();
                    eprintln!(
                        "{}",
                        format!(
                            "Warning: Job process on `{}` terminated unexpectedly without writing exit code (possible OOM killer SIGKILL or kernel crash). Check `qecs logs {}`.",
                            vm.name, vm.name
                        )
                        .yellow()
                        .bold()
                    );
                    break 137;
                }
            } else {
                dead_checks = 0;
            }
        }

        if start.elapsed() > timeout {
            pb.finish_and_clear();
            anyhow::bail!(
                "timed out waiting for job on `{}` after {}s (VM TTL). Check `qecs logs {}`.",
                vm.name,
                timeout.as_secs(),
                vm.name
            );
        }

        tokio::time::sleep(interval).await;
        interval = (interval * 2).min(Duration::from_secs(10));
    };

    pb.finish_and_clear();
    println!(
        "{}",
        format!("✓ Job completed with exit code {exit_code}.")
            .green()
            .bold()
    );

    // Recover the recipe's output dir from the marker the detached launcher dropped
    // (falls back to "out" for older jobs or a missing marker).
    let output_subdir = {
        let marker = crate::run::execute::DETACHED_OUTPUT_MARKER;
        let out = connect::build_ssh_command(&ip, port, &paths.private_key, proxy_cmd.as_deref())
            .arg(format!("cat {marker} 2>/dev/null || true"))
            .output();
        let raw = out
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "out".to_string());
        if raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/')
            && !raw.contains("..")
        {
            raw
        } else {
            "out".to_string()
        }
    };

    // Pull output artifacts
    let local_out = PathBuf::from("./out");
    let pb = crate::ui::spinner("Checking for output artifacts...");
    let dl_res = ctx.telemetry.phase_sync("output-download", || {
        sync::download_output(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            "/home/ubuntu/workspace",
            &output_subdir,
            &local_out,
        )
    });
    match dl_res {
        Ok(true) => {
            pb.finish_and_clear();
            println!(
                "{}",
                format!("✓ Retrieved artifacts to `{}`.", local_out.display()).green()
            );
        }
        Ok(false) => {
            pb.finish_and_clear();
        }
        Err(e) => {
            pb.finish_and_clear();
            eprintln!(
                "{}",
                format!("Warning: failed to download output: {e}").yellow()
            );
        }
    }

    // Auto-destroy VM
    let pb = crate::ui::spinner(format!("Tearing down VM `{}`...", vm.name));
    ctx.telemetry
        .phase_try("destroy", async {
            let region = ctx.region();
            let client = ctx.signed();
            let project = iam::discover_project(&client, &region).await?;

            let job_id = ecs::delete_servers(&client, &region, &project.id, &[&vm.id]).await?;
            let _ = jobs::poll_job(
                &client,
                Service::Ecs,
                &region,
                &project.id,
                &job_id,
                &PollConfig::default(),
                ctx.telemetry.as_ref(),
            )
            .await;
            let _ = store.remove(&vm.name);
            if let Some(ip) = &vm.eip {
                let _ = keys::remove_known_host(ip);
            }
            if let Some(ip) = &vm.private_ip {
                let _ = keys::remove_known_host(ip);
            }
            anyhow::Ok(())
        })
        .await?;
    pb.finish_and_clear();
    println!(
        "{}",
        format!("✓ Destroyed ephemeral VM `{}`.", vm.name).green()
    );

    if exit_code != 0 {
        return Err(crate::error::ExitCode(exit_code).into());
    }

    Ok(())
}
