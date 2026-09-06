//! Read-only. QECS_AK=.. QECS_SK=.. QECS_REGION=ap-southeast-3 \
//!   cargo test --test hwc_resources_live -- --ignored --nocapture
use qecs::creds::Credentials;
use qecs::hwc::{client::SignedClient, ecs, iam, images, vpc};

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
async fn discover_project_live() {
    let p = iam::discover_project(&client(), &region()).await.unwrap();
    eprintln!("project = {} domain = {}", p.id, p.domain_id);
    assert_eq!(p.id.len(), 32);
}

#[tokio::test]
#[ignore]
async fn resolve_image_live() {
    let img = images::resolve_image(&client(), &region(), images::Platform::Ubuntu)
        .await
        .unwrap();
    eprintln!(
        "ubuntu image = {} ({}) min_disk {}",
        img.id, img.os_version, img.min_disk
    );
    assert!(img.os_version.contains("Ubuntu"));
}

#[tokio::test]
#[ignore]
async fn list_vpcs_and_servers_live() {
    let p = iam::discover_project(&client(), &region()).await.unwrap();
    let vpcs = vpc::list_vpcs(&client(), &region(), &p.id).await.unwrap();
    let servers = ecs::list_servers(&client(), &region(), &p.id)
        .await
        .unwrap();
    eprintln!("{} vpcs, {} servers", vpcs.len(), servers.len());
}
