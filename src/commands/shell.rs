//! `qecs shell` command: opens an interactive SSH shell on a VM.
use crate::cli::ShellArgs;
use crate::connect;
use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::iam;
use crate::keys;
use crate::state::{StateStore, VmRecord};
use colored::Colorize;

/// Resolve the target VM record, defaulting to the most recent if not specified.
pub async fn resolve_target_vm(
    ctx: &Ctx,
    name: Option<&str>,
) -> anyhow::Result<(StateStore, VmRecord)> {
    let store = StateStore::open()?;

    if let Some(target_name) = name {
        if let Some(rec) = store.get(target_name)? {
            return Ok((store, rec));
        }

        // Fallback: check directly with cloud if state doesn't have it
        let region = ctx.region();
        let client = ctx.signed();
        let project = iam::discover_project(&client, &region).await?;
        let servers = ecs::list_servers(&client, &region, &project.id).await?;
        if let Some(server) = servers.into_iter().find(|s| s.name == target_name) {
            let rec = VmRecord {
                id: server.id,
                name: server.name,
                preset: "unknown".into(),
                flavor: server.flavor,
                region: region.clone(),
                az: server.az,
                eip: server.public_ip,
                private_ip: server.private_ip,
                created_at: chrono::Utc::now().to_rfc3339(),
                ttl_secs: 7200,
                connect_port: None,
                job: None,
                tags: server.tags,
            };
            store.upsert(rec.clone())?;
            return Ok((store, rec));
        }

        anyhow::bail!("VM `{target_name}` not found in local state or cloud");
    }

    // Name omitted: find most recent from state
    let mut records = store.list()?;
    if records.is_empty() {
        anyhow::bail!("no active VMs found; bring one up with `qecs up`");
    }

    // Sort descending by created_at
    records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    let chosen = records.remove(0);

    if !records.is_empty() {
        eprintln!(
            "{}",
            format!(
                "Note: connecting to most recent VM `{}` (specify NAME to target another)",
                chosen.name
            )
            .dimmed()
        );
    }

    Ok((store, chosen))
}

pub async fn cmd_shell(ctx: &Ctx, args: ShellArgs) -> anyhow::Result<()> {
    let (paths, _pub_key) = keys::ensure_keypair(None)?;
    let (store, mut vm) = resolve_target_vm(ctx, args.name.as_deref()).await?;

    let ip = vm
        .eip
        .clone()
        .or_else(|| vm.private_ip.clone())
        .ok_or_else(|| anyhow::anyhow!("VM `{}` has no IP address assigned", vm.name))?;

    let pb = crate::ui::spinner(format!("Connecting to `{}` ({ip})...", vm.name));
    let port = match connect::resolve_connection_port(&ip, vm.connect_port).await {
        Ok(p) => {
            pb.finish_and_clear();
            p
        }
        Err(e) => {
            pb.finish_and_clear();
            // Try fetching VNC console url as emergency fallback
            if let Ok(project) = iam::discover_project(&ctx.signed(), &ctx.region()).await
                && let Ok(console_url) =
                    ecs::remote_console(&ctx.signed(), &ctx.region(), &project.id, &vm.id).await
            {
                eprintln!("{}", "SSH connection failed.".red().bold());
                eprintln!("Direct VNC Console URL:");
                eprintln!("  {}", console_url.cyan());
            }
            return Err(e);
        }
    };

    // Cache winning port if changed
    if vm.connect_port != Some(port) {
        vm.connect_port = Some(port);
        let _ = store.upsert(vm);
    }

    let status = connect::exec_interactive_shell(&ip, port, &paths.private_key)?;
    if !status.success()
        && let Some(code) = status.code()
    {
        std::process::exit(code);
    }
    Ok(())
}
