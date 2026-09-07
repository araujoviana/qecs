//! `qecs kill` command: deletes a VM or all VMs.
use crate::cli::KillArgs;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::endpoints::Service;
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
            Service::Ecs,
            &region,
            &project.id,
            &job_id,
            &PollConfig::default(),
            ctx.telemetry.as_ref(),
        )
        .await?;
        pb.finish_and_clear();

        for r in &records {
            let _ = store.remove(&r.name);
            if let Some(ip) = &r.eip {
                let _ = crate::keys::remove_known_host(ip);
            }
            if let Some(ip) = &r.private_ip {
                let _ = crate::keys::remove_known_host(ip);
            }
        }
        println!(
            "{}",
            format!("✓ Killed {} VM(s).", ids.len()).green().bold()
        );
        return Ok(());
    }

    let (server_id, vm_name) = match &args.name {
        Some(name) => {
            let record = store.get(name)?;
            let id = match record {
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
            (id, name.clone())
        }
        None => {
            let records = store.list()?;
            if records.len() == 1 {
                let r = &records[0];
                (r.id.clone(), r.name.clone())
            } else if records.is_empty() {
                let servers = ecs::list_servers(&client, &region, &project.id).await?;
                if servers.len() == 1 {
                    let s = &servers[0];
                    (s.id.clone(), s.name.clone())
                } else if servers.is_empty() {
                    anyhow::bail!("no active VMs found to kill");
                } else {
                    let names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
                    anyhow::bail!(
                        "multiple active VMs found ({}). specify a VM name or pass --all",
                        names.join(", ")
                    );
                }
            } else {
                let names: Vec<_> = records.iter().map(|r| r.name.as_str()).collect();
                anyhow::bail!(
                    "multiple active VMs found ({}). specify a VM name or pass --all",
                    names.join(", ")
                );
            }
        }
    };

    let pb = crate::ui::spinner(format!("Deleting VM `{vm_name}`..."));
    let job_id = ecs::delete_servers(&client, &region, &project.id, &[&server_id]).await?;
    jobs::poll_job(
        &client,
        Service::Ecs,
        &region,
        &project.id,
        &job_id,
        &PollConfig::default(),
        ctx.telemetry.as_ref(),
    )
    .await?;
    if let Ok(Some(r)) = store.get(&vm_name) {
        if let Some(ip) = &r.eip {
            let _ = crate::keys::remove_known_host(ip);
        }
        if let Some(ip) = &r.private_ip {
            let _ = crate::keys::remove_known_host(ip);
        }
    }
    let _ = store.remove(&vm_name);
    pb.finish_and_clear();
    println!("{}", format!("✓ VM `{vm_name}` killed.").green().bold());
    Ok(())
}
