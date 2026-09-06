//! `qecs kill` command: deletes a VM or all VMs.
use crate::cli::KillArgs;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::iam;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::state::StateStore;
use colored::Colorize;

pub async fn cmd_kill(ctx: &Ctx, args: KillArgs) -> anyhow::Result<()> {
    let store = StateStore::open()?;
    let region = ctx.region();
    let client = ctx.signed();
    let project = iam::discover_project(&client, &region).await?;

    if args.all {
        let records = store.list()?;
        if records.is_empty() {
            println!("No active VMs tracked to kill.");
            return Ok(());
        }

        let ids: Vec<&str> = records.iter().map(|r| r.id.as_str()).collect();
        let pb = crate::ui::spinner(format!("Deleting {} VM(s)...", ids.len()));
        let job_id = ecs::delete_servers(&client, &region, &project.id, &ids).await?;
        jobs::poll_job(
            &client,
            &region,
            &project.id,
            &job_id,
            &PollConfig::default(),
        )
        .await?;
        pb.finish_and_clear();

        for r in &records {
            let _ = store.remove(&r.name);
        }
        println!(
            "{}",
            format!("✓ Killed {} VM(s).", ids.len()).green().bold()
        );
    } else if let Some(name) = &args.name {
        let record = store.get(name)?;
        let server_id = match record {
            Some(r) => r.id,
            None => {
                let servers = ecs::list_servers(&client, &region, &project.id).await?;
                servers
                    .into_iter()
                    .find(|s| &s.name == name)
                    .map(|s| s.id)
                    .ok_or_else(|| anyhow::anyhow!("VM `{name}` not found in state or cloud"))?
            }
        };

        let pb = crate::ui::spinner(format!("Deleting VM `{name}`..."));
        let job_id = ecs::delete_servers(&client, &region, &project.id, &[&server_id]).await?;
        jobs::poll_job(
            &client,
            &region,
            &project.id,
            &job_id,
            &PollConfig::default(),
        )
        .await?;
        let _ = store.remove(name);
        pb.finish_and_clear();
        println!("{}", format!("✓ VM `{name}` killed.").green().bold());
    }
    Ok(())
}
