//! Virtual Private Cloud - idempotent VPC + subnet ensure.
//!
//! `qecs` reuses a single shared VPC and subnet (both named `qecs`) across every
//! run. `ensure_vpc` / `ensure_subnet` list first, reuse a match by name, and
//! only create when nothing is there. A freshly created subnet starts `UNKNOWN`
//! and must reach `ACTIVE` before an ECS can use it, so `ensure_subnet` polls.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use crate::hwc::wait::{Poll, PollConfig, poll_until};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Deserialize)]
pub struct Vpc {
    pub id: String,
    pub name: String,
    pub cidr: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Subnet {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cidr: String,
    #[serde(default)]
    pub vpc_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub availability_zone: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VpcsResp {
    pub vpcs: Vec<Vpc>,
}

#[derive(Debug, Deserialize)]
pub struct VpcResp {
    pub vpc: Vpc,
}

#[derive(Debug, Deserialize)]
pub struct SubnetsResp {
    pub subnets: Vec<Subnet>,
}

#[derive(Debug, Deserialize)]
pub struct SubnetResp {
    pub subnet: Subnet,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicIp {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub public_ip_address: String,
}

#[derive(Debug, Deserialize)]
pub struct PublicIpsResp {
    #[serde(default)]
    pub publicips: Vec<PublicIp>,
}

/// `https://vpc.<region>.myhuaweicloud.com/v1/<project_id>` - the base every VPC,
/// subnet, security-group and EIP call hangs off.
pub(crate) fn vpc_base(region: &str, project_id: &str) -> String {
    format!(
        "https://{}/v1/{project_id}",
        endpoint_host(Service::Vpc, region)
    )
}

fn create_vpc_body(name: &str, cidr: &str) -> serde_json::Value {
    json!({
        "vpc": {
            "name": name,
            "cidr": cidr,
            "description": "qecs ephemeral network",
        }
    })
}

fn create_subnet_body(
    name: &str,
    cidr: &str,
    vpc_id: &str,
    gateway_ip: &str,
    az: &str,
) -> serde_json::Value {
    // No dns_list / primary_dns: DHCP hands out the region resolver. No
    // dhcp_enable: it defaults true, which cloud-init needs (notes section 3).
    json!({
        "subnet": {
            "name": name,
            "cidr": cidr,
            "vpc_id": vpc_id,
            "gateway_ip": gateway_ip,
            "availability_zone": az,
        }
    })
}

trait Named {
    fn name(&self) -> &str;
}

impl Named for Vpc {
    fn name(&self) -> &str {
        &self.name
    }
}

impl Named for Subnet {
    fn name(&self) -> &str {
        &self.name
    }
}

/// The one resource named `name`, if the list has it. HWC's VPC/subnet list APIs
/// have no server-side name filter, so the scan is client-side.
fn pick_by_name<T: Named>(items: Vec<T>, name: &str) -> Option<T> {
    items.into_iter().find(|i| i.name() == name)
}

/// Terminal-state predicate for a subnet poll: `ACTIVE` is ready, `ERROR` is
/// fatal, anything else keeps waiting.
fn subnet_ready(s: &Subnet) -> anyhow::Result<Poll<Subnet>> {
    match s.status.as_str() {
        "ACTIVE" => Ok(Poll::Ready(s.clone())),
        "ERROR" => anyhow::bail!("subnet {} entered ERROR", s.id),
        _ => Ok(Poll::Pending),
    }
}

/// Every VPC in the project (`limit=200` covers any realistic account).
pub async fn list_vpcs(
    c: &SignedClient,
    region: &str,
    project_id: &str,
) -> anyhow::Result<Vec<Vpc>> {
    let url = format!("{}/vpcs?limit=200", vpc_base(region, project_id));
    let resp: VpcsResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.vpcs)
}

/// Reuse the VPC named `name`, or create it with `cidr`.
pub async fn ensure_vpc(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    name: &str,
    cidr: &str,
) -> anyhow::Result<Vpc> {
    if let Some(existing) = pick_by_name(list_vpcs(c, region, project_id).await?, name) {
        return Ok(existing);
    }
    let url = format!("{}/vpcs", vpc_base(region, project_id));
    let body = create_vpc_body(name, cidr);
    let resp: Result<VpcResp, _> = c.send_json(reqwest::Method::POST, &url, Some(&body)).await;
    match resp {
        Ok(r) => Ok(r.vpc),
        Err(e) => {
            if let Some(existing) = pick_by_name(list_vpcs(c, region, project_id).await?, name) {
                return Ok(existing);
            }
            Err(anyhow::anyhow!(e))
        }
    }
}

/// Every subnet in `vpc_id`.
pub async fn list_subnets(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    vpc_id: &str,
) -> anyhow::Result<Vec<Subnet>> {
    let url = format!(
        "{}/subnets?vpc_id={vpc_id}&limit=200",
        vpc_base(region, project_id)
    );
    let resp: SubnetsResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.subnets)
}

async fn get_subnet(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    id: &str,
) -> anyhow::Result<Subnet> {
    let url = format!("{}/subnets/{id}", vpc_base(region, project_id));
    let resp: SubnetResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.subnet)
}

/// Generate non-overlapping CIDR and gateway IP for an availability zone within 192.168.0.0/16.
pub fn az_subnet_cidr_and_gateway(az: &str) -> (String, String) {
    let suffix = az.chars().last().unwrap_or('a');
    let idx = if suffix.is_ascii_lowercase() {
        (suffix as u8 - b'a') as usize
    } else {
        0
    };
    (format!("192.168.{idx}.0/24"), format!("192.168.{idx}.1"))
}

/// Reuse the subnet named `name` in `vpc_id` (or matching AZ), or create it and wait for `ACTIVE`.
#[allow(clippy::too_many_arguments)]
pub async fn ensure_subnet(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    vpc_id: &str,
    name: &str,
    cidr: &str,
    gateway_ip: &str,
    az: &str,
) -> anyhow::Result<Subnet> {
    let subnets = list_subnets(c, region, project_id, vpc_id).await?;
    if let Some(existing) = pick_by_name(subnets.clone(), name) {
        return Ok(existing);
    }
    // Backward compatibility: if "qecs" exists and belongs to target AZ, reuse it
    if let Some(existing) = pick_by_name(subnets, "qecs")
        && existing.availability_zone.as_deref().unwrap_or(az) == az
    {
        return Ok(existing);
    }
    let url = format!("{}/subnets", vpc_base(region, project_id));
    let body = create_subnet_body(name, cidr, vpc_id, gateway_ip, az);
    let post_res: Result<SubnetResp, _> =
        c.send_json(reqwest::Method::POST, &url, Some(&body)).await;

    let created = match post_res {
        Ok(resp) => resp.subnet,
        Err(e) => {
            let current_subnets = list_subnets(c, region, project_id, vpc_id).await?;
            if let Some(existing) = pick_by_name(current_subnets, name) {
                return Ok(existing);
            }
            return Err(anyhow::anyhow!(e));
        }
    };
    let ready = poll_until(
        &PollConfig::default(),
        "subnet ACTIVE",
        || async { subnet_ready(&get_subnet(c, region, project_id, &created.id).await?) },
        c.telemetry.as_ref(),
    )
    .await?;
    Ok(ready)
}

/// Every public IP in the project.
pub async fn list_publicips(
    c: &SignedClient,
    region: &str,
    project_id: &str,
) -> anyhow::Result<Vec<PublicIp>> {
    let url = format!("{}/publicips?limit=200", vpc_base(region, project_id));
    let resp: PublicIpsResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.publicips)
}

/// Delete an unassociated public IP.
pub async fn delete_publicip(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    id: &str,
) -> anyhow::Result<()> {
    let url = format!("{}/publicips/{id}", vpc_base(region, project_id));
    c.send_raw(reqwest::Method::DELETE, &url, None, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url() {
        assert_eq!(
            vpc_base("ap-southeast-3", "P"),
            "https://vpc.ap-southeast-3.myhuaweicloud.com/v1/P"
        );
    }

    #[test]
    fn parses_vpcs() {
        let j = r#"{"vpcs":[{"id":"v-1","name":"qecs-vpc","cidr":"192.168.0.0/16"}]}"#;
        let r: VpcsResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.vpcs[0].name, "qecs-vpc");
        assert_eq!(r.vpcs[0].cidr, "192.168.0.0/16");
    }

    #[test]
    fn create_vpc_body_shape() {
        let b = create_vpc_body("qecs", "192.168.0.0/16");
        assert_eq!(b["vpc"]["name"], "qecs");
        assert_eq!(b["vpc"]["cidr"], "192.168.0.0/16");
        assert_eq!(b["vpc"]["description"], "qecs ephemeral network");
    }

    #[test]
    fn create_subnet_body_shape() {
        let b = create_subnet_body(
            "qecs",
            "192.168.0.0/24",
            "vpc-1",
            "192.168.0.1",
            "ap-southeast-3a",
        );
        assert_eq!(b["subnet"]["name"], "qecs");
        assert_eq!(b["subnet"]["cidr"], "192.168.0.0/24");
        assert_eq!(b["subnet"]["vpc_id"], "vpc-1");
        assert_eq!(b["subnet"]["gateway_ip"], "192.168.0.1");
        assert_eq!(b["subnet"]["availability_zone"], "ap-southeast-3a");
        assert!(b["subnet"].get("dns_list").is_none());
        assert!(b["subnet"].get("dhcp_enable").is_none());
    }

    fn vpc(name: &str) -> Vpc {
        Vpc {
            id: format!("id-{name}"),
            name: name.into(),
            cidr: "192.168.0.0/16".into(),
        }
    }

    fn subnet(name: &str, status: &str) -> Subnet {
        Subnet {
            id: format!("id-{name}"),
            name: name.into(),
            cidr: "192.168.0.0/24".into(),
            vpc_id: "vpc-1".into(),
            status: status.into(),
            availability_zone: None,
        }
    }

    #[test]
    fn pick_by_name_finds_qecs_vpc() {
        let vpcs = vec![vpc("shared"), vpc("qecs"), vpc("other")];
        assert_eq!(pick_by_name(vpcs, "qecs").unwrap().id, "id-qecs");
        assert!(pick_by_name(vec![vpc("shared")], "qecs").is_none());
    }

    #[test]
    fn pick_by_name_finds_qecs_subnet() {
        let subnets = vec![subnet("other", "ACTIVE"), subnet("qecs", "ACTIVE")];
        assert_eq!(pick_by_name(subnets, "qecs").unwrap().id, "id-qecs");
        assert!(pick_by_name(Vec::<Subnet>::new(), "qecs").is_none());
    }

    #[test]
    fn subnet_ready_active_is_ready() {
        match subnet_ready(&subnet("qecs", "ACTIVE")).unwrap() {
            Poll::Ready(r) => assert_eq!(r.id, "id-qecs"),
            Poll::Pending => panic!("ACTIVE should be Ready"),
        }
    }

    #[test]
    fn subnet_ready_error_is_err() {
        assert!(subnet_ready(&subnet("qecs", "ERROR")).is_err());
    }

    #[test]
    fn subnet_ready_other_is_pending() {
        assert!(matches!(
            subnet_ready(&subnet("qecs", "UNKNOWN")).unwrap(),
            Poll::Pending
        ));
    }

    #[test]
    fn test_az_subnet_cidr_and_gateway() {
        let (cidr_a, gw_a) = az_subnet_cidr_and_gateway("ap-southeast-3a");
        assert_eq!(cidr_a, "192.168.0.0/24");
        assert_eq!(gw_a, "192.168.0.1");

        let (cidr_b, gw_b) = az_subnet_cidr_and_gateway("ap-southeast-3b");
        assert_eq!(cidr_b, "192.168.1.0/24");
        assert_eq!(gw_b, "192.168.1.1");

        let (cidr_c, gw_c) = az_subnet_cidr_and_gateway("sa-brazil-1c");
        assert_eq!(cidr_c, "192.168.2.0/24");
        assert_eq!(gw_c, "192.168.2.1");
    }

    #[test]
    fn parses_publicips() {
        let j = r#"{"publicips":[{"id":"eip-1","status":"FREE","public_ip_address":"1.2.3.4"}]}"#;
        let r: PublicIpsResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.publicips.len(), 1);
        assert_eq!(r.publicips[0].id, "eip-1");
        assert_eq!(r.publicips[0].status, "FREE");
        assert_eq!(r.publicips[0].public_ip_address, "1.2.3.4");
    }
}
