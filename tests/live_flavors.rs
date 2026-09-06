//! Real network tests. Run with:
//!   QECS_AK=.. QECS_SK=.. QECS_PROJECT_ID=.. QECS_REGION=ap-southeast-3 \
//!   cargo test --test live_flavors -- --ignored --nocapture
use qecs::creds::Credentials;
use qecs::hwc::{client::SignedClient, flavors};

fn client() -> SignedClient {
    SignedClient::new(
        reqwest::Client::new(),
        Credentials {
            ak: std::env::var("QECS_AK").expect("QECS_AK"),
            sk: std::env::var("QECS_SK").expect("QECS_SK"),
            security_token: None,
        },
    )
}

fn region() -> String {
    std::env::var("QECS_REGION").unwrap_or_else(|_| "ap-southeast-3".into())
}

#[tokio::test]
#[ignore]
async fn lists_flavors_live() {
    let project = std::env::var("QECS_PROJECT_ID").expect("QECS_PROJECT_ID");
    let flavors = flavors::list_flavors(&client(), &region(), &project)
        .await
        .unwrap();
    assert!(!flavors.is_empty(), "expected at least one flavor");
    eprintln!(
        "got {} flavors; first = {:?}",
        flavors.len(),
        flavors.first()
    );
}

#[tokio::test]
#[ignore]
async fn find_flavor_live() {
    let project = std::env::var("QECS_PROJECT_ID").expect("QECS_PROJECT_ID");
    let region = region();
    let az = format!("{region}a");

    let hit = flavors::find_flavor(&client(), &region, &project, "s7n.2xlarge.2", &az)
        .await
        .unwrap();
    eprintln!("find_flavor(s7n.2xlarge.2, {az}) = {hit:?}");
    assert!(hit.is_some(), "s7n.2xlarge.2 should be findable in {az}");

    let missing = flavors::find_flavor(&client(), &region, &project, "does.not.exist.9", &az)
        .await
        .unwrap();
    assert!(missing.is_none());
}
