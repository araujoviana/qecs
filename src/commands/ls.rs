//! `qecs ls` command: list tracked VMs.
use crate::state::StateStore;
use chrono::{DateTime, Utc};
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
    #[tabled(rename = "TTL Remaining")]
    ttl_remaining: String,
}

pub fn format_ttl_remaining(created_at: &str, ttl_secs: u64) -> String {
    if let Ok(created) = DateTime::parse_from_rfc3339(created_at) {
        let elapsed = Utc::now().signed_duration_since(created.with_timezone(&Utc));
        let remaining_secs = (ttl_secs as i64) - elapsed.num_seconds();
        if remaining_secs <= 0 {
            "expired".to_string()
        } else {
            let mins = remaining_secs / 60;
            let hrs = mins / 60;
            if hrs > 0 {
                format!("{}h {}m", hrs, mins % 60)
            } else {
                format!("{}m", mins)
            }
        }
    } else {
        "-".to_string()
    }
}

pub async fn cmd_ls(json: bool) -> anyhow::Result<()> {
    let store = StateStore::open()?;
    let records = store.list()?;

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
            ttl_remaining: format_ttl_remaining(&r.created_at, r.ttl_secs),
        })
        .collect();

    crate::ui::print_table(rows);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
