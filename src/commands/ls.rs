//! `qecs ls` command: list tracked and cloud VMs.
use crate::ctx::Ctx;
use crate::hwc::{ecs, iam};
use crate::state::StateStore;
use tabled::Tabled;

#[derive(Tabled)]
struct LsRow {
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Preset")]
    preset: String,
    #[tabled(rename = "Flavor")]
    flavor: String,
    #[tabled(rename = "Region/AZ")]
    region_az: String,
    #[tabled(rename = "Public IP")]
    public_ip: String,
    #[tabled(rename = "Status")]
    status: String,
    #[tabled(rename = "TTL Remaining")]
    ttl_remaining: String,
}

pub fn format_ttl_remaining(created_at: &str, ttl_secs: u64) -> String {
    crate::lifecycle::calculate_ttl(created_at, ttl_secs).human_remaining
}

pub async fn cmd_ls(ctx: Option<&Ctx>, local_only: bool, json: bool) -> anyhow::Result<()> {
    let store = StateStore::open()?;
    let records = store.list()?;

    let cloud_servers = if !local_only && let Some(ctx) = ctx {
        let region = ctx.region();
        let client = ctx.signed();
        match iam::discover_project(&client, &region).await {
            Ok(proj) => ecs::list_servers(&client, &region, &proj.id).await.ok(),
            Err(_) => None,
        }
    } else {
        None
    };

    if let Some(cloud) = cloud_servers {
        let qecs_cloud: Vec<_> = cloud
            .into_iter()
            .filter(|s| {
                s.tags
                    .iter()
                    .any(|t| t == "managed-by=qecs" || t == "managed-by")
                    || s.name.starts_with("qecs-")
            })
            .collect();

        if json {
            let mut out = Vec::new();
            for r in &records {
                let matching = qecs_cloud.iter().find(|s| s.id == r.id || s.name == r.name);
                let status = matching
                    .map(|s| s.status.as_str())
                    .unwrap_or("LOCAL ONLY (GONE)");
                let mut val = serde_json::to_value(r)?;
                if let Some(obj) = val.as_object_mut() {
                    obj.insert("status".into(), serde_json::json!(status));
                    obj.insert("orphaned".into(), serde_json::json!(false));
                }
                out.push(val);
            }
            for s in &qecs_cloud {
                if !records.iter().any(|r| r.id == s.id || r.name == s.name) {
                    out.push(serde_json::json!({
                        "id": s.id,
                        "name": s.name,
                        "preset": "-",
                        "flavor": s.flavor,
                        "region": ctx.map(|c| c.region()).unwrap_or_default(),
                        "az": s.az,
                        "eip": s.public_ip,
                        "private_ip": s.private_ip,
                        "created_at": "-",
                        "ttl_secs": 0,
                        "status": format!("ORPHAN ({})", s.status),
                        "orphaned": true,
                    }));
                }
            }
            println!("{}", serde_json::to_string_pretty(&out)?);
            return Ok(());
        }

        let mut rows = Vec::new();
        for r in &records {
            let matching = qecs_cloud.iter().find(|s| s.id == r.id || s.name == r.name);
            let status = match matching {
                Some(s) => s.status.clone(),
                None => "LOCAL ONLY (GONE)".to_string(),
            };
            rows.push(LsRow {
                name: r.name.clone(),
                preset: r.preset.clone(),
                flavor: r.flavor.clone(),
                region_az: format!("{}/{}", r.region, r.az),
                public_ip: r
                    .eip
                    .clone()
                    .unwrap_or_else(|| r.private_ip.clone().unwrap_or_else(|| "-".into())),
                status,
                ttl_remaining: format_ttl_remaining(&r.created_at, r.ttl_secs),
            });
        }
        for s in &qecs_cloud {
            if !records.iter().any(|r| r.id == s.id || r.name == s.name) {
                let region = ctx.map(|c| c.region()).unwrap_or_default();
                rows.push(LsRow {
                    name: s.name.clone(),
                    preset: "-".into(),
                    flavor: s.flavor.clone(),
                    region_az: format!("{}/{}", region, s.az),
                    public_ip: s.public_ip.clone().unwrap_or_else(|| "-".into()),
                    status: format!("ORPHAN ({})", s.status),
                    ttl_remaining: "-".into(),
                });
            }
        }

        if rows.is_empty() {
            println!("No active VMs tracked.");
            return Ok(());
        }

        crate::ui::print_table(rows);
        Ok(())
    } else {
        if json {
            println!("{}", serde_json::to_string_pretty(&records)?);
            return Ok(());
        }

        if records.is_empty() {
            println!("No active VMs tracked.");
            return Ok(());
        }

        let rows: Vec<LsRow> = records
            .into_iter()
            .map(|r| LsRow {
                name: r.name,
                preset: r.preset,
                flavor: r.flavor,
                region_az: format!("{}/{}", r.region, r.az),
                public_ip: r
                    .eip
                    .unwrap_or_else(|| r.private_ip.unwrap_or_else(|| "-".into())),
                status: "TRACKED".into(),
                ttl_remaining: format_ttl_remaining(&r.created_at, r.ttl_secs),
            })
            .collect();

        crate::ui::print_table(rows);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn formats_ttl_correctly() {
        let now = Utc::now().to_rfc3339();
        let remaining = format_ttl_remaining(&now, 3600);
        assert!(remaining.contains("m") || remaining.contains("h"));

        // Already expired
        let past = "2020-01-01T00:00:00Z";
        assert_eq!(format_ttl_remaining(past, 100), "expired");
    }
}
