//! `qecs image` commands: build, list, and delete pre-baked IMS images.

use anyhow::Context;
use chrono::Utc;
use colored::Colorize;
use std::time::Duration;
use tabled::Tabled;

use crate::cli::{ImageAction, ImageArgs, ImageBuildArgs, ImageDeleteArgs};
use crate::ctx::Ctx;
use crate::hwc::endpoints::Service;
use crate::hwc::images;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::keys;
use crate::provision::{self, ProvisionOptions, generate_vm_name};

#[derive(Tabled)]
struct ImageRow {
    #[tabled(rename = "ID")]
    id: String,
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "MIN DISK (GB)")]
    min_disk: u32,
    #[tabled(rename = "CREATED AT")]
    created_at: String,
}

pub async fn cmd_image(ctx: &Ctx, args: ImageArgs, json: bool) -> anyhow::Result<()> {
    match args.action {
        ImageAction::Ls => cmd_image_ls(ctx, json).await,
        ImageAction::Build(b) => cmd_image_build(ctx, b).await,
        ImageAction::Delete(d) => cmd_image_delete(ctx, d).await,
    }
}

async fn cmd_image_ls(ctx: &Ctx, json: bool) -> anyhow::Result<()> {
    let client = ctx.signed();
    let region = ctx.region();
    let images = images::list_private_images(&client, &region).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&images)?);
        return Ok(());
    }

    if images.is_empty() {
        println!(
            "{}",
            format!("No private images found in region `{region}`.").dimmed()
        );
        println!("Run `qecs image build` to create a pre-baked GPU image.");
        return Ok(());
    }

    let rows: Vec<_> = images
        .into_iter()
        .map(|img| ImageRow {
            id: img.id,
            name: img.name,
            status: img.status,
            min_disk: img.min_disk,
            created_at: img.created_at.unwrap_or_else(|| "-".into()),
        })
        .collect();

    crate::ui::print_table(rows);
    Ok(())
}

async fn cmd_image_delete(ctx: &Ctx, args: ImageDeleteArgs) -> anyhow::Result<()> {
    let client = ctx.signed();
    let region = ctx.region();

    let pb = crate::ui::spinner(format!("Deleting private image `{}`...", args.id));
    images::delete_image(&client, &region, &args.id).await?;
    pb.finish_and_clear();

    println!(
        "{}",
        format!("✓ Deleted private image `{}`.", args.id).green()
    );
    Ok(())
}

async fn cmd_image_build(ctx: &Ctx, args: ImageBuildArgs) -> anyhow::Result<()> {
    let client = ctx.signed();
    let region = ctx.region();
    let (paths, _pub_key) = keys::ensure_keypair(None)?;

    let image_name = args
        .name
        .unwrap_or_else(|| format!("qecs-gpu-{}", Utc::now().format("%Y%m%d-%H%M")));

    println!(
        "{}",
        format!("▶ Building pre-baked GPU image `{image_name}` in `{region}`...")
            .cyan()
            .bold()
    );

    // 1. Provision ephemeral GPU builder instance (force fresh gold image)
    let builder_name = generate_vm_name("qecs-builder-{shortid}", "gpu");
    let opts = ProvisionOptions {
        preset: Some("gpu".into()),
        flavor: None,
        name: Some(builder_name.clone()),
        ttl: Some("2h".into()),
        dry_run: false,
        no_baked_image: true,
    };

    let pb = crate::ui::spinner("Provisioning ephemeral GPU builder VM...");
    let vm = provision::provision_vm(ctx, &opts)
        .await?
        .ok_or_else(|| anyhow::anyhow!("builder VM provisioning returned no record"))?;
    pb.finish_and_clear();

    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

    // 2. Wait for SSH readiness
    let pb = crate::ui::spinner(format!(
        "Waiting for SSH readiness on `{}` ({ip})...",
        vm.name
    ));
    let relay_cfg = ctx.config.relay.as_ref();
    let port = crate::connect::resolve_connection_port_with_relay(&ip, None, relay_cfg).await?;
    pb.finish_and_clear();

    let proxy_cmd = relay_cfg.and_then(|r| r.proxy_command_for(&ip, port));

    // 3. Wait for GPU driver and toolkit installation to complete
    let pb = crate::ui::spinner("Installing NVIDIA drivers and container toolkit (~4-8m)...");
    crate::commands::run::wait_for_gpu_ready(
        &ip,
        port,
        &paths.private_key,
        proxy_cmd.as_deref(),
        Duration::from_secs(900),
    )
    .await?;
    pb.finish_and_clear();
    println!(
        "{}",
        "✓ NVIDIA drivers and container toolkit installed successfully.".green()
    );

    // 4. Trigger IMS image creation
    let pb = crate::ui::spinner(format!(
        "Creating IMS system image `{image_name}` from instance..."
    ));
    let job_id = images::create_image_from_server(
        &client,
        &region,
        &vm.id,
        &image_name,
        args.description.as_deref(),
    )
    .await
    .context("requesting IMS system image creation")?;

    let poll_cfg = PollConfig {
        interval: Duration::from_secs(5),
        max_interval: Duration::from_secs(10),
        timeout: Duration::from_secs(900),
    };

    let project = crate::hwc::iam::discover_project(&client, &region).await?;
    let job_res = jobs::poll_job(
        &client,
        Service::Ims,
        &region,
        &project.id,
        &job_id,
        &poll_cfg,
    )
    .await
    .context("waiting for IMS image creation job to complete")?;
    pb.finish_and_clear();

    let created_image_id = job_res
        .image_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "available".into());

    println!(
        "{}",
        format!("✓ Pre-baked image `{image_name}` (ID: {created_image_id}) is ACTIVE.")
            .green()
            .bold()
    );
    println!(
        "  Subsequent `qecs run` and `qecs up` with GPU presets will use this image for ~30s cold starts."
    );

    // 5. Clean up ephemeral builder VM unless --keep is passed
    if !args.keep {
        let pb = crate::ui::spinner(format!(
            "Tearing down ephemeral builder VM `{}`...",
            vm.name
        ));
        crate::commands::run::destroy_vm(ctx, &vm.id, &vm.name).await?;
        pb.finish_and_clear();
        println!(
            "{}",
            format!("✓ Destroyed builder VM `{}`.", vm.name).green()
        );
    } else {
        println!(
            "{}",
            format!(
                "Note: Builder VM `{}` kept alive (--keep specified).",
                vm.name
            )
            .dimmed()
        );
    }

    Ok(())
}
