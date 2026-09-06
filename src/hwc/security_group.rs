//! HWC Security Group + ingress rules - idempotent SG ensure + port opening.
//!
//! `qecs` reuses a single shared security group (named `qecs`) and opens TCP 22 + 443
//! for SSH. Egress is allow-all by default, so nothing to add there.

use crate::hwc::client::SignedClient;
use crate::hwc::vpc::vpc_base;
use serde::Deserialize;
use serde_json::{Value, json};

/// A security group with id and name.
#[derive(Debug, Clone, Deserialize)]
pub struct SecurityGroup {
    pub id: String,
    pub name: String,
}

/// Response from create SG or get single SG.
#[derive(Debug, Deserialize)]
pub struct SgResp {
    pub security_group: SecurityGroup,
}

/// Response from list SGs.
#[derive(Debug, Deserialize)]
pub struct SgListResp {
    pub security_groups: Vec<SecurityGroup>,
}

/// Pure: construct the body for creating a security group.
pub(crate) fn create_sg_body(name: &str, vpc_id: &str) -> Value {
    json!({
        "security_group": {
            "name": name,
            "vpc_id": vpc_id,
        }
    })
}

/// Pure: construct the body for adding an ingress rule.
pub fn ingress_rule_body(sg_id: &str, protocol: &str, port: u16) -> Value {
    json!({
        "security_group_rule": {
            "security_group_id": sg_id,
            "direction": "ingress",
            "ethertype": "IPv4",
            "protocol": protocol,
            "port_range_min": port,
            "port_range_max": port,
            "remote_ip_prefix": "0.0.0.0/0",
        }
    })
}

/// Pure: check if a status code indicates a duplicate rule error (409).
pub fn is_duplicate_rule_error(status: u16) -> bool {
    status == 409
}

/// Pure: find a security group by name, or None if not found.
pub(crate) fn pick_sg_by_name(sgs: Vec<SecurityGroup>, name: &str) -> Option<SecurityGroup> {
    sgs.into_iter().find(|sg| sg.name == name)
}

/// Async: ensure a security group named `name` exists in the VPC, creating it if needed.
///
/// Lists all SGs in the VPC, looks for a name match, and if found returns it.
/// Otherwise, creates a new SG with the given name and returns it.
async fn ensure_security_group(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    name: &str,
    vpc_id: &str,
) -> anyhow::Result<SecurityGroup> {
    let base = vpc_base(region, project_id);
    let list_url = format!("{}/security-groups", base);

    // GET list of SGs
    let list_resp: SgListResp = c.send_json(reqwest::Method::GET, &list_url, None).await?;

    // Try to find an existing SG by name
    if let Some(sg) = pick_sg_by_name(list_resp.security_groups, name) {
        return Ok(sg);
    }

    // SG not found, create it
    let create_body = create_sg_body(name, vpc_id);
    let create_resp: SgResp = c
        .send_json(reqwest::Method::POST, &list_url, Some(&create_body))
        .await?;

    Ok(create_resp.security_group)
}

/// Async: ensure an ingress rule exists, creating it if needed.
///
/// Posts the rule; if it already exists (409), returns Ok(()). Other errors are propagated.
async fn ensure_ingress_rule(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    sg_id: &str,
    protocol: &str,
    port: u16,
) -> anyhow::Result<()> {
    let base = vpc_base(region, project_id);
    let rules_url = format!("{}/security-group-rules", base);
    let rule_body = ingress_rule_body(sg_id, protocol, port);

    // POST the rule
    match c
        .send_json::<Value>(reqwest::Method::POST, &rules_url, Some(&rule_body))
        .await
    {
        Ok(_) => Ok(()),
        Err(e) => {
            if is_duplicate_rule_error(e.status) {
                // Rule already exists, treat as success
                Ok(())
            } else {
                Err(anyhow::anyhow!(e))
            }
        }
    }
}

/// Async: ensure the qecs standard ingress rules (TCP 22 and 443).
async fn ensure_qecs_rules(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    sg_id: &str,
) -> anyhow::Result<()> {
    ensure_ingress_rule(c, region, project_id, sg_id, "tcp", 22).await?;
    ensure_ingress_rule(c, region, project_id, sg_id, "tcp", 443).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rule_body_opens_one_port_from_anywhere() {
        let b = ingress_rule_body("sg-1", "tcp", 22);
        let r = &b["security_group_rule"];
        assert_eq!(r["security_group_id"], "sg-1");
        assert_eq!(r["direction"], "ingress");
        assert_eq!(r["ethertype"], "IPv4");
        assert_eq!(r["protocol"], "tcp");
        assert_eq!(r["port_range_min"], 22);
        assert_eq!(r["port_range_max"], 22);
        assert_eq!(r["remote_ip_prefix"], "0.0.0.0/0");
    }

    #[test]
    fn duplicate_rule_status_is_409() {
        assert!(is_duplicate_rule_error(409));
        assert!(!is_duplicate_rule_error(400));
        assert!(!is_duplicate_rule_error(200));
    }

    #[test]
    fn create_sg_body_shape() {
        let b = create_sg_body("qecs", "vpc-1");
        assert_eq!(b["security_group"]["name"], "qecs");
        assert_eq!(b["security_group"]["vpc_id"], "vpc-1");
    }

    #[test]
    fn pick_sg_by_name_finds_and_misses() {
        let sgs: Vec<SecurityGroup> = serde_json::from_value(json!([
            {"id": "sg-1", "name": "qecs"},
            {"id": "sg-2", "name": "other"},
        ]))
        .unwrap();

        let found = pick_sg_by_name(sgs.clone(), "qecs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, "sg-1");

        let not_found = pick_sg_by_name(sgs, "absent");
        assert!(not_found.is_none());
    }
}
