//! MCP Resource definitions and reading handlers.

use crate::mcp::protocol::{ResourceContent, ResourceDefinition};

/// Return list of available MCP resources.
pub fn list_resources() -> Vec<ResourceDefinition> {
    vec![
        ResourceDefinition {
            uri: "qecs://vms".to_string(),
            name: "Active Ephemeral VMs".to_string(),
            description: Some("Live JSON array of running ephemeral VMs and metadata".to_string()),
            mime_type: "application/json".to_string(),
        },
        ResourceDefinition {
            uri: "qecs://presets".to_string(),
            name: "Compute Presets & Flavors".to_string(),
            description: Some(
                "Catalog of compute presets (normal, ram, compute, gpu, beefy) and specs"
                    .to_string(),
            ),
            mime_type: "application/json".to_string(),
        },
    ]
}

/// Read a resource by its URI.
pub fn read_resource(uri: &str) -> anyhow::Result<ResourceContent> {
    match uri {
        "qecs://vms" => {
            let vms = crate::state::StateStore::open()
                .and_then(|s| s.list())
                .unwrap_or_default();
            let json_text = serde_json::to_string_pretty(&vms)?;
            Ok(ResourceContent {
                uri: uri.to_string(),
                mime_type: "application/json".to_string(),
                text: json_text,
            })
        }
        "qecs://presets" => {
            let specs: Vec<_> = crate::presets::Preset::ALL
                .iter()
                .map(|p| crate::presets::resolve(*p, &crate::config::Config::default(), None))
                .collect();
            let json_text = serde_json::to_string_pretty(&specs)?;
            Ok(ResourceContent {
                uri: uri.to_string(),
                mime_type: "application/json".to_string(),
                text: json_text,
            })
        }
        unknown => anyhow::bail!("unknown resource URI '{unknown}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_resources_exposes_vms_and_presets() {
        let res = list_resources();
        let uris: Vec<&str> = res.iter().map(|r| r.uri.as_str()).collect();
        assert!(uris.contains(&"qecs://vms"));
        assert!(uris.contains(&"qecs://presets"));
    }

    #[test]
    fn reads_presets_resource() {
        let content = read_resource("qecs://presets").unwrap();
        assert_eq!(content.mime_type, "application/json");
        assert!(content.text.contains("normal"));
        assert!(content.text.contains("gpu"));
    }
}
