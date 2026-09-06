//! ECS flavor catalog + availability checks.
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

pub async fn list_flavors(
    client: &SignedClient,
    region: &str,
    project_id: &str,
) -> anyhow::Result<Vec<Flavor>> {
    let host = endpoint_host(Service::Ecs, region);
    let url = format!("https://{host}/v1/{project_id}/cloudservers/flavors");
    let resp: FlavorsResp = client
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp
        .flavors
        .into_iter()
        .map(|r| {
            let gpu = r
                .os_extra_specs
                .get("ecs:performancetype")
                .filter(|t| matches!(t.as_str(), "gpu" | "compute_accelerated"))
                .map(|_| {
                    r.os_extra_specs
                        .get("pci_passthrough:gpu_specs")
                        .or_else(|| r.os_extra_specs.get("pci_passthrough:alias"))
                        .cloned()
                        .unwrap_or_default()
                })
                .filter(|s| !s.is_empty());
            Flavor {
                id: r.id,
                name: r.name,
                vcpus: r.vcpus,
                ram_mb: r.ram,
                gpu,
            }
        })
        .collect())
}

/// True unless the flavor's extra specs mark it sold out / abandoned in `az`.
pub fn flavor_available_in_az(specs: &HashMap<String, String>, az: &str) -> bool {
    if let Some(list) = specs.get("cond:operation:az") {
        // "az1(normal),az2(sellout)"
        for entry in list.split(',') {
            if let Some((name, status)) = entry.trim().split_once('(')
                && name == az
            {
                return status.trim_end_matches(')') == "normal";
            }
        }
        return true;
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
    fn missing_specs_defaults_available() {
        assert!(flavor_available_in_az(&HashMap::new(), "any"));
    }

    #[test]
    fn parses_flavors_with_string_and_numeric_ram() {
        let j = r#"{"flavors":[
            {"id":"s7n.2xlarge.2","name":"s7n.2xlarge.2","vcpus":"8","ram":16384,
             "os_extra_specs":{"ecs:performancetype":"normal"}},
            {"id":"pi2.2xlarge.4","name":"pi2.2xlarge.4","vcpus":"8","ram":"32768",
             "os_extra_specs":{"ecs:performancetype":"gpu","pci_passthrough:gpu_specs":"nvidia-t4:1"}}
        ]}"#;
        let r: FlavorsResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.flavors[0].ram, 16384);
        assert_eq!(r.flavors[1].ram, 32768);
    }
}
