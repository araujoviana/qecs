//! `qecs gc` command: reconciles local state with cloud and deletes dead VMs.
use crate::cli::GcArgs;
use crate::ctx::Ctx;
use crate::lifecycle;
use colored::Colorize;

pub async fn cmd_gc(ctx: &Ctx, args: GcArgs, json: bool) -> anyhow::Result<()> {
    let mut stats = lifecycle::reconcile_and_purge(ctx, args.force).await?;

    if !args.force
        && !json
        && !stats.untracked_active_kept.is_empty()
        && std::io::IsTerminal::is_terminal(&std::io::stdin())
    {
        use std::io::Write;
        println!(
            "{}",
            format!(
                "Found {} untracked active VM(s): {}",
                stats.untracked_active_kept.len(),
                stats.untracked_active_kept.join(", ")
            )
            .yellow()
        );
        print!("Delete these untracked active VM(s)? [y/N]: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok()
            && (line.trim().eq_ignore_ascii_case("y") || line.trim().eq_ignore_ascii_case("yes"))
        {
            let more_stats = lifecycle::reconcile_and_purge(ctx, true).await?;
            stats
                .deleted_from_cloud
                .extend(more_stats.deleted_from_cloud);
            stats.untracked_active_kept.clear();
        }
    }

    let removed_from_state = stats.removed_from_state;
    let deleted_from_cloud = stats.deleted_from_cloud;
    let deleted_eips = stats.deleted_eips;
    let untracked_kept = stats.untracked_active_kept;

    if json {
        let out = serde_json::json!({
            "orphaned_state_removed": removed_from_state,
            "dead_servers_deleted": deleted_from_cloud,
            "unattached_eips_deleted": deleted_eips,
            "untracked_active_kept": untracked_kept,
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
            println!("  Cloud servers deleted: {}", deleted_from_cloud.join(", "));
        }
        if !deleted_eips.is_empty() {
            println!(
                "  Unattached public IPs deleted: {}",
                deleted_eips.join(", ")
            );
        }
        if !untracked_kept.is_empty() {
            println!(
                "  Untracked active VMs kept: {} (pass --force to delete)",
                untracked_kept.join(", ")
            );
        }
        if removed_from_state.is_empty()
            && deleted_from_cloud.is_empty()
            && deleted_eips.is_empty()
            && untracked_kept.is_empty()
        {
            println!("  Nothing to clean up.");
        }
    }

    Ok(())
}
