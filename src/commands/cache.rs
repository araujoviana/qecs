//! `qecs cache`: manage the regional OBS dependency and build cache.

use anyhow::Context;
use colored::Colorize;
use tabled::{Table, Tabled};

use crate::cli::{CacheAction, CacheArgs};
use crate::ctx::Ctx;
use crate::hwc::{iam, obs};
use crate::telemetry::TelemetryExt;

#[derive(Tabled)]
struct CacheRow {
    #[tabled(rename = "KEY")]
    key: String,
    #[tabled(rename = "SIZE")]
    size: String,
    #[tabled(rename = "LAST MODIFIED")]
    last_modified: String,
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub async fn cmd_cache(ctx: &Ctx, args: CacheArgs) -> anyhow::Result<()> {
    let region = ctx.region();
    let client = ctx.signed();

    let project = ctx
        .telemetry
        .phase_try("iam-project", iam::discover_project(&client, &region))
        .await
        .context("discovering IAM project ID")?;

    let bucket = obs::cache_bucket_name(&region, &project.id);

    match args.action {
        CacheAction::Ls => {
            let objects = if ctx.global.json {
                obs::list_cache_objects(&client, &region, &bucket).await?
            } else {
                let pb = crate::ui::spinner(format!("Querying OBS cache in bucket `{bucket}`..."));
                let res = obs::list_cache_objects(&client, &region, &bucket).await;
                pb.finish_and_clear();
                res?
            };

            if ctx.global.json {
                println!("{}", serde_json::to_string_pretty(&objects)?);
                return Ok(());
            }

            if objects.is_empty() {
                println!(
                    "{}",
                    format!("No cache archives found in bucket `{bucket}`.").dimmed()
                );
                return Ok(());
            }

            let total_bytes: u64 = objects.iter().map(|o| o.size_bytes).sum();
            let rows: Vec<CacheRow> = objects
                .iter()
                .map(|o| CacheRow {
                    key: o.key.clone(),
                    size: format_bytes(o.size_bytes),
                    last_modified: o.last_modified.clone(),
                })
                .collect();

            println!("{}", Table::new(rows));
            println!(
                "{}",
                format!(
                    "\nTotal: {} archives ({}) in `{}`.",
                    objects.len(),
                    format_bytes(total_bytes),
                    bucket
                )
                .green()
            );
        }
        CacheAction::Clean { force } => {
            if ctx.global.json {
                let count = obs::delete_all_cache_objects(&client, &region, &bucket).await?;
                println!(
                    "{}",
                    serde_json::json!({
                        "bucket": bucket,
                        "deleted": count,
                    })
                );
                return Ok(());
            }

            if !force {
                eprintln!(
                    "{}",
                    format!("Warning: this will delete all cached archives in `{bucket}`.")
                        .yellow()
                );
            }
            let pb = crate::ui::spinner(format!("Cleaning cache archives from `{bucket}`..."));
            let count = obs::delete_all_cache_objects(&client, &region, &bucket).await?;
            pb.finish_and_clear();
            println!(
                "{}",
                format!("✓ Deleted {count} cache archives from `{bucket}`.").green()
            );
        }
        CacheAction::Destroy { force } => {
            if ctx.global.json {
                obs::destroy_cache_bucket(&client, &region, &bucket).await?;
                println!(
                    "{}",
                    serde_json::json!({
                        "bucket": bucket,
                        "status": "destroyed",
                    })
                );
                return Ok(());
            }

            if !force {
                eprintln!(
                    "{}",
                    format!("Warning: this will delete all cached archives and DESTROY bucket `{bucket}`.").yellow()
                );
            }
            let pb = crate::ui::spinner(format!("Destroying OBS cache bucket `{bucket}`..."));
            obs::destroy_cache_bucket(&client, &region, &bucket).await?;
            pb.finish_and_clear();
            println!(
                "{}",
                format!("✓ Destroyed OBS cache bucket `{bucket}`.").green()
            );
        }
    }

    Ok(())
}
