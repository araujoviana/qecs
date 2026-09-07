//! ECS flavor catalog + availability checks.
//!
//! The full `cloudservers/flavors` list is ~1.3 MB / 724 entries and takes
//! several seconds. For the only thing qecs needs at provision time - "does
//! this one flavor exist and is it sellable in this AZ" - use `find_flavor`,
//! which passes `flavor_id` + `availability_zone` as server-side filters and
//! comes back in ~1.5 s with a few KB. `list_flavors` (the whole catalog) is
//! kept for `qecs presets` enrichment and debugging.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Flavor {
    pub id: String,
    pub name: String,
    pub vcpus: String,
    pub ram_mb: u64,
    pub gpu: Option<String>,
    pub cond_image: Option<String>,
}

#[derive(Deserialize)]
struct FlavorsResp {
    flavors: Vec<RawFlavor>,
}

#[derive(Deserialize)]
struct RawFlavor {
    id: String,
    name: String,
    vcpus: String,
    #[serde(deserialize_with = "de_stringy_u64")]
    ram: u64, // MB
    #[serde(default)]
    os_extra_specs: HashMap<String, String>,
}

impl RawFlavor {
    fn into_flavor(self) -> Flavor {
        let gpu = self
            .os_extra_specs
            .get("ecs:performancetype")
            .filter(|t| matches!(t.as_str(), "gpu" | "compute_accelerated"))
            .map(|_| {
                self.os_extra_specs
                    .get("pci_passthrough:gpu_specs")
                    .or_else(|| self.os_extra_specs.get("pci_passthrough:alias"))
                    .cloned()
                    .unwrap_or_default()
            })
            .filter(|s| !s.is_empty());
        let cond_image = self.os_extra_specs.get("cond:image").cloned();
        Flavor {
            id: self.id,
            name: self.name,
            vcpus: self.vcpus,
            ram_mb: self.ram,
            gpu,
            cond_image,
        }
    }
}

/// HWC returns `ram` as a number in some APIs and a string in others.
fn de_stringy_u64<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum N {
        S(String),
        U(u64),
    }
    match N::deserialize(d)? {
        N::U(u) => Ok(u),
        N::S(s) => s.parse().map_err(serde::de::Error::custom),
    }
}

fn flavors_url(region: &str, project_id: &str) -> String {
    let host = endpoint_host(Service::Ecs, region);
    format!("https://{host}/v1/{project_id}/cloudservers/flavors")
}

/// The whole flavor catalog. Slow (~1.3 MB); prefer `find_flavor` for validation.
pub async fn list_flavors(
    client: &SignedClient,
    region: &str,
    project_id: &str,
) -> anyhow::Result<Vec<Flavor>> {
    let resp: FlavorsResp = client
        .send_json(reqwest::Method::GET, &flavors_url(region, project_id), None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp
        .flavors
        .into_iter()
        .map(RawFlavor::into_flavor)
        .collect())
}

/// Whether `flavor_id` exists in `region` and is sellable in `az` right now.
///
/// Uses the server-side `flavor_id` + `availability_zone` filters (small, fast
/// response), then applies `flavor_available_in_az` because HWC still returns a
/// flavor whose `cond:operation` marks it `sellout`/`abandon` in that AZ.
pub async fn find_flavor(
    client: &SignedClient,
    region: &str,
    project_id: &str,
    flavor_id: &str,
    az: &str,
) -> anyhow::Result<Option<Flavor>> {
    let url = format!(
        "{}?availability_zone={az}&flavor_id={flavor_id}",
        flavors_url(region, project_id)
    );
    let resp: FlavorsResp = client
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp
        .flavors
        .into_iter()
        .find(|r| r.id == flavor_id && flavor_available_in_az(&r.os_extra_specs, az))
        .map(RawFlavor::into_flavor))
}

/// True unless the flavor's extra specs mark it sold out / abandoned in `az`.
/// The per-AZ `cond:operation:az` list wins over the global `cond:operation:status`.
pub fn flavor_available_in_az(specs: &HashMap<String, String>, az: &str) -> bool {
    // Per-AZ status ("az1(normal),az2(sellout)") wins where the AZ is listed.
    if let Some(list) = specs.get("cond:operation:az") {
        for entry in list.split(',') {
            if let Some((name, status)) = entry.trim().split_once('(')
                && name == az
            {
                return status.trim_end_matches(')') == "normal";
            }
        }
        // AZ not in the list: fall through to the global status (an unlisted AZ
        // on an otherwise-`abandon` flavor is not offered there).
    }
    specs
        .get("cond:operation:status")
        .map(|s| s != "abandon" && s != "sellout")
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn az_sellout_is_unavailable() {
        let mut m = HashMap::new();
        m.insert(
            "cond:operation:az".to_string(),
            "ap-southeast-3a(normal),ap-southeast-3b(sellout)".to_string(),
        );
        assert!(flavor_available_in_az(&m, "ap-southeast-3a"));
        assert!(!flavor_available_in_az(&m, "ap-southeast-3b"));
    }

    #[test]
    fn per_az_normal_overrides_global_abandon() {
        // Real shape seen from HWC for s7n.2xlarge.2.
        let mut m = HashMap::new();
        m.insert("cond:operation:status".to_string(), "abandon".to_string());
        m.insert(
            "cond:operation:az".to_string(),
            "ap-southeast-3e(normal),ap-southeast-3a(normal)".to_string(),
        );
        assert!(flavor_available_in_az(&m, "ap-southeast-3a"));
        assert!(!flavor_available_in_az(&m, "ap-southeast-3c"));
    }

    #[test]
    fn missing_specs_defaults_available() {
        assert!(flavor_available_in_az(&HashMap::new(), "any"));
    }

    #[test]
    fn find_flavor_filter_url_shape() {
        let url = format!(
            "{}?availability_zone={az}&flavor_id={fid}",
            flavors_url("ap-southeast-3", "proj"),
            az = "ap-southeast-3a",
            fid = "s7n.2xlarge.2",
        );
        assert_eq!(
            url,
            "https://ecs.ap-southeast-3.myhuaweicloud.com/v1/proj/cloudservers/flavors\
             ?availability_zone=ap-southeast-3a&flavor_id=s7n.2xlarge.2"
        );
    }

    #[test]
    fn filtered_response_selects_the_matching_available_flavor() {
        // Real single-flavor filtered payload shape from HWC.
        let j = r#"{"flavors":[{"id":"s7n.2xlarge.2","name":"s7n.2xlarge.2","vcpus":"8",
            "ram":16384,"os_extra_specs":{"ecs:performancetype":"normal",
            "cond:operation:status":"abandon",
            "cond:operation:az":"ap-southeast-3e(normal),ap-southeast-3a(normal)"}}]}"#;
        let r: FlavorsResp = serde_json::from_str(j).unwrap();
        let hit = r
            .flavors
            .into_iter()
            .find(|f| {
                f.id == "s7n.2xlarge.2"
                    && flavor_available_in_az(&f.os_extra_specs, "ap-southeast-3a")
            })
            .map(RawFlavor::into_flavor);
        assert_eq!(hit.unwrap().ram_mb, 16384);

        let r2: FlavorsResp = serde_json::from_str(j).unwrap();
        let miss = r2
            .flavors
            .into_iter()
            .find(|f| flavor_available_in_az(&f.os_extra_specs, "ap-southeast-3c"));
        assert!(miss.is_none());
    }

    #[test]
    fn parses_flavors_with_string_and_numeric_ram() {
        let j = r#"{"flavors":[
            {"id":"s7n.2xlarge.2","name":"s7n.2xlarge.2","vcpus":"8","ram":16384,
             "os_extra_specs":{"ecs:performancetype":"normal"}},
            {"id":"pi2.2xlarge.4","name":"pi2.2xlarge.4","vcpus":"8","ram":"32768",
             "os_extra_specs":{"ecs:performancetype":"gpu","pci_passthrough:gpu_specs":"nvidia-t4:1","cond:image":"__support_gpu_t4=true"}}
        ]}"#;
        let mut r: FlavorsResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.flavors[0].ram, 16384);
        assert_eq!(r.flavors[1].ram, 32768);
        let pi2 = r.flavors.pop().unwrap().into_flavor();
        assert_eq!(pi2.gpu.as_deref(), Some("nvidia-t4:1"));
        assert_eq!(pi2.cond_image.as_deref(), Some("__support_gpu_t4=true"));
    }
}
