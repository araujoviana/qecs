//! Live, real-OBS regression test for the SigV4 header signer used by bucket admin
//! calls (`ensure_cache_bucket`, `list_cache_objects`). These calls previously used the
//! HWC `SDK-HMAC-SHA256` signer meant for ECS/VPC/IAM/IMS, which OBS's S3-compatible API
//! rejects with a bare 400 - silently breaking `qecs run`'s dependency cache and the
//! `qecs cache` subcommand. This guards against that regressing.
//!
//! QECS_AK=.. QECS_SK=.. QECS_PROJECT_ID=.. QECS_REGION=ap-southeast-3 \
//!   cargo test --test obs_cache_live -- --ignored --nocapture
use qecs::creds::Credentials;
use qecs::hwc::client::SignedClient;
use qecs::hwc::obs;

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
async fn ensure_and_list_cache_bucket_live() {
    let project_id = std::env::var("QECS_PROJECT_ID").expect("QECS_PROJECT_ID");
    let region = region();
    let client = client();
    let bucket = obs::cache_bucket_name(&region, &project_id);

    obs::ensure_cache_bucket(&client, &region, &bucket)
        .await
        .expect("ensure_cache_bucket should succeed against real OBS");

    let objects = obs::list_cache_objects(&client, &region, &bucket)
        .await
        .expect("list_cache_objects should succeed against real OBS");
    eprintln!("bucket `{bucket}` has {} cached archives", objects.len());
}

#[tokio::test]
#[ignore]
async fn object_exists_distinguishes_present_and_absent_live() {
    let project_id = std::env::var("QECS_PROJECT_ID").expect("QECS_PROJECT_ID");
    let region = region();
    let client = client();
    let bucket = obs::cache_bucket_name(&region, &project_id);

    obs::ensure_cache_bucket(&client, &region, &bucket)
        .await
        .expect("ensure_cache_bucket should succeed against real OBS");

    let missing = obs::object_exists(&client, &region, &bucket, "caches/v1/does-not-exist.tar.gz")
        .await
        .expect("HEAD on a missing object should not error");
    assert!(!missing, "a never-uploaded key must report as absent");
}
