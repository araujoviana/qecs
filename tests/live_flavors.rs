//! Real network test. Run with:
//!   QECS_AK=.. QECS_SK=.. QECS_PROJECT_ID=.. QECS_REGION=ap-southeast-3 \
//!   cargo test --test live_flavors -- --ignored --nocapture
use qecs::creds::Credentials;
use qecs::hwc::{client::SignedClient, flavors};

#[tokio::test]
#[ignore]
async fn lists_flavors_live() {
    let ak = std::env::var("QECS_AK").expect("QECS_AK");
    let sk = std::env::var("QECS_SK").expect("QECS_SK");
    let project = std::env::var("QECS_PROJECT_ID").expect("QECS_PROJECT_ID");
    let region = std::env::var("QECS_REGION").unwrap_or_else(|_| "ap-southeast-3".into());

    let client = SignedClient::new(
        reqwest::Client::new(),
        Credentials {
            ak,
            sk,
            security_token: None,
        },
    );
    let flavors = flavors::list_flavors(&client, &region, &project)
        .await
        .unwrap();
    assert!(!flavors.is_empty(), "expected at least one flavor");
    eprintln!(
        "got {} flavors; first = {:?}",
        flavors.len(),
        flavors.first()
    );
}
