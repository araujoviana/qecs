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
    resolve_target_vm_in_store(store, ctx, name).await
}

/// Resolve the target VM record from a provided StateStore.
pub async fn resolve_target_vm_in_store(
    store: StateStore,
    ctx: &Ctx,
    name: Option<&str>,
) -> anyhow::Result<(StateStore, VmRecord)> {
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

    let relay = connect::Relay::from_config(ctx.config.relay.as_ref())?;

    let pb = crate::ui::spinner(format!("Connecting to `{}` ({ip})...", vm.name));
    let port = match connect::resolve_connection_port_with_relay(&ip, vm.connect_port, Some(&relay))
        .await
    {
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

    let proxy_cmd = relay.proxy_command(&ip, port);

    let status =
        connect::exec_interactive_shell(&ip, port, &paths.private_key, proxy_cmd.as_deref())?;
    if !status.success()
        && let Some(code) = status.code()
    {
        return Err(crate::error::ExitCode(code).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::creds::Credentials;

    fn mock_ctx() -> Ctx {
        let creds = Credentials {
            ak: "test_ak".into(),
            sk: "test_sk".into(),
            security_token: None,
        };
        let global = crate::cli::GlobalArgs {
            region: Some("ap-southeast-3".into()),
            profile: None,
            ak: None,
            sk: None,
            json: false,
            quiet: false,
            verbose: false,
            telemetry: false,
        };
        Ctx {
            config: Config::default(),
            creds,
            http: reqwest::Client::new(),
            global,
            telemetry: None,
        }
    }

    fn sample_vm(name: &str, created_at: &str) -> VmRecord {
        VmRecord {
            id: format!("id-{name}"),
            name: name.to_string(),
            preset: "normal".into(),
            flavor: "s7n.2xlarge.2".into(),
            region: "ap-southeast-3".into(),
            az: "ap-southeast-3a".into(),
            eip: Some("1.2.3.4".into()),
            private_ip: Some("192.168.0.10".into()),
            created_at: created_at.to_string(),
            ttl_secs: 7200,
            connect_port: None,
            job: None,
            tags: vec![],
        }
    }

    #[tokio::test]
    async fn resolve_empty_store_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(dir.path().join("vms.json"));
        let ctx = mock_ctx();

        let res = resolve_target_vm_in_store(store, &ctx, None).await;
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("no active VMs found"));
    }

    #[tokio::test]
    async fn resolve_explicit_name_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(dir.path().join("vms.json"));
        store
            .upsert(sample_vm("target-box", "2026-09-06T10:00:00Z"))
            .unwrap();
        let ctx = mock_ctx();

        let (_store, vm) = resolve_target_vm_in_store(store, &ctx, Some("target-box"))
            .await
            .unwrap();
        assert_eq!(vm.name, "target-box");
    }

    #[tokio::test]
    async fn resolve_omitted_name_picks_most_recent() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(dir.path().join("vms.json"));
        store
            .upsert(sample_vm("older-box", "2026-09-06T09:00:00Z"))
            .unwrap();
        store
            .upsert(sample_vm("newer-box", "2026-09-06T11:00:00Z"))
            .unwrap();
        let ctx = mock_ctx();

        let (_store, vm) = resolve_target_vm_in_store(store, &ctx, None).await.unwrap();
        assert_eq!(vm.name, "newer-box");
    }
}
