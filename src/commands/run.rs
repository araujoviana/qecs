//! `qecs run` command: provisions a VM, ships workdir, executes recipe, pulls artifacts, and tears down.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use colored::Colorize;

use crate::cli::RunArgs;
use crate::connect;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::endpoints::Service;
use crate::hwc::iam;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::keys;
use crate::provision::{self, ProvisionOptions};
use crate::run::detect::{RunRecipe, detect_recipe};
use crate::run::execute;
use crate::run::sync;
use crate::state::StateStore;
use crate::telemetry::TelemetryExt;

pub async fn cmd_run(ctx: &Ctx, args: RunArgs) -> anyhow::Result<()> {
    // 1. Resolve workdir & detect recipe
    let target_path = args.path.clone().unwrap_or_else(|| PathBuf::from("."));
    let recipe = detect_recipe(&target_path)?;

    // 2. Resolve ad-hoc command or detected run_command
    let effective_run_cmd = args
        .command
        .clone()
        .unwrap_or_else(|| recipe.run_command.clone());

    // 3. Collect environment variables
    let env_vars = collect_env_vars(&args)?;

    // 4. Handle --dry-run
    if args.dry_run {
        print_dry_run_summary(&recipe, &args, &effective_run_cmd, &env_vars);
        return Ok(());
    }

    // 5. Resolve preset & flavor
    let final_preset = args.preset.unwrap_or(recipe.preset);

    let (paths, _pub_key) = keys::ensure_keypair(None)?;

    // 6. Provision VM
    let opts = ProvisionOptions {
        preset: Some(final_preset),
        flavor: args.flavor.clone(),
        name: None,
        ttl: args.ttl.clone(),
        dry_run: false,
        no_baked_image: args.no_baked_image,
    };

    let vm = provision::provision_vm(ctx, &opts)
        .await?
        .ok_or_else(|| anyhow::anyhow!("provisioning did not return a VM record"))?;

    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

    let _ = keys::remove_known_host(&ip);

    // 7. Await SSH readiness
    let pb = crate::ui::spinner(format!(
        "Waiting for SSH readiness on `{}` ({ip})...",
        vm.name
    ));
    let relay = crate::connect::Relay::from_config(ctx.config.relay.as_ref())?;
    let port = ctx
        .telemetry
        .phase_try(
            "ssh-probe",
            connect::wait_for_ssh_ready(&ip, Duration::from_secs(90), &relay),
        )
        .await?;
    pb.finish_and_clear();

    // Cache port in state store
    let store = StateStore::open()?;
    let mut updated_vm = vm.clone();
    updated_vm.connect_port = Some(port);
    let _ = store.upsert(updated_vm.clone());

    let proxy_cmd = relay.proxy_command(&ip, port);

    // 8. Stream workspace directly to VM (constant memory)
    let pb = crate::ui::spinner("Streaming workspace to VM...");
    ctx.telemetry.phase_sync("workdir-stream", || {
        sync::stream_workdir(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            &recipe.workdir,
            "/home/ubuntu/workspace",
        )
        .context("streaming workspace to VM")
    })?;
    pb.finish_and_clear();

    // 9. Stage environment variables if present
    if !env_vars.is_empty() {
        execute::stage_env_vars(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            &env_vars,
        )?;
    }

    // 10. Await GPU readiness if GPU preset is active
    if final_preset.needs_gpu() {
        let pb = crate::ui::spinner(
            "Waiting for NVIDIA GPU driver and container toolkit installation (~4-8m)...",
        );
        ctx.telemetry
            .phase_try(
                "gpu-wait",
                wait_for_gpu_ready(
                    &ip,
                    port,
                    &paths.private_key,
                    proxy_cmd.as_deref(),
                    Duration::from_secs(900),
                ),
            )
            .await?;
        pb.finish_and_clear();
        println!(
            "{}",
            "✓ NVIDIA GPU driver and container toolkit ready."
                .green()
                .bold()
        );
    }

    // 10b. Prepare OBS dependency caching
    let mut effective_setup_cmds = recipe.setup_commands.clone();
    let cache_key = if !args.no_cache {
        crate::run::detect::compute_cache_key(&recipe.workdir)
    } else {
        None
    };

    let cache_put_url = if let Some(ref key) = cache_key {
        let client = ctx.signed();
        let region = ctx.region();
        let project_id_res = ctx
            .telemetry
            .phase_try("iam-project", iam::discover_project(&client, &region))
            .await;

        if let Ok(project) = project_id_res {
            let bucket = crate::hwc::obs::cache_bucket_name(&region, &project.id);
            let _ = crate::hwc::obs::ensure_cache_bucket(&client, &region, &bucket).await;
            let object_key = format!("caches/{key}.tar.gz");

            let get_url = crate::hwc::obs::generate_presigned_url(
                client.creds(),
                &region,
                &bucket,
                &object_key,
                "GET",
                900,
            );
            let restore_cmd = format!(
                "curl -sf -o /tmp/qecs-cache.tar.gz \"{get_url}\" && tar -xzf /tmp/qecs-cache.tar.gz -C /home/ubuntu 2>/dev/null && rm -f /tmp/qecs-cache.tar.gz || true"
            );
            effective_setup_cmds.insert(0, restore_cmd);

            let put_url = crate::hwc::obs::generate_presigned_url(
                client.creds(),
                &region,
                &bucket,
                &object_key,
                "PUT",
                1800,
            );
            Some(put_url)
        } else {
            None
        }
    } else {
        None
    };

    // 11. Execution: Detached vs Attached
    let artifact_spec = args.artifacts.as_deref().unwrap_or(&recipe.output_dir);

    if args.detach {
        let cache_upload_cmd = cache_put_url.as_ref().map(|put_url| {
            format!(
                "if [ -d /home/ubuntu/.cache ] || [ -d /home/ubuntu/.cargo ]; then \
                    tar -czf - -C /home/ubuntu $([ -d /home/ubuntu/.cache ] && echo .cache) $([ -d /home/ubuntu/.cargo ] && echo .cargo) 2>/dev/null | curl -sf -X PUT --upload-file - \"{put_url}\" || true; \
                fi"
            )
        });

        ctx.telemetry.phase_sync("job-exec", || {
            execute::execute_job_detached(
                &ip,
                port,
                &paths.private_key,
                proxy_cmd.as_deref(),
                "/home/ubuntu/workspace",
                &effective_setup_cmds,
                &effective_run_cmd,
                &args.args,
                artifact_spec,
                cache_upload_cmd.as_deref(),
            )
        })?;

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
            effective_run_cmd, recipe.name, vm.name
        )
        .cyan()
        .bold()
    );

    let pty_opt = if args.pty {
        Some(true)
    } else if args.no_pty {
        Some(false)
    } else {
        None
    };

    let exit_code = ctx.telemetry.phase_sync("job-exec", || {
        execute::execute_job_attached(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            "/home/ubuntu/workspace",
            &effective_setup_cmds,
            &effective_run_cmd,
            &args.args,
            pty_opt,
        )
    })?;

    // If attached job succeeded, save cache to OBS
    if exit_code == 0
        && let Some(ref put_url) = cache_put_url
    {
        let pb = crate::ui::spinner("Saving dependency cache to OBS...");
        let cache_upload_cmd = format!(
            "if [ -d /home/ubuntu/.cache ] || [ -d /home/ubuntu/.cargo ]; then \
                    tar -czf - -C /home/ubuntu $([ -d /home/ubuntu/.cache ] && echo .cache) $([ -d /home/ubuntu/.cargo ] && echo .cargo) 2>/dev/null | curl -sf -X PUT --upload-file - \"{put_url}\" || true; \
                fi"
        );
        let _ = execute::run_remote_command(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            &cache_upload_cmd,
        );
        pb.finish_and_clear();
    }

    // 12. Post-mortem diagnostics BEFORE VM teardown
    let diagnostic = if exit_code != 0 {
        crate::run::diagnostics::inspect_failure(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            exit_code,
            &[],
            final_preset,
            args.flavor
                .as_deref()
                .unwrap_or_else(|| final_preset.as_str()),
            &vm.name,
        )
    } else {
        None
    };

    // 13. Pull output artifacts
    let local_output_dir = args
        .output
        .unwrap_or_else(|| recipe.workdir.join(&recipe.output_dir));

    let pb = crate::ui::spinner("Checking for output artifacts...");
    let dl_res = ctx.telemetry.phase_sync("output-download", || {
        sync::download_output(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            "/home/ubuntu/workspace",
            artifact_spec,
            &local_output_dir,
        )
    });
    match dl_res {
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

    // 14. Auto-destruction decision
    let should_keep = args.keep || (args.keep_on_failure && exit_code != 0);
    if should_keep {
        println!("{}", format!("Note: VM `{}` kept alive.", vm.name).dimmed());
        if exit_code != 0 {
            println!("  Debug with interactive shell: qecs shell {}", vm.name);
        }
    } else {
        let pb = crate::ui::spinner(format!("Tearing down VM `{}`...", vm.name));
        destroy_vm(ctx, &vm.id, &vm.name).await?;
        pb.finish_and_clear();
        println!(
            "{}",
            format!("✓ Destroyed ephemeral VM `{}`.", vm.name).green()
        );
    }

    // Render diagnostic report if present
    if let Some(ref report) = diagnostic {
        report.render_cargo_style();
    }

    if exit_code != 0 {
        return Err(crate::error::ExitCode(exit_code).into());
    }

    Ok(())
}

const WELL_KNOWN_ENV_VARS: &[&str] = &[
    "HF_TOKEN",
    "HUGGING_FACE_HUB_TOKEN",
    "WANDB_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_DEFAULT_REGION",
    "GITHUB_TOKEN",
];

fn collect_env_vars(args: &RunArgs) -> anyhow::Result<Vec<(String, String)>> {
    let mut env_map = std::collections::BTreeMap::new();

    // 1. Load --env-file if specified
    if let Some(ref env_path) = args.env_file {
        let content = std::fs::read_to_string(env_path)
            .with_context(|| format!("failed to read --env-file `{}`", env_path.display()))?;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = trimmed.split_once('=') {
                let val = v.trim_matches(|c| c == '\'' || c == '"');
                env_map.insert(k.trim().to_string(), val.to_string());
            }
        }
    }

    // 2. Auto-forward well-known variables if forward_env is true
    if args.forward_env {
        for &var_name in WELL_KNOWN_ENV_VARS {
            if let Ok(val) = std::env::var(var_name)
                && !val.trim().is_empty()
            {
                env_map.insert(var_name.to_string(), val);
            }
        }
    }

    // 3. Explicit -e / --env flags
    for entry in &args.env {
        if let Some((k, v)) = entry.split_once('=') {
            env_map.insert(k.trim().to_string(), v.to_string());
        } else if let Ok(val) = std::env::var(entry) {
            env_map.insert(entry.clone(), val);
        }
    }

    Ok(env_map.into_iter().collect())
}

fn mask_token(val: &str) -> String {
    if val.len() <= 6 {
        "***".to_string()
    } else {
        format!("{}...{}", &val[..3], &val[val.len() - 3..])
    }
}

fn print_dry_run_summary(
    recipe: &RunRecipe,
    args: &RunArgs,
    effective_run_cmd: &str,
    env_vars: &[(String, String)],
) {
    println!("{}", "=== qecs run Dry-Run Plan ===".green().bold());
    println!("  Workdir:        {}", recipe.workdir.display());
    println!("  Detector:       {}", recipe.name);
    let preset_str = args
        .preset
        .map(|p| p.as_str())
        .unwrap_or_else(|| recipe.preset.as_str());
    println!("  Preset:         {}", preset_str);
    if let Some(ref flavor) = args.flavor {
        println!("  Flavor:         {}", flavor);
    }
    if !env_vars.is_empty() {
        println!("  Environment variables:");
        for (k, v) in env_vars {
            println!("    - {}={}", k, mask_token(v));
        }
    }
    if !recipe.setup_commands.is_empty() {
        println!("  Setup commands:");
        for cmd in &recipe.setup_commands {
            println!("    - {}", cmd);
        }
    }
    if args.args.is_empty() {
        println!("  Run command:    {}", effective_run_cmd);
    } else {
        println!(
            "  Run command:    {} {}",
            effective_run_cmd,
            args.args.join(" ")
        );
    }
    if let Some(ref artifacts) = args.artifacts {
        println!("  Artifacts:      {}", artifacts);
    } else {
        println!("  Output dir:     {}", recipe.output_dir);
    }
    println!("  Detached:       {}", args.detach);
    println!("  Keep VM:        {}", args.keep || args.keep_on_failure);
}

pub async fn destroy_vm(ctx: &Ctx, server_id: &str, name: &str) -> anyhow::Result<()> {
    ctx.telemetry
        .phase_try("destroy", async {
            let region = ctx.region();
            let client = ctx.signed();
            let project = iam::discover_project(&client, &region).await?;

            let job_id = ecs::delete_servers(&client, &region, &project.id, &[server_id]).await?;
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

            let store = StateStore::open()?;
            if let Ok(Some(r)) = store.get(name) {
                if let Some(ip) = &r.eip {
                    let _ = keys::remove_known_host(ip);
                }
                if let Some(ip) = &r.private_ip {
                    let _ = keys::remove_known_host(ip);
                }
            }
            let _ = store.remove(name);
            anyhow::Ok(())
        })
        .await
}

pub async fn wait_for_gpu_ready(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    timeout: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let poll_cmd = "[ -f /run/qecs/gpu.status ] && cat /run/qecs/gpu.status";

    while start.elapsed() < timeout {
        let output = connect::build_ssh_command(ip, port, key_path, proxy_command)
            .arg(poll_cmd)
            .output();

        if let Ok(out) = output
            && out.status.success()
        {
            let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if status == "READY" {
                return Ok(());
            } else if status == "FAILED" {
                let log_cmd = "tail -n 25 /var/log/qecs-gpu-setup.log 2>/dev/null || true";
                let log_output = connect::build_ssh_command(ip, port, key_path, proxy_command)
                    .arg(log_cmd)
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
