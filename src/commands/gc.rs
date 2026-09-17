//! `qecs gc` command: reconciles local state with cloud and deletes dead VMs.
use crate::ctx::Ctx;
use crate::lifecycle;
use colored::Colorize;

pub async fn cmd_gc(ctx: &Ctx, json: bool) -> anyhow::Result<()> {
    let stats = lifecycle::reconcile_and_purge(ctx).await?;
    let removed_from_state = stats.removed_from_state;
    let deleted_from_cloud = stats.deleted_from_cloud;
    let deleted_eips = stats.deleted_eips;

    if json {
        let out = serde_json::json!({
            "orphaned_state_removed": removed_from_state,
            "dead_servers_deleted": deleted_from_cloud,
            "unattached_eips_deleted": deleted_eips,
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
        if !deleted_eips.is_empty() {
            println!(
                "  Unattached public IPs deleted: {}",
                deleted_eips.join(", ")
            );
        }
        if removed_from_state.is_empty() && deleted_from_cloud.is_empty() && deleted_eips.is_empty()
        {
            println!("  Nothing to clean up.");
        }
    }

    Ok(())
}
