//! `qecs gc` command: reconciles local state with cloud and deletes dead VMs.
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::iam;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::state::StateStore;
use colored::Colorize;

pub async fn cmd_gc(ctx: &Ctx, json: bool) -> anyhow::Result<()> {
    let store = StateStore::open()?;
    let region = ctx.region();
    let client = ctx.signed();
    let project = iam::discover_project(&client, &region).await?;

    let cloud_servers = ecs::list_servers(&client, &region, &project.id).await?;
    let local_records = store.list()?;

    let mut removed_from_state = Vec::new();
    let mut deleted_from_cloud = Vec::new();

    // 1. Remove state entries for VMs that no longer exist in cloud
    for rec in &local_records {
        if !cloud_servers.iter().any(|s| s.id == rec.id) {
            store.remove(&rec.name)?;
            removed_from_state.push(rec.name.clone());
        }
    }

    // 2. Find managed cloud servers that are stopped or errored and clean them up
    let dead_servers: Vec<&str> = cloud_servers
        .iter()
        .filter(|s| {
            s.tags
                .iter()
                .any(|t| t == "managed-by=qecs" || t == "managed-by")
                || s.name.starts_with("qecs-")
        })
        .filter(|s| s.status == "SHUTOFF" || s.status == "ERROR")
        .map(|s| s.id.as_str())
        .collect();

    if !dead_servers.is_empty() {
        let job_id = ecs::delete_servers(&client, &region, &project.id, &dead_servers).await?;
        jobs::poll_job(
            &client,
            &region,
            &project.id,
            &job_id,
            &PollConfig::default(),
        )
        .await?;
        for id in &dead_servers {
            deleted_from_cloud.push(id.to_string());
            if let Some(r) = local_records.iter().find(|r| &r.id == id) {
                let _ = store.remove(&r.name);
            }
        }
    }

    if json {
        let out = serde_json::json!({
            "orphaned_state_removed": removed_from_state,
            "dead_servers_deleted": deleted_from_cloud,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!("{}", "✓ Garbage collection complete.".green().bold());
        if !removed_from_state.is_empty() {
            println!(
                "  Orphaned local records removed: {}",
                removed_from_state.join(", ")
            );
        }
        if !deleted_from_cloud.is_empty() {
            println!(
                "  Dead cloud servers deleted: {}",
                deleted_from_cloud.join(", ")
            );
        }
        if removed_from_state.is_empty() && deleted_from_cloud.is_empty() {
            println!("  Nothing to clean up.");
        }
    }

    Ok(())
}
