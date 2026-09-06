//! `qecs wait` command: awaits completion of a detached job, pulls output, and cleans up.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use colored::Colorize;

use crate::cli::WaitArgs;
use crate::commands::shell::resolve_target_vm;
use crate::connect;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::iam;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::keys;
use crate::run::sync;

pub async fn cmd_wait(ctx: &Ctx, args: WaitArgs) -> anyhow::Result<()> {
    let (paths, _pub_key) = keys::ensure_keypair(None)?;
    let (store, mut vm) = resolve_target_vm(ctx, Some(&args.job)).await?;

    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

    let relay_cfg = ctx.config.relay.as_ref();
    let port = connect::resolve_connection_port_with_relay(&ip, vm.connect_port, relay_cfg).await?;
    if vm.connect_port != Some(port) {
        vm.connect_port = Some(port);
        let _ = store.upsert(vm.clone());
    }

    let proxy_cmd = relay_cfg.and_then(|r| r.proxy_command_for(&ip, port));

    let pb = crate::ui::spinner(format!(
        "Waiting for job `{}` on `{}`...",
        args.job, vm.name
    ));

    // Poll for remote job.exit file
    let poll_cmd = "[ -f /home/ubuntu/job.exit ] && cat /home/ubuntu/job.exit";
    let start = Instant::now();
    let timeout = Duration::from_secs(3600); // 1 hour max wait

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
            }
        }

        if start.elapsed() > timeout {
            pb.finish_and_clear();
            anyhow::bail!("timed out waiting for job to complete on `{}`", vm.name);
        }

        tokio::time::sleep(Duration::from_secs(3)).await;
    };

    pb.finish_and_clear();
    println!(
        "{}",
        format!("✓ Job completed with exit code {exit_code}.")
            .green()
            .bold()
    );

    // Pull output artifacts
    let local_out = PathBuf::from("./out");
    let pb = crate::ui::spinner("Checking for output artifacts...");
    match sync::download_output(
        &ip,
        port,
        &paths.private_key,
        proxy_cmd.as_deref(),
        "/home/ubuntu/workspace",
        &local_out,
    ) {
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
    let region = ctx.region();
    let client = ctx.signed();
    let project = iam::discover_project(&client, &region).await?;

    let job_id = ecs::delete_servers(&client, &region, &project.id, &[&vm.id]).await?;
    let _ = jobs::poll_job(
        &client,
        &region,
        &project.id,
        &job_id,
        &PollConfig::default(),
    )
    .await;
    let _ = store.remove(&vm.name);
    pb.finish_and_clear();
    println!(
        "{}",
        format!("✓ Destroyed ephemeral VM `{}`.", vm.name).green()
    );

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}
