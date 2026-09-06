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
