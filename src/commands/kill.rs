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
        let cloud_servers = ecs::list_servers(&client, &region, &project.id)
            .await
            .unwrap_or_default();
        let mut ids: Vec<String> = records.iter().map(|r| r.id.clone()).collect();
        for s in &cloud_servers {
            let is_qecs = s
                .tags
                .iter()
                .any(|t| t == "managed-by=qecs" || t == "managed-by")
                || s.name.starts_with("qecs-");
            if is_qecs && !ids.contains(&s.id) {
                ids.push(s.id.clone());
            }
        }

        if ids.is_empty() {
            if ctx.global.json {
                println!("{}", serde_json::json!({ "deleted": 0, "requested": 0 }));
            } else {
                println!("No active VMs tracked to kill.");
            }
            return Ok(());
        }

        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        let pb = crate::ui::spinner(format!("Deleting {} VM(s)...", id_refs.len()));
        let job_id = ecs::delete_servers(&client, &region, &project.id, &id_refs).await?;
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
        pb.finish_and_clear();

        let remaining = ecs::list_servers(&client, &region, &project.id)
            .await
            .unwrap_or_default();
        let mut confirmed_deleted = 0;
        for id in &ids {
            if !remaining.iter().any(|s| &s.id == id) {
                confirmed_deleted += 1;
                let maybe_rec = store.get(id).ok().flatten();
                let _ = store.remove(id);
                if let Some(r) = maybe_rec {
                    let _ = store.remove(&r.name);
                    if let Some(ip) = &r.eip {
                        let _ = crate::keys::remove_known_host(ip);
                    }
                    if let Some(ip) = &r.private_ip {
                        let _ = crate::keys::remove_known_host(ip);
                    }
                }
            }
        }
        for r in &records {
            if !remaining.iter().any(|s| s.id == r.id) {
                let _ = store.remove(&r.name);
                let _ = store.remove(&r.id);
            }
        }
        for s in &cloud_servers {
            if !remaining.iter().any(|srv| srv.id == s.id) {
                let _ = store.remove(&s.name);
                let _ = store.remove(&s.id);
            }
        }

        if confirmed_deleted == ids.len() {
            if ctx.global.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "deleted": confirmed_deleted,
                        "requested": ids.len(),
                    })
                );
            } else {
                println!(
                    "{}",
                    format!("✓ Killed {} VM(s).", id_refs.len()).green().bold()
                );
            }
            return Ok(());
        } else if let Err(e) = poll_res {
            return Err(e);
        } else {
            anyhow::bail!(
                "only {} of {} VMs confirmed deleted",
                confirmed_deleted,
                ids.len()
            );
        }
    }

    let (server_id, vm_name) =
        resolve_kill_target(&store, &client, &region, &project.id, args.name.as_deref()).await?;

    let pb = crate::ui::spinner(format!("Deleting VM `{vm_name}`..."));
    let job_id = ecs::delete_servers(&client, &region, &project.id, &[&server_id]).await?;
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
    pb.finish_and_clear();

    let confirmed = match &poll_res {
        Ok(_) => true,
        Err(_) => {
            if let Ok(servers) = ecs::list_servers(&client, &region, &project.id).await {
                !servers.iter().any(|s| s.id == server_id)
            } else {
                false
            }
        }
    };

    if confirmed {
        if let Ok(Some(r)) = store.get(&server_id) {
            if let Some(ip) = &r.eip {
                let _ = crate::keys::remove_known_host(ip);
            }
            if let Some(ip) = &r.private_ip {
                let _ = crate::keys::remove_known_host(ip);
            }
        }
        let _ = store.remove(&vm_name);
        let _ = store.remove(&server_id);
        if ctx.global.json {
            println!(
                "{}",
                serde_json::json!({
                    "name": vm_name,
                    "id": server_id,
                    "status": "deleted",
                })
            );
        } else {
            println!("{}", format!("✓ VM `{vm_name}` killed.").green().bold());
        }
        Ok(())
    } else {
        let err = poll_res
            .err()
            .unwrap_or_else(|| anyhow::anyhow!("failed to confirm VM deletion in cloud"));
        Err(err)
    }
}

pub(crate) async fn resolve_kill_target(
    store: &StateStore,
    client: &crate::hwc::client::SignedClient,
    region: &str,
    project_id: &str,
    name: Option<&str>,
) -> anyhow::Result<(String, String)> {
    match name {
        Some(target) => {
            let record = store.get(target)?;
            let (id, vname) = match record {
                Some(r) => (r.id, r.name),
                None => {
                    let servers = ecs::list_servers(client, region, project_id).await?;
                    let matched = servers
                        .into_iter()
                        .find(|s| s.name == target || s.id == target)
                        .ok_or_else(|| {
                            anyhow::anyhow!("VM `{target}` not found in state or cloud")
                        })?;
                    (matched.id, matched.name)
                }
            };
            Ok((id, vname))
        }
        None => {
            let records = store.list()?;
            if records.len() == 1 {
                let r = &records[0];
                Ok((r.id.clone(), r.name.clone()))
            } else if records.is_empty() {
                let all_servers = ecs::list_servers(client, region, project_id).await?;
                let servers: Vec<_> = all_servers
                    .into_iter()
                    .filter(|s| {
                        s.tags
                            .iter()
                            .any(|t| t == "managed-by=qecs" || t == "managed-by")
                            || s.name.starts_with("qecs-")
                    })
                    .collect();
                if servers.len() == 1 {
                    let s = &servers[0];
                    Ok((s.id.clone(), s.name.clone()))
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::VmRecord;

    fn sample_record(id: &str, name: &str) -> VmRecord {
        VmRecord {
            id: id.to_string(),
            name: name.to_string(),
            preset: "gpu".into(),
            flavor: "pi2.4xlarge.4".into(),
            region: "ap-southeast-3".into(),
            az: "ap-southeast-3a".into(),
            eip: Some("1.2.3.4".into()),
            private_ip: Some("192.168.0.10".into()),
            created_at: "2026-09-18T00:00:00Z".into(),
            ttl_secs: 7200,
            connect_port: None,
            job: None,
            tags: vec![],
        }
    }

    fn dummy_client() -> crate::hwc::client::SignedClient {
        crate::hwc::client::SignedClient::new(
            reqwest::Client::new(),
            crate::creds::Credentials {
                ak: "mock_ak".into(),
                sk: "mock_sk".into(),
                security_token: None,
            },
        )
    }

    #[tokio::test]
    async fn resolve_by_name_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(dir.path().join("vms.json"));
        store
            .upsert(sample_record("uuid-1234", "qecs-gpu-demo"))
            .unwrap();
        let client = dummy_client();

        let (id, name) = resolve_kill_target(
            &store,
            &client,
            "ap-southeast-3",
            "proj-123",
            Some("qecs-gpu-demo"),
        )
        .await
        .unwrap();

        assert_eq!(id, "uuid-1234");
        assert_eq!(name, "qecs-gpu-demo");
    }

    #[tokio::test]
    async fn resolve_by_id_from_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(dir.path().join("vms.json"));
        store
            .upsert(sample_record("uuid-1234", "qecs-gpu-demo"))
            .unwrap();
        let client = dummy_client();

        let (id, name) = resolve_kill_target(
            &store,
            &client,
            "ap-southeast-3",
            "proj-123",
            Some("uuid-1234"),
        )
        .await
        .unwrap();

        assert_eq!(id, "uuid-1234");
        assert_eq!(name, "qecs-gpu-demo");
    }

    #[tokio::test]
    async fn resolve_no_arg_single_vm() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(dir.path().join("vms.json"));
        store
            .upsert(sample_record("uuid-1234", "qecs-gpu-demo"))
            .unwrap();
        let client = dummy_client();

        let (id, name) = resolve_kill_target(&store, &client, "ap-southeast-3", "proj-123", None)
            .await
            .unwrap();

        assert_eq!(id, "uuid-1234");
        assert_eq!(name, "qecs-gpu-demo");
    }

    #[tokio::test]
    async fn resolve_no_arg_multiple_vms_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::at(dir.path().join("vms.json"));
        store.upsert(sample_record("uuid-1", "qecs-gpu-1")).unwrap();
        store.upsert(sample_record("uuid-2", "qecs-gpu-2")).unwrap();
        let client = dummy_client();

        let res = resolve_kill_target(&store, &client, "ap-southeast-3", "proj-123", None).await;

        assert!(res.is_err());
        assert!(
            res.unwrap_err()
                .to_string()
                .contains("multiple active VMs found")
        );
    }
}
