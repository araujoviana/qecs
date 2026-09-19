//! Integration: the signed HWC client against a local mock HTTP server.
use qecs::creds::Credentials;
use qecs::hwc::client::SignedClient;
use serde::Deserialize;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client() -> SignedClient {
    SignedClient::new(
        reqwest::Client::new(),
        Credentials {
            ak: "TESTAK".into(),
            sk: "TESTSK".into(),
            security_token: None,
        },
    )
}

static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Deserialize)]
struct Flavors {
    flavors: Vec<Flavor>,
}
#[derive(Debug, Deserialize)]
struct Flavor {
    name: String,
}

#[tokio::test]
async fn success_response_is_deserialized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/proj/cloudservers/flavors"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"flavors":[{"name":"s7n.2xlarge.2"}]})),
        )
        .mount(&server)
        .await;

    let url = format!("{}/v1/proj/cloudservers/flavors", server.uri());
    let got: Flavors = client()
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .unwrap();
    assert_eq!(got.flavors[0].name, "s7n.2xlarge.2");
}

#[tokio::test]
async fn every_request_carries_the_sdk_signature_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header_exists("x-sdk-date"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"flavors":[]})))
        .expect(1)
        .mount(&server)
        .await;

    let url = format!("{}/a", server.uri());
    let _: Flavors = client()
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    let auth = reqs[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(auth.starts_with("SDK-HMAC-SHA256 Access=TESTAK, SignedHeaders="));
    assert!(auth.contains("Signature="));
}

#[tokio::test]
async fn hwc_flat_error_body_becomes_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).set_body_json(
            serde_json::json!({"error_code":"Ecs.0114","error_msg":"flavor not found"}),
        ))
        .mount(&server)
        .await;

    let url = format!("{}/missing", server.uri());
    let err = client()
        .send_json::<Flavors>(reqwest::Method::GET, &url, None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 404);
    assert_eq!(err.code.as_deref(), Some("Ecs.0114"));
    assert!(err.message.contains("flavor not found"));
}

#[tokio::test]
async fn hwc_nested_error_body_becomes_api_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            serde_json::json!({"error":{"code":"APIGW.0301","message":"incorrect signature"}}),
        ))
        .mount(&server)
        .await;

    let url = format!("{}/x", server.uri());
    let body = serde_json::json!({"k": "v"});
    let err = client()
        .send_json::<Flavors>(reqwest::Method::POST, &url, Some(&body))
        .await
        .unwrap_err();
    assert_eq!(err.status, 400);
    assert_eq!(err.code.as_deref(), Some("APIGW.0301"));
    assert!(err.message.contains("incorrect signature"));
}

#[tokio::test]
async fn undecodable_success_body_is_an_error_not_a_panic() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>nope</html>"))
        .mount(&server)
        .await;

    let url = format!("{}/weird", server.uri());
    let err = client()
        .send_json::<Flavors>(reqwest::Method::GET, &url, None)
        .await
        .unwrap_err();
    assert_eq!(err.status, 200);
    assert!(err.message.contains("decoding response"));
}

#[tokio::test]
async fn post_body_is_sent_and_content_type_set() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_exists("content-type"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"flavors":[]})))
        .mount(&server)
        .await;

    let url = format!("{}/create", server.uri());
    let body = serde_json::json!({"server": {"name": "qecs-gpu-ab12"}});
    let _: Flavors = client()
        .send_json(reqwest::Method::POST, &url, Some(&body))
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(sent["server"]["name"], "qecs-gpu-ab12");
}

#[tokio::test]
async fn list_servers_shape_parses_into_flattened_server() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/P/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1,
            "servers": [{
                "id": "1bdf5b7a",
                "name": "qecs-normal-a1b2c3",
                "status": "ACTIVE",
                "OS-EXT-AZ:availability_zone": "ap-southeast-3a",
                "OS-EXT-STS:power_state": 1,
                "flavor": { "id": "s7n.2xlarge.2" },
                "addresses": {
                    "99dd236b": [
                        {"version":"4","addr":"192.168.0.42","OS-EXT-IPS:type":"fixed","OS-EXT-IPS:port_id":"af1b"},
                        {"version":"4","addr":"123.45.67.89","OS-EXT-IPS:type":"floating"}
                    ]
                }
            }]
        })))
        .mount(&server)
        .await;

    let url = format!("{}/v1/P/cloudservers/detail", server.uri());
    let body: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let s = qecs::hwc::ecs::Server::from_raw(&body["servers"][0]).unwrap();
    assert_eq!(s.az, "ap-southeast-3a");
    assert_eq!(s.flavor, "s7n.2xlarge.2");
    assert_eq!(s.private_ip.as_deref(), Some("192.168.0.42"));
    assert_eq!(s.public_ip.as_deref(), Some("123.45.67.89"));
    assert_eq!(s.port_id.as_deref(), Some("af1b"));
    assert_eq!(s.power_state, 1);
}

#[tokio::test]
async fn remote_console_response_yields_the_vnc_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/P/cloudservers/s1/remote_console"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "remote_console": { "type": "novnc", "protocol": "vnc",
                "url": "https://nova-novncproxy.example.myhuaweicloud.com:8002/vnc_auto.html?token=x" }
        })))
        .mount(&server)
        .await;

    let url = format!("{}/v1/P/cloudservers/s1/remote_console", server.uri());
    let body: serde_json::Value = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({"remote_console":{"protocol":"vnc","type":"novnc"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        qecs::hwc::ecs::console_url_from(&body).unwrap(),
        "https://nova-novncproxy.example.myhuaweicloud.com:8002/vnc_auto.html?token=x"
    );
}

#[tokio::test]
async fn empty_success_body_is_handled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/empty"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let url = format!("{}/empty", server.uri());
    let val: serde_json::Value = client()
        .send_json(reqwest::Method::POST, &url, None)
        .await
        .unwrap();
    assert_eq!(val, serde_json::Value::Null);

    let unit: () = client()
        .send_json(reqwest::Method::POST, &url, None)
        .await
        .unwrap();
    assert_eq!(unit, ());
}

#[tokio::test]
async fn ims_create_image_and_list_private_deserializes() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v2/cloudimages/action"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job_id": "job-image-123"
        })))
        .mount(&server)
        .await;

    let action_url = format!("{}/v2/cloudimages/action", server.uri());
    let body = serde_json::json!({
        "name": "qecs-gpu-test",
        "instance_id": "srv-1"
    });
    let resp: serde_json::Value = client()
        .send_json(reqwest::Method::POST, &action_url, Some(&body))
        .await
        .unwrap();
    assert_eq!(resp["job_id"], "job-image-123");

    Mock::given(method("GET"))
        .and(path("/v2/cloudimages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "images": [
                {
                    "id": "img-baked-1",
                    "name": "qecs-gpu-20260906-1200",
                    "__os_version": "Ubuntu 22.04",
                    "min_disk": 40,
                    "status": "active",
                    "__imagetype": "private",
                    "created_at": "2026-09-06T12:00:00Z"
                }
            ]
        })))
        .mount(&server)
        .await;

    let list_url = format!(
        "{}/v2/cloudimages?__imagetype=private&status=active&limit=100",
        server.uri()
    );
    let got: serde_json::Value = client()
        .send_json(reqwest::Method::GET, &list_url, None)
        .await
        .unwrap();
    assert_eq!(got["images"][0]["name"], "qecs-gpu-20260906-1200");
}

#[tokio::test]
async fn telemetry_records_one_hwc_call_per_request() {
    let _guard = TEST_LOCK.lock().await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": 1})))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let tel = {
        unsafe {
            std::env::set_var("HOME", tmp.path());
            std::env::remove_var("XDG_STATE_HOME");
        }
        qecs::telemetry::Telemetry::init(true, "test").expect("init telemetry")
    };

    let url = format!("{}/v1/test", server.uri());
    let client = client().with_telemetry(Some(tel.clone()));
    let _: serde_json::Value = client
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .unwrap();

    let count = tel.finish(0);
    assert!(count >= 2);

    let traces_dir = tmp.path().join(".local/state/qecs/traces");
    let file = std::fs::read_dir(&traces_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .expect("one trace file");
    let body = std::fs::read_to_string(file).unwrap();
    let hwc_line: serde_json::Value = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .find(|v: &serde_json::Value| v["kind"] == "hwc_call")
        .expect("hwc_call event present");

    assert_eq!(hwc_line["method"], "GET");
    assert_eq!(hwc_line["status"], 200);
    assert_eq!(hwc_line["path"], "/v1/test");
    assert!(hwc_line["total_ms"].as_u64().is_some());
}

#[tokio::test]
async fn telemetry_hwc_call_on_transport_error() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let tel = {
        unsafe {
            std::env::set_var("HOME", tmp.path());
            std::env::remove_var("XDG_STATE_HOME");
        }
        qecs::telemetry::Telemetry::init(true, "test").expect("init telemetry")
    };

    let url = "http://127.0.0.1:1/v1/dead-endpoint";
    let client = client().with_telemetry(Some(tel.clone()));
    let res: Result<serde_json::Value, _> = client.send_json(reqwest::Method::GET, url, None).await;
    assert!(res.is_err());

    tel.finish(1);

    let traces_dir = tmp.path().join(".local/state/qecs/traces");
    let file = std::fs::read_dir(&traces_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .expect("one trace file");
    let body = std::fs::read_to_string(file).unwrap();
    let hwc_line: serde_json::Value = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .find(|v: &serde_json::Value| v["kind"] == "hwc_call")
        .expect("hwc_call event present on transport error");

    assert_eq!(hwc_line["method"], "GET");
    assert_eq!(hwc_line["status"], 0);
    assert!(hwc_line["total_ms"].as_u64().is_some());
}

fn integration_ctx() -> qecs::ctx::Ctx {
    qecs::ctx::Ctx {
        config: qecs::config::Config::default(),
        creds: Credentials {
            ak: "TESTAK".into(),
            sk: "TESTSK".into(),
            security_token: None,
        },
        http: reqwest::Client::new(),
        global: qecs::cli::GlobalArgs {
            region: Some("ap-southeast-3".into()),
            ..Default::default()
        },
        telemetry: None,
    }
}

fn sample_record(id: &str, name: &str, eip: Option<&str>) -> qecs::state::VmRecord {
    qecs::state::VmRecord {
        id: id.to_string(),
        name: name.to_string(),
        preset: "gpu".into(),
        flavor: "pi2.4xlarge.4".into(),
        region: "ap-southeast-3".into(),
        az: "ap-southeast-3a".into(),
        eip: eip.map(str::to_string),
        private_ip: Some("192.168.0.10".into()),
        created_at: "2026-09-18T00:00:00Z".into(),
        ttl_secs: 7200,
        connect_port: None,
        job: None,
        tags: vec![],
    }
}

async fn mock_iam_project(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v3/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "projects": [
                {
                    "id": "proj-42",
                    "name": "ap-southeast-3",
                    "domain_id": "dom-1"
                }
            ]
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn test_gc_removes_stale_local_record_when_server_absent_from_cloud() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("qecs/vms.json");
    let store = qecs::state::StateStore::at(store_path);
    store
        .upsert(sample_record("id-stale", "qecs-stale", Some("1.2.3.4")))
        .unwrap();

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/proj-42/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 0,
            "servers": []
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/publicips"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "publicips": []
        })))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let stats = qecs::lifecycle::reconcile_and_purge(&ctx, false)
        .await
        .unwrap();

    assert_eq!(stats.removed_from_state, vec!["qecs-stale"]);
    assert!(store.get("id-stale").unwrap().is_none());
    assert!(store.get("qecs-stale").unwrap().is_none());

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}

#[tokio::test]
async fn test_gc_purges_shutoff_server() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("qecs/vms.json");
    let store = qecs::state::StateStore::at(store_path);
    store
        .upsert(sample_record("id-dead", "qecs-dead", None))
        .unwrap();

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/proj-42/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1,
            "servers": [{
                "id": "id-dead",
                "name": "qecs-dead",
                "status": "SHUTOFF",
                "OS-EXT-AZ:availability_zone": "ap-southeast-3a",
                "OS-EXT-STS:power_state": 4,
                "flavor": { "id": "s7n.2xlarge.2" },
                "tags": ["managed-by=qecs"],
                "addresses": {}
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/proj-42/cloudservers/delete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job_id": "job-del-dead"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/jobs/job-del-dead"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS",
            "entities": {}
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/publicips"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "publicips": []
        })))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let stats = qecs::lifecycle::reconcile_and_purge(&ctx, false)
        .await
        .unwrap();

    assert_eq!(stats.deleted_from_cloud, vec!["id-dead"]);
    assert!(store.get("id-dead").unwrap().is_none());

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}

#[tokio::test]
async fn test_gc_purges_untracked_active_server_with_force() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("qecs/vms.json");
    let store = qecs::state::StateStore::at(store_path);

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/proj-42/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1,
            "servers": [{
                "id": "id-orphan-act",
                "name": "qecs-orphan-act",
                "status": "ACTIVE",
                "OS-EXT-AZ:availability_zone": "ap-southeast-3a",
                "OS-EXT-STS:power_state": 1,
                "flavor": { "id": "s7n.2xlarge.2" },
                "tags": ["managed-by=qecs"],
                "addresses": {}
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/proj-42/cloudservers/delete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job_id": "job-del-act"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/jobs/job-del-act"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS",
            "entities": {}
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/publicips"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "publicips": []
        })))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let stats = qecs::lifecycle::reconcile_and_purge(&ctx, true)
        .await
        .unwrap();

    assert_eq!(stats.deleted_from_cloud, vec!["id-orphan-act"]);
    assert!(stats.untracked_active_kept.is_empty());
    assert!(store.get("id-orphan-act").unwrap().is_none());

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}

#[tokio::test]
async fn test_gc_keeps_untracked_active_server_without_force() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/proj-42/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1,
            "servers": [{
                "id": "id-orphan-act",
                "name": "qecs-orphan-act",
                "status": "ACTIVE",
                "OS-EXT-AZ:availability_zone": "ap-southeast-3a",
                "OS-EXT-STS:power_state": 1,
                "flavor": { "id": "s7n.2xlarge.2" },
                "tags": ["managed-by=qecs"],
                "addresses": {}
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/publicips"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "publicips": []
        })))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let stats = qecs::lifecycle::reconcile_and_purge(&ctx, false)
        .await
        .unwrap();

    assert!(stats.deleted_from_cloud.is_empty());
    assert_eq!(stats.untracked_active_kept, vec!["qecs-orphan-act"]);

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}

#[tokio::test]
async fn test_gc_purges_untracked_free_eip() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/proj-42/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 0,
            "servers": []
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/publicips"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "publicips": [{
                "id": "eip-free-99",
                "status": "FREE",
                "public_ip_address": "200.1.2.3"
            }]
        })))
        .mount(&server)
        .await;

    Mock::given(method("DELETE"))
        .and(path("/v1/proj-42/publicips/eip-free-99"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let stats = qecs::lifecycle::reconcile_and_purge(&ctx, false)
        .await
        .unwrap();

    assert_eq!(stats.deleted_eips, vec!["200.1.2.3"]);

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}

#[tokio::test]
async fn test_destroy_vm_preserves_state_on_async_job_failure() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("qecs/vms.json");
    let store = qecs::state::StateStore::at(store_path);
    store
        .upsert(sample_record("id-fail", "qecs-fail", None))
        .unwrap();

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/proj-42/cloudservers/delete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job_id": "job-fail-1"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/jobs/job-fail-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "FAIL",
            "fail_reason": "resource locked"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1,
            "servers": [{
                "id": "id-fail",
                "name": "qecs-fail",
                "status": "ACTIVE",
                "OS-EXT-AZ:availability_zone": "ap-southeast-3a",
                "OS-EXT-STS:power_state": 1,
                "flavor": { "id": "s7n.2xlarge.2" },
                "tags": ["managed-by=qecs"],
                "addresses": {}
            }]
        })))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let res = qecs::commands::run::destroy_vm(&ctx, "id-fail", "qecs-fail").await;

    assert!(res.is_err());
    assert!(res.unwrap_err().to_string().contains("resource locked"));
    // State must still be preserved!
    assert!(store.get("id-fail").unwrap().is_some());
    assert!(store.get("qecs-fail").unwrap().is_some());

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}

#[tokio::test]
async fn test_destroy_vm_cleans_state_on_success() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let store_path = tmp.path().join("qecs/vms.json");
    let store = qecs::state::StateStore::at(store_path);
    store
        .upsert(sample_record("id-succ", "qecs-succ", None))
        .unwrap();

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("POST"))
        .and(path("/v1/proj-42/cloudservers/delete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job_id": "job-succ-1"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/v1/proj-42/jobs/job-succ-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "SUCCESS",
            "entities": {}
        })))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let res = qecs::commands::run::destroy_vm(&ctx, "id-succ", "qecs-succ").await;

    assert!(res.is_ok());
    assert!(store.get("id-succ").unwrap().is_none());
    assert!(store.get("qecs-succ").unwrap().is_none());

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}

#[tokio::test]
async fn test_ls_surfaces_untracked_active_orphan() {
    let _guard = TEST_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();

    let server = MockServer::start().await;
    unsafe {
        std::env::set_var("QECS_TEST_ENDPOINT", server.uri());
        std::env::set_var("XDG_STATE_HOME", tmp.path());
    }

    mock_iam_project(&server).await;
    Mock::given(method("GET"))
        .and(path("/v1/proj-42/cloudservers/detail"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "count": 1,
            "servers": [{
                "id": "id-orphan-ls",
                "name": "qecs-orphan-ls",
                "status": "ACTIVE",
                "OS-EXT-AZ:availability_zone": "ap-southeast-3a",
                "OS-EXT-STS:power_state": 1,
                "flavor": { "id": "pi2.4xlarge.4" },
                "tags": ["managed-by=qecs"],
                "addresses": {
                    "vpc-1": [{
                        "version": "4",
                        "addr": "1.2.3.4",
                        "OS-EXT-IPS:type": "floating"
                    }]
                }
            }]
        })))
        .mount(&server)
        .await;

    let ctx = integration_ctx();
    let res = qecs::commands::ls::cmd_ls(Some(&ctx), false, false).await;
    assert!(res.is_ok());

    let json_res = qecs::commands::ls::cmd_ls(Some(&ctx), false, true).await;
    assert!(json_res.is_ok());

    unsafe {
        std::env::remove_var("QECS_TEST_ENDPOINT");
        std::env::remove_var("XDG_STATE_HOME");
    }
}
