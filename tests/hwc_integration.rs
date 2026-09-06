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
