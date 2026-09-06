//! Client-side lifecycle management, TTL tracking, and cloud reconciliation.

use crate::ctx::Ctx;
use crate::hwc::ecs;
use crate::hwc::iam;
use crate::hwc::jobs;
use crate::hwc::wait::PollConfig;
use crate::state::StateStore;

/// Computed TTL information for an active or past VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtlInfo {
    pub remaining_secs: i64,
    pub is_expired: bool,
    pub human_remaining: String,
}

/// Calculate the remaining TTL and status from an RFC 3339 creation timestamp.
pub fn calculate_ttl(created_at_rfc3339: &str, ttl_secs: u64) -> TtlInfo {
    let now = chrono::Utc::now();
    let created = match chrono::DateTime::parse_from_rfc3339(created_at_rfc3339) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => now,
    };
    let elapsed = (now - created).num_seconds();
    let remaining = (ttl_secs as i64) - elapsed;

    let is_expired = remaining <= 0;
    let human_remaining = if is_expired {
        "expired".to_string()
    } else {
        let mins = remaining / 60;
        let hrs = mins / 60;
        let rem_mins = mins % 60;
        if hrs > 0 {
            format!("{hrs}h {rem_mins}m")
        } else {
            format!("{mins}m")
        }
    };

    TtlInfo {
        remaining_secs: remaining,
        is_expired,
        human_remaining,
    }
}

/// Statistics returned by garbage collection / reconciliation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GcStats {
    pub removed_from_state: Vec<String>,
    pub deleted_from_cloud: Vec<String>,
}

/// Reconcile local state with cloud and purge stopped (`SHUTOFF`) or errored VMs.
pub async fn reconcile_and_purge(ctx: &Ctx) -> anyhow::Result<GcStats> {
    let store = StateStore::open()?;
    let region = ctx.region();
    let client = ctx.signed();
    let project = iam::discover_project(&client, &region).await?;

    let cloud_servers = ecs::list_servers(&client, &region, &project.id).await?;
    let local_records = store.list()?;

    let mut stats = GcStats::default();

    // 1. Remove state entries for VMs that no longer exist in cloud
    for rec in &local_records {
        if !cloud_servers.iter().any(|s| s.id == rec.id) {
            store.remove(&rec.name)?;
            stats.removed_from_state.push(rec.name.clone());
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
            stats.deleted_from_cloud.push(id.to_string());
            if let Some(r) = local_records.iter().find(|r| &r.id == id) {
                let _ = store.remove(&r.name);
            }
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn test_calculate_ttl_active() {
        let created = (Utc::now() - Duration::minutes(30)).to_rfc3339();
        let ttl = calculate_ttl(&created, 7200); // 2 hours total, 1.5 hours remaining

        assert!(!ttl.is_expired);
        assert!(ttl.remaining_secs > 5000 && ttl.remaining_secs <= 5400);
        assert!(ttl.human_remaining.contains("1h"));
    }

    #[test]
    fn test_calculate_ttl_expired() {
        let created = (Utc::now() - Duration::hours(3)).to_rfc3339();
        let ttl = calculate_ttl(&created, 7200); // 2 hours total, created 3 hours ago

        assert!(ttl.is_expired);
        assert!(ttl.remaining_secs < 0);
        assert_eq!(ttl.human_remaining, "expired");
    }

    #[test]
    fn test_calculate_ttl_sub_hour() {
        let created = (Utc::now() - Duration::minutes(50)).to_rfc3339();
        let ttl = calculate_ttl(&created, 3600); // 1 hour total, 10 min remaining

        assert!(!ttl.is_expired);
        assert_eq!(ttl.human_remaining, "10m");
    }
}
