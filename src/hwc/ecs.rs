//! Elastic Cloud Server - server lifecycle models plus create + dry-run, and the
//! get / list / wait / delete / VNC-console read calls with their address helpers.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use crate::hwc::wait::{Poll, PollConfig, poll_until};
use crate::telemetry::Telemetry;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// One ECS server, flattened from HWC's raw response (the `OS-EXT-*` colon keys
/// make `#[derive(Deserialize)]` painful, so `from_raw` walks the `Value` by hand).
#[derive(Debug, Clone, Serialize)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub status: String,
    pub az: String,
    pub flavor: String,
    pub private_ip: Option<String>,
    pub public_ip: Option<String>,
    pub port_id: Option<String>,
    pub power_state: i32,
    pub tags: Vec<String>,
}

impl Server {
    /// Build a `Server` from one `servers[]` element (or the `server` object of a
    /// get). `id` / `name` / `status` are required; everything else defaults.
    pub fn from_raw(v: &Value) -> anyhow::Result<Server> {
        let req = |key: &str| -> anyhow::Result<String> {
            v.get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
                .with_context(|| format!("server response missing `{key}`"))
        };
        let (private_ip, public_ip, port_id) = parse_addresses(&v["addresses"]);
        let tags = v["tags"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Server {
            id: req("id")?,
            name: req("name")?,
            status: req("status")?,
            az: v["OS-EXT-AZ:availability_zone"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            flavor: v["flavor"]["id"].as_str().unwrap_or_default().to_string(),
            private_ip,
            public_ip,
            port_id,
            power_state: v["OS-EXT-STS:power_state"].as_i64().unwrap_or(0) as i32,
            tags,
        })
    }
}

/// Walk a server's `addresses` object (keyed by VPC id, each value an array of
/// entries) and pull out `(private_ip, public_ip, port_id)`. `fixed` entries give
/// the private IP + port id, `floating` entries give the public IP. First match of
/// each wins; never panics on `{}` or missing keys.
pub fn parse_addresses(v: &Value) -> (Option<String>, Option<String>, Option<String>) {
    let mut private_ip = None;
    let mut public_ip = None;
    let mut port_id = None;
    let Some(by_vpc) = v.as_object() else {
        return (private_ip, public_ip, port_id);
    };
    for entries in by_vpc.values() {
        let Some(entries) = entries.as_array() else {
            continue;
        };
        for entry in entries {
            match entry["OS-EXT-IPS:type"].as_str() {
                Some("fixed") if private_ip.is_none() => {
                    private_ip = entry["addr"].as_str().map(str::to_string);
                    port_id = entry["OS-EXT-IPS:port_id"].as_str().map(str::to_string);
                }
                Some("floating") if public_ip.is_none() => {
                    public_ip = entry["addr"].as_str().map(str::to_string);
                }
                _ => {}
            }
        }
    }
    (private_ip, public_ip, port_id)
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

/// `GET {ecs_base}/cloudservers/{id}` -> `{"server":{...}}`.
pub async fn get_server(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    id: &str,
) -> anyhow::Result<Server> {
    let url = format!("{}/cloudservers/{id}", ecs_base(region, project_id));
    let resp: Value = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Server::from_raw(&resp["server"])
}

/// `GET {ecs_base}/cloudservers/detail?limit=200` -> `{"servers":[...]}`.
pub async fn list_servers(
    c: &SignedClient,
    region: &str,
    project_id: &str,
) -> anyhow::Result<Vec<Server>> {
    let url = format!(
        "{}/cloudservers/detail?limit=200",
        ecs_base(region, project_id)
    );
    let resp: Value = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    resp["servers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(Server::from_raw)
        .collect()
}

/// Poll `get_server` until the server is `ACTIVE`; bail on `ERROR`.
pub async fn wait_active(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    id: &str,
    cfg: &PollConfig,
    tel: Option<&Telemetry>,
) -> anyhow::Result<Server> {
    let tel = tel.or(c.telemetry.as_ref());
    poll_until(
        cfg,
        &format!("server {id} ACTIVE"),
        || async {
            let s = get_server(c, region, project_id, id).await?;
            Ok(match s.status.as_str() {
                "ACTIVE" => Poll::Ready(s),
                "ERROR" => anyhow::bail!("server {id} entered ERROR"),
                _ => Poll::Pending,
            })
        },
        tel,
    )
    .await
}

/// `POST {ecs_base}/cloudservers/delete` (batch) -> async `job_id`.
pub async fn delete_servers(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    ids: &[&str],
) -> anyhow::Result<String> {
    let url = format!("{}/cloudservers/delete", ecs_base(region, project_id));
    let body = delete_body(ids);
    let resp: CreateResp = c
        .send_json(reqwest::Method::POST, &url, Some(&body))
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.job_id)
}

/// `POST {ecs_base}/cloudservers/{id}/remote_console` -> the one-time noVNC URL.
pub async fn remote_console(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    id: &str,
) -> anyhow::Result<String> {
    let url = format!(
        "{}/cloudservers/{id}/remote_console",
        ecs_base(region, project_id)
    );
    let body = json!({ "remote_console": { "protocol": "vnc", "type": "novnc" } });
    let resp: Value = c
        .send_json(reqwest::Method::POST, &url, Some(&body))
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    console_url_from(&resp)
}

/// Pure: batch-delete payload with EIP + data-volume cleanup on.
pub(crate) fn delete_body(ids: &[&str]) -> Value {
    json!({
        "servers": ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>(),
        "delete_publicip": true,
        "delete_volume": true,
    })
}

/// Pure: pull `remote_console.url` out of a console response, erroring if absent.
/// `pub` (not `pub(crate)`) so `tests/hwc_integration.rs` can exercise it against
/// a mocked HTTP round-trip.
pub fn console_url_from(v: &Value) -> anyhow::Result<String> {
    v["remote_console"]["url"]
        .as_str()
        .map(str::to_string)
        .context("remote_console response missing `remote_console.url`")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_addresses_splits_fixed_and_floating() {
        let v = serde_json::json!({
            "vpc-uuid": [
                {"version":"4","addr":"192.168.0.42","OS-EXT-IPS:type":"fixed","OS-EXT-IPS:port_id":"port-9"},
                {"version":"4","addr":"123.45.67.89","OS-EXT-IPS:type":"floating"}
            ]
        });
        let (priv_ip, pub_ip, port) = parse_addresses(&v);
        assert_eq!(priv_ip.as_deref(), Some("192.168.0.42"));
        assert_eq!(pub_ip.as_deref(), Some("123.45.67.89"));
        assert_eq!(port.as_deref(), Some("port-9"));
    }

    #[test]
    fn parse_addresses_tolerates_empty_object() {
        assert_eq!(parse_addresses(&serde_json::json!({})), (None, None, None));
    }

    #[test]
    fn server_from_raw_flattens_ext_fields() {
        let raw = serde_json::json!({
            "id":"s1","name":"qecs-normal-ab12","status":"ACTIVE",
            "OS-EXT-AZ:availability_zone":"ap-southeast-3a",
            "OS-EXT-STS:power_state":1,
            "flavor":{"id":"s7n.2xlarge.2"},
            "addresses":{}
        });
        let s = Server::from_raw(&raw).unwrap();
        assert_eq!(s.az, "ap-southeast-3a");
        assert_eq!(s.flavor, "s7n.2xlarge.2");
        assert_eq!(s.power_state, 1);
    }

    #[test]
    fn server_from_raw_errors_without_id() {
        let raw = serde_json::json!({ "name": "x", "status": "ACTIVE" });
        assert!(Server::from_raw(&raw).is_err());
    }

    #[test]
    fn delete_body_requests_publicip_and_volume_cleanup() {
        let b = delete_body(&["s1", "s2"]);
        assert_eq!(b["servers"][1]["id"], "s2");
        assert_eq!(b["delete_publicip"], true);
        assert_eq!(b["delete_volume"], true);
    }

    #[test]
    fn console_url_from_extracts_url_or_errors() {
        let ok = serde_json::json!({"remote_console":{"url":"https://x/vnc_auto.html?token=t"}});
        assert_eq!(
            console_url_from(&ok).unwrap(),
            "https://x/vnc_auto.html?token=t"
        );
        assert!(console_url_from(&serde_json::json!({"remote_console":{}})).is_err());
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
