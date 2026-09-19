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

/// Build the remote shell command that tars `.cache`/`.cargo` and uploads it
/// to OBS via a presigned PUT URL. Bounded with `curl --max-time`: without it,
/// a slow or stalled upload over a bandwidth-capped connection can silently
/// eat many minutes of wall time before finally failing (the failure itself
/// is swallowed by `|| true` since caching is best-effort) - a live run once
/// lost 681s this way with nothing ever landing in the bucket. Capping the
/// attempt means a run either caches successfully within the budget or gives
/// up fast, instead of stalling silently.
fn build_cache_upload_cmd(put_url: &str) -> String {
    format!(
        "if [ -d /home/ubuntu/.cache ] || [ -d /home/ubuntu/.cargo ]; then \
            tar -czf /tmp/qecs-cache.tar.gz -C /home/ubuntu $([ -d /home/ubuntu/.cache ] && echo .cache) $([ -d /home/ubuntu/.cargo ] && echo .cargo) 2>/dev/null && \
            curl -sf --max-time 300 -T /tmp/qecs-cache.tar.gz \"{put_url}\" && rm -f /tmp/qecs-cache.tar.gz || true; \
        fi"
    )
}

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

    let mut lease = crate::lease::VmLease::new(ctx.clone(), vm);

    let run_res = tokio::select! {
        res = execute_run_pipeline(ctx, &mut lease, &args, &recipe, &effective_run_cmd, &env_vars, final_preset, &paths) => res,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\n{}", format!("Interrupt received. Tearing down VM `{}`...", lease.record().name).yellow().bold());
            if !lease.is_disarmed() && !lease.is_destroyed() {
                let pb = crate::ui::spinner(format!("Tearing down VM `{}`...", lease.record().name));
                let teardown_res = lease.teardown().await;
                pb.finish_and_clear();
                match teardown_res {
                    Ok(()) => {
                        eprintln!(
                            "{}",
                            format!("✓ Destroyed ephemeral VM `{}`.", lease.record().name).green()
                        );
                    }
                    Err(teardown_err) => {
                        eprintln!(
                            "{}",
                            format!(
                                "Warning: failed to tear down VM `{}` ({}): {teardown_err}",
                                lease.record().name,
                                lease.record().id
                            )
                            .red()
                            .bold()
                        );
                        eprintln!("  Clean up manually with: qecs kill {}", lease.record().id);
                    }
                }
            }
            anyhow::bail!("run cancelled by user");
        }
    };

    match run_res {
        Ok(()) => Ok(()),
        Err(e) => {
            if !lease.is_disarmed() && !lease.is_destroyed() {
                let should_keep = args.keep || args.keep_on_failure;
                if should_keep {
                    lease.disarm();
                    eprintln!(
                        "{}",
                        format!("Note: VM `{}` kept alive.", lease.record().name).dimmed()
                    );
                    eprintln!(
                        "  Debug with interactive shell: qecs shell {}",
                        lease.record().name
                    );
                } else {
                    let pb =
                        crate::ui::spinner(format!("Tearing down VM `{}`...", lease.record().name));
                    let teardown_res = lease.teardown().await;
                    pb.finish_and_clear();
                    match teardown_res {
                        Ok(()) => {
                            eprintln!(
                                "{}",
                                format!("✓ Destroyed ephemeral VM `{}`.", lease.record().name)
                                    .green()
                            );
                        }
                        Err(teardown_err) => {
                            eprintln!(
                                "{}",
                                format!(
                                    "Warning: failed to tear down VM `{}` ({}): {teardown_err}",
                                    lease.record().name,
                                    lease.record().id
                                )
                                .red()
                                .bold()
                            );
                            eprintln!("  Clean up manually with: qecs kill {}", lease.record().id);
                        }
                    }
                }
            }
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_run_pipeline(
    ctx: &Ctx,
    lease: &mut crate::lease::VmLease,
    args: &RunArgs,
    recipe: &RunRecipe,
    effective_run_cmd: &str,
    env_vars: &[(String, String)],
    final_preset: crate::presets::Preset,
    paths: &keys::KeyPairPaths,
) -> anyhow::Result<()> {
    let vm = lease.record();
    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

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
    let mut updated_vm = lease.record().clone();
    updated_vm.connect_port = Some(port);
    let _ = store.upsert(updated_vm.clone());
    lease.record_mut().connect_port = Some(port);

    let proxy_cmd = relay.proxy_command(&ip, port);

    // Instantiate persistent ControlMaster session early so all subsequent commands
    // (workspace streaming, env staging, GPU probing, execution) share a single multiplexed channel.
    let control_session = crate::run::tunnel::ControlMasterSession::new(
        &lease.record().name,
        &ip,
        port,
        &paths.private_key,
        proxy_cmd.as_deref(),
    );

    // 8. Stream workspace directly to VM (constant memory, multi-threaded zstd)
    let pb = crate::ui::spinner("Streaming workspace to VM...");
    ctx.telemetry.phase_sync("workdir-stream", || {
        sync::stream_workdir_full(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            Some(&control_session.socket_path),
            &recipe.workdir,
            "/home/ubuntu/workspace",
        )
        .context("streaming workspace to VM")
    })?;
    pb.finish_and_clear();

    // 9. Stage environment variables if present
    if !env_vars.is_empty() {
        execute::stage_env_vars_full(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            Some(&control_session.socket_path),
            env_vars,
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
                wait_for_gpu_ready_full(
                    &ip,
                    port,
                    &paths.private_key,
                    proxy_cmd.as_deref(),
                    Some(&control_session.socket_path),
                    Duration::from_secs(900),
                ),
            )
            .await?;
        pb.finish_and_clear();
        if !ctx.global.json {
            println!(
                "{}",
                "✓ NVIDIA GPU driver and container toolkit ready."
                    .green()
                    .bold()
            );
        }
    }

    // 10b. Prepare OBS dependency caching
    let mut effective_setup_cmds = recipe.setup_commands.clone();
    let cache_key = if !args.no_cache {
        crate::run::detect::compute_cache_key(&recipe.workdir)
    } else {
        None
    };

    // Skip re-uploading the dependency cache when this exact lockfile hash is
    // already in OBS - tar-ing and re-uploading a multi-GB `.cache`/`.cargo`
    // tree over a bandwidth-capped connection on every single successful run
    // (even when nothing changed) previously cost several minutes for no
    // 10b. Restore cache from OBS if a matching cache archive already exists.
    // We check existence first so cold runs don't incur a failed download and
    // unarchive step; the cache key is deterministic so this check has no race
    // benefit; a HEAD request costs a fraction of a second.
    let (cache_target, cache_put_url_detached) = if let Some(ref key) = cache_key {
        let client = ctx.signed();
        let region = ctx.region();
        let project_id_res = ctx
            .telemetry
            .phase_try("iam-project", iam::discover_project(&client, &region))
            .await;

        if let Ok(project) = project_id_res {
            let bucket = crate::hwc::obs::cache_bucket_name(&region, &project.id);
            let _ = crate::hwc::obs::ensure_cache_bucket(&client, &region, &bucket).await;
            let arch = "x86_64";
            let object_key = format!("caches/v1/{arch}-linux/{key}.tar.gz");

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

            let already_cached =
                crate::hwc::obs::object_exists(&client, &region, &bucket, &object_key)
                    .await
                    .unwrap_or(false);

            if already_cached {
                (None, None)
            } else {
                let target = Some((region.clone(), bucket.clone(), object_key.clone()));
                let detached_put = if args.detach {
                    let detach_expiry = (lease.record().ttl_secs + 3600).clamp(1800, 86400);
                    Some(crate::hwc::obs::generate_presigned_url(
                        client.creds(),
                        &region,
                        &bucket,
                        &object_key,
                        "PUT",
                        detach_expiry,
                    ))
                } else {
                    None
                };
                (target, detached_put)
            }
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    // 11. Execution: Detached vs Attached
    let artifact_spec = args.artifacts.as_deref().unwrap_or(&recipe.output_dir);

    if args.detach {
        let cache_upload_cmd = cache_put_url_detached
            .as_ref()
            .map(|put_url| build_cache_upload_cmd(put_url));

        ctx.telemetry.phase_sync("job-exec", || {
            execute::execute_job_detached_full(
                &ip,
                port,
                &paths.private_key,
                proxy_cmd.as_deref(),
                Some(&control_session.socket_path),
                "/home/ubuntu/workspace",
                &effective_setup_cmds,
                effective_run_cmd,
                &args.args,
                artifact_spec,
                cache_upload_cmd.as_deref(),
            )
        })?;

        updated_vm.job = Some(lease.record().name.clone());
        let _ = store.upsert(updated_vm);
        lease.disarm();

        if ctx.global.json {
            println!("{}", serde_json::to_string_pretty(&lease.record())?);
        } else {
            println!(
                "{}",
                format!("✓ Job launched in background on `{}`.", lease.record().name)
                    .green()
                    .bold()
            );
            println!("  Stream logs:  qecs logs {} --follow", lease.record().name);
            println!("  Await result: qecs wait {}", lease.record().name);
        }
        return Ok(());
    }

    // Attached execution
    println!(
        "{}",
        format!(
            "▶ Running `{}` ({}) on `{}`...",
            effective_run_cmd,
            recipe.name,
            lease.record().name
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

    let watcher_stop = crate::run::tunnel::spawn_port_watcher(control_session.clone());

    let exit_code = ctx.telemetry.phase_sync("job-exec", || {
        execute::execute_job_attached(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            "/home/ubuntu/workspace",
            &effective_setup_cmds,
            effective_run_cmd,
            &args.args,
            pty_opt,
            Some(&control_session.socket_path),
        )
    })?;

    let _ = watcher_stop.send(());

    // If attached job succeeded, save cache to OBS
    if exit_code == 0
        && let Some((ref region, ref bucket, ref object_key)) = cache_target
    {
        let pb = crate::ui::spinner("Saving dependency cache to OBS...");
        let client = ctx.signed();
        let fresh_put_url = crate::hwc::obs::generate_presigned_url(
            client.creds(),
            region,
            bucket,
            object_key,
            "PUT",
            1800,
        );
        let cache_upload_cmd = build_cache_upload_cmd(&fresh_put_url);
        let _ = execute::run_remote_command_full(
            &ip,
            port,
            &paths.private_key,
            proxy_cmd.as_deref(),
            Some(&control_session.socket_path),
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
            &lease.record().name,
        )
    } else {
        None
    };

    // 13. Pull output artifacts
    let local_output_dir = args
        .output
        .clone()
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
        lease.disarm();
        println!(
            "{}",
            format!("Note: VM `{}` kept alive.", lease.record().name).dimmed()
        );
        if exit_code != 0 {
            println!(
                "  Debug with interactive shell: qecs shell {}",
                lease.record().name
            );
        }
    } else {
        let pb = crate::ui::spinner(format!("Tearing down VM `{}`...", lease.record().name));
        lease.teardown().await?;
        pb.finish_and_clear();
        println!(
            "{}",
            format!("✓ Destroyed ephemeral VM `{}`.", lease.record().name).green()
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

    for k in env_map.keys() {
        if !crate::run::execute::is_valid_env_key(k) {
            anyhow::bail!(
                "invalid environment variable name `{k}`: must be a valid POSIX identifier (^[a-zA-Z_][a-zA-Z0-9_]*$)"
            );
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

            let poll_res = jobs::poll_job(
                &client,
                Service::Ecs,
                &region,
                &project.id,
                &job_id,
                &PollConfig::default(),
                ctx.telemetry.as_ref(),
            )
            .await;

            let deletion_confirmed = match &poll_res {
                Ok(_) => true,
                Err(_) => {
                    if let Ok(servers) = ecs::list_servers(&client, &region, &project.id).await {
                        !servers.iter().any(|s| s.id == server_id)
                    } else {
                        false
                    }
                }
            };

            if deletion_confirmed {
                if let Ok(store) = StateStore::open() {
                    if let Ok(Some(r)) = store.get(name) {
                        if let Some(ip) = &r.eip {
                            let _ = keys::remove_known_host(ip);
                        }
                        if let Some(ip) = &r.private_ip {
                            let _ = keys::remove_known_host(ip);
                        }
                    }
                    let _ = store.remove(name);
                    let _ = store.remove(server_id);
                }
                anyhow::Ok(())
            } else {
                let err = poll_res
                    .err()
                    .unwrap_or_else(|| anyhow::anyhow!("failed to confirm VM deletion in cloud"));
                Err(err)
            }
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
    wait_for_gpu_ready_full(ip, port, key_path, proxy_command, None, timeout).await
}

pub async fn wait_for_gpu_ready_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    control_socket: Option<&Path>,
    timeout: Duration,
) -> anyhow::Result<()> {
    let start = Instant::now();
    let poll_cmd = "[ -f /run/qecs/gpu.status ] && cat /run/qecs/gpu.status";

    while start.elapsed() < timeout {
        let output = connect::build_ssh_command_full(
            ip,
            port,
            key_path,
            proxy_command,
            None,
            control_socket,
        )
        .arg(poll_cmd)
        .output();

        if let Ok(out) = output
            && out.status.success()
        {
            let status = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if status == "READY" {
                return Ok(());
            } else if status == "FAILED" {
                let log_cmd = "tail -n 60 /var/log/qecs-gpu-setup.log 2>/dev/null || true";
                let log_output = connect::build_ssh_command_full(
                    ip,
                    port,
                    key_path,
                    proxy_command,
                    None,
                    control_socket,
                )
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

#[cfg(test)]
mod cache_upload_tests {
    use super::build_cache_upload_cmd;

    #[test]
    fn caps_the_upload_with_a_bounded_timeout() {
        let cmd = build_cache_upload_cmd("https://example.com/put");
        assert!(
            cmd.contains("--max-time"),
            "upload must not be able to hang indefinitely: {cmd}"
        );
    }

    #[test]
    fn upload_failure_is_best_effort_and_never_fails_the_run() {
        let cmd = build_cache_upload_cmd("https://example.com/put");
        assert!(cmd.contains("|| true"));
    }

    #[test]
    fn embeds_the_exact_presigned_url() {
        let cmd = build_cache_upload_cmd("https://example.com/put?X-Amz-Signature=abc123");
        assert!(cmd.contains("\"https://example.com/put?X-Amz-Signature=abc123\""));
    }
}
