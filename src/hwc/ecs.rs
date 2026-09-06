//! Elastic Cloud Server - server lifecycle models plus the create + dry-run calls.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Deserialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct ServersResp {
    pub servers: Vec<Server>,
}

/// `https://ecs.<region>.myhuaweicloud.com/v1/<project_id>` - the base every
/// `cloudservers` call hangs off (mirrors `vpc::vpc_base`). `jobs::job_url` builds
/// the same `/v1/{project}` prefix independently for the jobs endpoint.
pub(crate) fn ecs_base(region: &str, project_id: &str) -> String {
    format!(
        "https://{}/v1/{project_id}",
        endpoint_host(Service::Ecs, region)
    )
}

/// Borrowed inputs for one pay-per-use ECS create (notes section 6). Branch 2b
/// assembles this from a resolved preset spec and owns the referenced strings.
pub struct CreateServer<'a> {
    pub name: &'a str,
    pub image_id: &'a str,
    pub flavor: &'a str,
    pub vpc_id: &'a str,
    pub subnet_id: &'a str,
    pub az: &'a str,
    pub key_name: &'a str,
    pub sg_id: &'a str,
    pub root_volume_type: &'a str,
    pub root_volume_gb: u32,
    /// base64 of the cloud-init text; omitted from the payload when `None`.
    pub user_data_b64: Option<&'a str>,
    /// UTC `yyyy-MM-ddTHH:mm:ssZ` server-side auto-delete; omitted when `None`.
    pub auto_terminate: Option<&'a str>,
    /// When `true`, request an inline dynamic-BGP EIP that dies with the VM.
    pub eip: bool,
    /// `(key, value)` server tags; the `server_tags` key is omitted when empty.
    pub tags: &'a [(&'a str, &'a str)],
}

impl CreateServer<'_> {
    /// Build the notes section 6 create payload. `dry_run` adds `"dry_run": true`
    /// as a sibling of `"server"` (HWC validates without creating anything).
    pub fn body(&self, dry_run: bool) -> Value {
        let mut server = Map::new();
        server.insert("name".into(), json!(self.name));
        server.insert("imageRef".into(), json!(self.image_id));
        server.insert("flavorRef".into(), json!(self.flavor));
        server.insert("vpcid".into(), json!(self.vpc_id));
        server.insert("nics".into(), json!([{ "subnet_id": self.subnet_id }]));
        server.insert(
            "root_volume".into(),
            json!({ "volumetype": self.root_volume_type, "size": self.root_volume_gb }),
        );
        server.insert("availability_zone".into(), json!(self.az));
        server.insert("key_name".into(), json!(self.key_name));
        server.insert("security_groups".into(), json!([{ "id": self.sg_id }]));
        server.insert("count".into(), json!(1));

        if let Some(user_data) = self.user_data_b64 {
            server.insert("user_data".into(), json!(user_data));
        }
        if let Some(at) = self.auto_terminate {
            server.insert("auto_terminate_time".into(), json!(at));
        }
        if self.eip {
            server.insert(
                "publicip".into(),
                json!({
                    "eip": {
                        "iptype": "5_bgp",
                        "bandwidth": { "size": 100, "sharetype": "PER", "chargemode": "traffic" },
                    },
                    "delete_on_termination": true,
                }),
            );
        }
        if !self.tags.is_empty() {
            let tags: Vec<Value> = self
                .tags
                .iter()
                .map(|&(k, v)| json!({ "key": k, "value": v }))
                .collect();
            server.insert("server_tags".into(), Value::Array(tags));
        }

        let mut out = Map::new();
        out.insert("server".into(), Value::Object(server));
        if dry_run {
            out.insert("dry_run".into(), json!(true));
        }
        Value::Object(out)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateResp {
    pub(crate) job_id: String,
}

/// `POST /v1/{project_id}/cloudservers` - submit a create, return the async `job_id`
/// (feed it to `jobs::poll_job` for the server id).
pub async fn create_server(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    spec: &CreateServer<'_>,
) -> anyhow::Result<String> {
    let url = format!("{}/cloudservers", ecs_base(region, project_id));
    let body = spec.body(false);
    let resp: CreateResp = c
        .send_json(reqwest::Method::POST, &url, Some(&body))
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.job_id)
}

/// Same endpoint with `dry_run: true` - HWC validates the spec (quota, flavor/AZ,
/// image, network) without creating anything. `Ok(())` on a 2xx, the `ApiError`
/// otherwise.
pub async fn create_server_dry_run(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    spec: &CreateServer<'_>,
) -> anyhow::Result<()> {
    let url = format!("{}/cloudservers", ecs_base(region, project_id));
    let body = spec.body(true);
    c.send_json::<Value>(reqwest::Method::POST, &url, Some(&body))
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_servers() {
        let j = r#"{"servers":[{"id":"i-1","name":"qecs-gpu-ab12","status":"ACTIVE"}]}"#;
        let r: ServersResp = serde_json::from_str(j).unwrap();
        assert_eq!(r.servers[0].status, "ACTIVE");
    }

    /// A spec with no optionals set, EIP off, no tags.
    fn minimal_spec() -> CreateServer<'static> {
        CreateServer {
            name: "qecs-normal-ab12",
            image_id: "img-1",
            flavor: "s7n.2xlarge.2",
            vpc_id: "vpc-1",
            subnet_id: "sub-1",
            az: "ap-southeast-3a",
            key_name: "qecs",
            sg_id: "sg-1",
            root_volume_type: "GPSSD",
            root_volume_gb: 100,
            user_data_b64: None,
            auto_terminate: None,
            eip: false,
            tags: &[],
        }
    }

    #[test]
    fn body_has_required_fields_and_object_shaped_nics_and_sgs() {
        let spec = CreateServer {
            name: "qecs-normal-ab12",
            image_id: "img-1",
            flavor: "s7n.2xlarge.2",
            vpc_id: "vpc-1",
            subnet_id: "sub-1",
            az: "ap-southeast-3a",
            key_name: "qecs",
            sg_id: "sg-1",
            root_volume_type: "GPSSD",
            root_volume_gb: 100,
            user_data_b64: Some("YmFzZTY0"),
            auto_terminate: Some("2026-09-06T14:00:00Z"),
            eip: true,
            tags: &[("managed-by", "qecs")],
        };
        let b = spec.body(false);
        let s = &b["server"];
        assert_eq!(s["flavorRef"], "s7n.2xlarge.2");
        assert_eq!(s["nics"][0]["subnet_id"], "sub-1");
        assert_eq!(s["security_groups"][0]["id"], "sg-1");
        assert_eq!(s["root_volume"]["volumetype"], "GPSSD");
        assert_eq!(s["root_volume"]["size"], 100);
        assert_eq!(s["user_data"], "YmFzZTY0");
        assert_eq!(s["auto_terminate_time"], "2026-09-06T14:00:00Z");
        assert_eq!(s["publicip"]["eip"]["iptype"], "5_bgp");
        assert_eq!(s["publicip"]["delete_on_termination"], true);
        assert_eq!(s["server_tags"][0]["key"], "managed-by");
        assert!(b.get("dry_run").is_none());
    }

    #[test]
    fn dry_run_flag_is_a_sibling_of_server() {
        let spec = minimal_spec();
        assert_eq!(spec.body(true)["dry_run"], true);
    }

    #[test]
    fn no_eip_omits_publicip() {
        let spec = minimal_spec();
        assert!(spec.body(false)["server"].get("publicip").is_none());
    }

    #[test]
    fn ecs_base_url_shape() {
        assert_eq!(
            ecs_base("ap-southeast-3", "proj-42"),
            "https://ecs.ap-southeast-3.myhuaweicloud.com/v1/proj-42"
        );
    }
}
