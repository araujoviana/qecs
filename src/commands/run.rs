//! `qecs run` command: provisions a VM, ships workdir, executes recipe, pulls artifacts, and tears down.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use colored::Colorize;

use crate::cli::RunArgs;
use crate::connect;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::iam;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::keys;
use crate::presets::Preset;
use crate::provision::{self, ProvisionOptions};
use crate::run::detect::{RunRecipe, detect_recipe};
use crate::run::execute;
use crate::run::sync;
use crate::state::StateStore;

pub async fn cmd_run(ctx: &Ctx, args: RunArgs) -> anyhow::Result<()> {
    // 1. Resolve workdir & detect recipe
    let target_path = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let recipe = detect_recipe(&target_path)?;

    // 2. Handle --dry-run
    if args.dry_run {
        print_dry_run_summary(&recipe, &args);
        return Ok(());
    }

    // 3. Resolve preset & flavor
    let final_preset = if let Some(ref p) = args.preset {
        p.parse::<Preset>()?
    } else {
        recipe.preset
    };

    let (paths, _pub_key) = keys::ensure_keypair(None)?;

    // 4. Provision VM
    let opts = ProvisionOptions {
        preset: Some(final_preset.to_string()),
        flavor: args.flavor.clone(),
        name: None,
        ttl: args.ttl.clone(),
        dry_run: false,
    };

    let vm = provision::provision_vm(ctx, &opts)
        .await?
        .ok_or_else(|| anyhow::anyhow!("provisioning did not return a VM record"))?;

    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

    // 5. Await SSH readiness
    let pb = crate::ui::spinner(format!(
        "Waiting for SSH readiness on `{}` ({ip})...",
        vm.name
    ));
    let port = wait_for_ssh_ready(&ip, Duration::from_secs(90)).await?;
    pb.finish_and_clear();

    // Cache port in state store
    let store = StateStore::open()?;
    let mut updated_vm = vm.clone();
    updated_vm.connect_port = Some(port);
    let _ = store.upsert(updated_vm.clone());

    // 6. Pack & upload workdir
    let pb = crate::ui::spinner("Packing and uploading workspace...");
    let tarball = sync::pack_directory(&recipe.workdir).context("packing workspace")?;
    sync::upload_workdir(
        &ip,
        port,
        &paths.private_key,
        &tarball,
        "/home/ubuntu/workspace",
    )
    .context("uploading workspace to VM")?;
    pb.finish_and_clear();

    // 7. Await GPU readiness if GPU preset is active
    if final_preset.needs_gpu() {
        let pb = crate::ui::spinner(
            "Waiting for NVIDIA GPU driver and container toolkit installation (~4-8m)...",
        );
        wait_for_gpu_ready(&ip, port, &paths.private_key, Duration::from_secs(900)).await?;
        pb.finish_and_clear();
        println!(
            "{}",
            "✓ NVIDIA GPU driver and container toolkit ready."
                .green()
                .bold()
        );
    }

    // 8. Execution: Detached vs Attached
    if args.detach {
        execute::execute_job_detached(
            &ip,
            port,
            &paths.private_key,
            "/home/ubuntu/workspace",
            &recipe.setup_commands,
            &recipe.run_command,
        )?;

        updated_vm.job = Some(vm.name.clone());
        let _ = store.upsert(updated_vm);

        println!(
            "{}",
            format!("✓ Job launched in background on `{}`.", vm.name)
                .green()
                .bold()
        );
        println!("  Stream logs:  qecs logs {} --follow", vm.name);
        println!("  Await result: qecs wait {}", vm.name);
        return Ok(());
    }

    // Attached execution
    println!(
        "{}",
        format!(
            "▶ Running `{}` ({}) on `{}`...",
            recipe.run_command, recipe.name, vm.name
        )
        .cyan()
        .bold()
    );

    let exit_code = execute::execute_job_attached(
        &ip,
        port,
        &paths.private_key,
        "/home/ubuntu/workspace",
        &recipe.setup_commands,
        &recipe.run_command,
    )?;

    // 8. Pull output artifacts
    let local_output_dir = args
        .output
        .unwrap_or_else(|| recipe.workdir.join(&recipe.output_dir));

    let pb = crate::ui::spinner("Checking for output artifacts...");
    match sync::download_output(
        &ip,
        port,
        &paths.private_key,
        "/home/ubuntu/workspace",
        &local_output_dir,
    ) {
        Ok(true) => {
            pb.finish_and_clear();
            println!(
                "{}",
                format!("✓ Retrieved artifacts to `{}`.", local_output_dir.display()).green()
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

    // 9. Auto-destruction
    if args.keep {
        println!(
            "{}",
            format!("Note: VM `{}` kept alive (--keep specified).", vm.name).dimmed()
        );
    } else {
        let pb = crate::ui::spinner(format!("Tearing down VM `{}`...", vm.name));
        destroy_vm(ctx, &vm.id, &vm.name).await?;
        pb.finish_and_clear();
        println!(
            "{}",
            format!("✓ Destroyed ephemeral VM `{}`.", vm.name).green()
        );
    }

    if exit_code != 0 {
        std::process::exit(exit_code);
    }

    Ok(())
}

fn print_dry_run_summary(recipe: &RunRecipe, args: &RunArgs) {
    println!("{}", "=== qecs run Dry-Run Plan ===".green().bold());
    println!("  Workdir:        {}", recipe.workdir.display());
    println!("  Detector:       {}", recipe.name);
    let preset_str = args
        .preset
        .as_deref()
        .unwrap_or_else(|| recipe.preset.as_str());
    println!("  Preset:         {}", preset_str);
    if let Some(ref flavor) = args.flavor {
        println!("  Flavor:         {}", flavor);
    }
    if !recipe.setup_commands.is_empty() {
        println!("  Setup commands:");
        for cmd in &recipe.setup_commands {
            println!("    - {}", cmd);
        }
    }
    println!("  Run command:    {}", recipe.run_command);
    println!("  Output dir:     {}", recipe.output_dir);
    println!("  Detached:       {}", args.detach);
    println!("  Keep VM:        {}", args.keep);
}

async fn wait_for_ssh_ready(ip: &str, timeout: Duration) -> anyhow::Result<u16> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(port) = connect::resolve_connection_port(ip, None).await {
            return Ok(port);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    anyhow::bail!("timed out waiting for SSH to become ready on {ip}")
}

async fn destroy_vm(ctx: &Ctx, server_id: &str, name: &str) -> anyhow::Result<()> {
    let region = ctx.region();
    let client = ctx.signed();
    let project = iam::discover_project(&client, &region).await?;

    let job_id = ecs::delete_servers(&client, &region, &project.id, &[server_id]).await?;
    let _ = jobs::poll_job(
        &client,
        &region,
        &project.id,
        &job_id,
        &PollConfig::default(),
    )
    .await;

    let store = StateStore::open()?;
    let _ = store.remove(name);
    Ok(())
}

async fn wait_for_gpu_ready(
    ip: &str,
    port: u16,
    key_path: &Path,
    timeout: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let poll_cmd = "[ -f /run/qecs/gpu.status ] && cat /run/qecs/gpu.status";

    while start.elapsed() < timeout {
        let output = std::process::Command::new("ssh")
            .args([
                "-i",
                key_path.to_str().unwrap(),
                "-p",
                &port.to_string(),
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "LogLevel=ERROR",
                &format!("ubuntu@{ip}"),
                poll_cmd,
            ])
            .output();

        if let Ok(out) = output
            && out.status.success()
        {
            let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if status == "READY" {
                return Ok(());
            } else if status == "FAILED" {
                let log_cmd = "tail -n 25 /var/log/qecs-gpu-setup.log 2>/dev/null || true";
                let log_output = std::process::Command::new("ssh")
                    .args([
                        "-i",
                        key_path.to_str().unwrap(),
                        "-p",
                        &port.to_string(),
                        "-o",
                        "StrictHostKeyChecking=accept-new",
                        "-o",
                        "IdentitiesOnly=yes",
                        "-o",
                        "LogLevel=ERROR",
                        &format!("ubuntu@{ip}"),
                        log_cmd,
                    ])
                    .output();
                let details = if let Ok(l) = log_output {
                    String::from_utf8_lossy(&l.stdout).trim().to_string()
                } else {
                    String::new()
                };
                anyhow::bail!(
                    "NVIDIA GPU driver setup failed on remote VM `{ip}`.\n\
                     Log snippet (/var/log/qecs-gpu-setup.log):\n{details}"
                );
            }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    anyhow::bail!(
        "Timed out waiting for GPU driver installation after {} seconds on `{ip}`.",
        timeout.as_secs()
    );
}
