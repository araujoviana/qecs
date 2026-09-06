//! SSH keypair management - import and find qecs public key.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Deserialize)]
pub struct Keypair {
    pub name: String,
    pub fingerprint: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawKeypair {
    pub(crate) name: String,
    pub(crate) fingerprint: String,
    #[serde(default)]
    pub(crate) public_key: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeypairEnvelope {
    keypair: RawKeypair,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeypairsResp {
    pub(crate) keypairs: Vec<KeypairEnvelope>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct KeypairResp {
    pub(crate) keypair: RawKeypair,
}

/// Build the os-keypairs endpoint URL.
pub(crate) fn keypairs_url(region: &str, project_id: &str) -> String {
    let host = endpoint_host(Service::Ecs, region);
    format!("https://{}/v2.1/{}/os-keypairs", host, project_id)
}

/// Pure: construct the body for importing a keypair.
pub(crate) fn import_body(name: &str, public_key: &str) -> serde_json::Value {
    json!({
        "keypair": {
            "name": name,
            "public_key": public_key,
            "type": "ssh"
        }
    })
}

/// Pure: find a keypair by name in the list response.
pub(crate) fn pick_keypair(resp: KeypairsResp, name: &str) -> Option<Keypair> {
    resp.keypairs
        .into_iter()
        .find(|e| e.keypair.name == name)
        .map(|e| Keypair {
            name: e.keypair.name,
            fingerprint: e.keypair.fingerprint,
        })
}

/// Async: get a keypair by name, or None if not found.
pub async fn find_keypair(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    name: &str,
) -> anyhow::Result<Option<Keypair>> {
    let url = keypairs_url(region, project_id);
    let resp: KeypairsResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(pick_keypair(resp, name))
}

/// Async: import a keypair, or return the existing one if it already exists.
///
/// If the keypair already exists, returns it. Otherwise, creates it via POST.
/// If POST returns 409 (conflict), treats it as "exists" and re-fetches.
pub async fn import_keypair(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    name: &str,
    public_key: &str,
) -> anyhow::Result<Keypair> {
    // Check if already exists
    if let Some(existing) = find_keypair(c, region, project_id, name).await? {
        return Ok(existing);
    }

    // Import the keypair
    let url = keypairs_url(region, project_id);
    let body = import_body(name, public_key);

    match c
        .send_json::<KeypairResp>(reqwest::Method::POST, &url, Some(&body))
        .await
    {
        Ok(resp) => Ok(Keypair {
            name: resp.keypair.name,
            fingerprint: resp.keypair.fingerprint,
        }),
        Err(e) => {
            if e.status == 409 {
                // Keypair already exists, re-fetch it
                find_keypair(c, region, project_id, name)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("keypair {} existed but cannot be found", name))
            } else {
                Err(anyhow::anyhow!(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_body_shape() {
        let b = import_body("qecs", "ssh-ed25519 AAAA... qecs");
        assert_eq!(b["keypair"]["name"], "qecs");
        assert_eq!(b["keypair"]["type"], "ssh");
        assert!(
            b["keypair"]["public_key"]
                .as_str()
                .unwrap()
                .starts_with("ssh-ed25519")
        );
    }

    #[test]
    fn parses_nested_keypair_list() {
        let j = r#"{"keypairs":[{"keypair":{"name":"qecs","fingerprint":"SHA256:x","public_key":"ssh-ed25519 A"}}]}"#;
        let found = super::pick_keypair(serde_json::from_str(j).unwrap(), "qecs");
        assert_eq!(found.unwrap().fingerprint, "SHA256:x");
    }
}
