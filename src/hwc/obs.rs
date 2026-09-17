//! Huawei Cloud OBS (Object Storage Service) client for dependency and build caching.
//!
//! Provides bucket lifecycle management, object listing, deletion, and
//! S3-compatible V4 presigned URL generation for intra-region 1-10 Gbps VPC transfers.

use anyhow::Context;
use hmac::{Hmac, Mac};
use reqwest::Method;
use sha2::{Digest, Sha256};

use crate::creds::Credentials;
use crate::hwc::client::SignedClient;
use crate::hwc::sign::pct;

type HmacSha256 = Hmac<Sha256>;

/// Derive the regional OBS cache bucket name for a project.
/// OBS bucket names must be lowercase alphanumeric and hyphens, between 3 and 63 chars.
pub fn cache_bucket_name(region: &str, project_id: &str) -> String {
    let sanitized_project: String = project_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();

    format!("qecs-cache-{region}-{sanitized_project}")
}

/// URL of an OBS bucket.
pub fn bucket_url(region: &str, bucket: &str) -> String {
    format!("https://{bucket}.obs.{region}.myhuaweicloud.com")
}

/// Metadata for a cached object in OBS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObsObject {
    pub key: String,
    pub size_bytes: u64,
    pub last_modified: String,
}

/// Extract `<tag>value</tag>` content from XML snippet.
fn extract_xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open_tag = format!("<{tag}>");
    let close_tag = format!("</{tag}>");
    let start = xml.find(&open_tag)? + open_tag.len();
    let end = xml[start..].find(&close_tag)?;
    Some(&xml[start..start + end])
}

/// Parse the XML response from an OBS/S3 `GET /` ListBucket call.
pub fn parse_list_bucket_xml(xml: &str) -> Vec<ObsObject> {
    let mut objects = Vec::new();
    let mut cursor = xml;

    while let Some(start_idx) = cursor.find("<Contents>") {
        let contents_start = start_idx + "<Contents>".len();
        let contents_end = match cursor[contents_start..].find("</Contents>") {
            Some(end) => end,
            None => break,
        };
        let block = &cursor[contents_start..contents_start + contents_end];

        let key = extract_xml_tag(block, "Key").unwrap_or_default().trim();
        let size_str = extract_xml_tag(block, "Size").unwrap_or("0").trim();
        let size_bytes = size_str.parse::<u64>().unwrap_or(0);
        let last_modified = extract_xml_tag(block, "LastModified")
            .unwrap_or_default()
            .trim();

        if !key.is_empty() {
            objects.push(ObsObject {
                key: key.to_string(),
                size_bytes,
                last_modified: last_modified.to_string(),
            });
        }

        cursor = &cursor[contents_start + contents_end + "</Contents>".len()..];
    }

    objects
}

/// Ensure the cache bucket exists in the given region, configuring an auto-expiring lifecycle policy if created.
pub async fn ensure_cache_bucket(
    client: &SignedClient,
    region: &str,
    bucket: &str,
) -> anyhow::Result<()> {
    let url = format!("{}/", bucket_url(region, bucket));

    // 1. Check if bucket already exists
    let (status, _) = client
        .send_raw(Method::GET, &url, None, None)
        .await
        .unwrap_or((0, String::new()));

    if status == 200 {
        return Ok(());
    }

    // 2. Create bucket via PUT
    let create_xml = format!(
        "<CreateBucketConfiguration xmlns=\"http://obs.myhuaweicloud.com/doc/2015-06-30/\">\n  \
           <LocationConstraint>{region}</LocationConstraint>\n\
         </CreateBucketConfiguration>"
    );

    let (put_status, body) = client
        .send_raw(
            Method::PUT,
            &url,
            Some("application/xml"),
            Some(create_xml.as_bytes()),
        )
        .await
        .context("creating OBS cache bucket")?;

    if put_status != 200 && put_status != 204 {
        // BucketAlreadyOwnedByYou is ok
        if !body.contains("BucketAlreadyOwnedByYou") {
            anyhow::bail!(
                "failed to create OBS bucket `{bucket}`: status {put_status}, response: {body}"
            );
        }
    }

    // 3. Configure 14-day auto-expire lifecycle policy so caches never accumulate ongoing costs
    configure_lifecycle(client, region, bucket, 14).await?;

    Ok(())
}

/// Configure an auto-expiring lifecycle rule on an OBS bucket.
pub async fn configure_lifecycle(
    client: &SignedClient,
    region: &str,
    bucket: &str,
    days: u32,
) -> anyhow::Result<()> {
    let url = format!("{}/?lifecycle", bucket_url(region, bucket));
    let lifecycle_xml = format!(
        "<LifecycleConfiguration xmlns=\"http://obs.myhuaweicloud.com/doc/2015-06-30/\">\n  \
           <Rule>\n    \
             <ID>qecs-cache-auto-expire</ID>\n    \
             <Prefix></Prefix>\n    \
             <Status>Enabled</Status>\n    \
             <Expiration>\n      \
               <Days>{days}</Days>\n    \
             </Expiration>\n  \
           </Rule>\n\
         </LifecycleConfiguration>"
    );

    let (status, body) = client
        .send_raw(
            Method::PUT,
            &url,
            Some("application/xml"),
            Some(lifecycle_xml.as_bytes()),
        )
        .await
        .context("setting OBS bucket lifecycle")?;

    if status != 200 && status != 204 {
        anyhow::bail!(
            "failed to set OBS lifecycle on `{bucket}`: status {status}, response: {body}"
        );
    }

    Ok(())
}

/// List all cached objects in the OBS bucket.
pub async fn list_cache_objects(
    client: &SignedClient,
    region: &str,
    bucket: &str,
) -> anyhow::Result<Vec<ObsObject>> {
    let url = format!("{}/", bucket_url(region, bucket));
    let (status, body) = client
        .send_raw(Method::GET, &url, None, None)
        .await
        .context("listing OBS cache objects")?;

    if status == 404 {
        return Ok(Vec::new());
    }

    if status != 200 {
        anyhow::bail!(
            "failed to list OBS objects in `{bucket}`: status {status}, response: {body}"
        );
    }

    Ok(parse_list_bucket_xml(&body))
}

/// Delete a specific cache object from OBS.
pub async fn delete_cache_object(
    client: &SignedClient,
    region: &str,
    bucket: &str,
    key: &str,
) -> anyhow::Result<()> {
    let url = format!(
        "{}/{}",
        bucket_url(region, bucket),
        key.trim_start_matches('/')
    );
    let (status, body) = client
        .send_raw(Method::DELETE, &url, None, None)
        .await
        .context("deleting OBS cache object")?;

    if status != 200 && status != 204 && status != 404 {
        anyhow::bail!("failed to delete OBS object `{key}`: status {status}, response: {body}");
    }

    Ok(())
}

/// Delete all cached objects in an OBS bucket. Returns the number of objects deleted.
pub async fn delete_all_cache_objects(
    client: &SignedClient,
    region: &str,
    bucket: &str,
) -> anyhow::Result<usize> {
    let objects = list_cache_objects(client, region, bucket).await?;
    let count = objects.len();

    for obj in objects {
        delete_cache_object(client, region, bucket, &obj.key).await?;
    }

    Ok(count)
}

/// Completely destroy the cache bucket and all its contents.
pub async fn destroy_cache_bucket(
    client: &SignedClient,
    region: &str,
    bucket: &str,
) -> anyhow::Result<()> {
    // 1. Delete all objects first (S3/OBS requirement before bucket deletion)
    delete_all_cache_objects(client, region, bucket).await?;

    // 2. Delete bucket
    let url = format!("{}/", bucket_url(region, bucket));
    let (status, body) = client
        .send_raw(Method::DELETE, &url, None, None)
        .await
        .context("destroying OBS bucket")?;

    if status != 200 && status != 204 && status != 404 {
        anyhow::bail!("failed to delete OBS bucket `{bucket}`: status {status}, response: {body}");
    }

    Ok(())
}

/// Generate an S3 V4 presigned URL for direct, intra-region ECS-to-OBS data transfers.
///
/// Valid for `expires_secs` (e.g. 900s / 15m), safe to curl on remote VMs without
/// embedding permanent AK/SK credentials.
pub fn generate_presigned_url(
    creds: &Credentials,
    region: &str,
    bucket: &str,
    key: &str,
    method: &str,
    expires_secs: u64,
) -> String {
    let now = chrono::Utc::now();
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();
    let host = format!("{bucket}.obs.{region}.myhuaweicloud.com");
    let credential_scope = format!("{date_stamp}/{region}/s3/aws4_request");
    let credential = format!("{}/{}", creds.ak, credential_scope);

    // Canonical Query Parameters
    let mut query_params = vec![
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
        ("X-Amz-Credential", credential),
        ("X-Amz-Date", amz_date.clone()),
        ("X-Amz-Expires", expires_secs.to_string()),
        ("X-Amz-SignedHeaders", "host".to_string()),
    ];
    if let Some(ref tok) = creds.security_token {
        query_params.push(("X-Amz-Security-Token", tok.clone()));
    }
    query_params.sort_by(|a, b| a.0.cmp(b.0));

    let canonical_query_str = query_params
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect::<Vec<_>>()
        .join("&");

    let canonical_uri = if key.starts_with('/') {
        key.to_string()
    } else {
        format!("/{key}")
    };

    let canonical_headers = format!("host:{host}\n");
    let signed_headers = "host";

    // Canonical Request
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query_str}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
    );

    let hashed_canonical_request = hex::encode(Sha256::digest(canonical_request.as_bytes()));

    // String to Sign
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{hashed_canonical_request}");

    // Derive Signing Key
    let k_secret = format!("AWS4{}", creds.sk);
    let mut mac =
        HmacSha256::new_from_slice(k_secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(date_stamp.as_bytes());
    let k_date = mac.finalize().into_bytes();

    let mut mac = HmacSha256::new_from_slice(&k_date).unwrap();
    mac.update(region.as_bytes());
    let k_region = mac.finalize().into_bytes();

    let mut mac = HmacSha256::new_from_slice(&k_region).unwrap();
    mac.update(b"s3");
    let k_service = mac.finalize().into_bytes();

    let mut mac = HmacSha256::new_from_slice(&k_service).unwrap();
    mac.update(b"aws4_request");
    let k_signing = mac.finalize().into_bytes();

    let mut mac = HmacSha256::new_from_slice(&k_signing).unwrap();
    mac.update(string_to_sign.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    format!("https://{host}{canonical_uri}?{canonical_query_str}&X-Amz-Signature={signature}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_bucket_name_formatting() {
        assert_eq!(
            cache_bucket_name("ap-southeast-3", "0a1b2c3d4e5f6g7h"),
            "qecs-cache-ap-southeast-3-0a1b2c3d"
        );
        assert_eq!(
            cache_bucket_name("sa-brazil-1", "PROJECT_12345"),
            "qecs-cache-sa-brazil-1-project1"
        );
    }

    #[test]
    fn parse_list_bucket_xml_extracts_objects() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://obs.myhuaweicloud.com/doc/2015-06-30/">
  <Name>qecs-cache-ap-southeast-3-demo</Name>
  <Prefix></Prefix>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>caches/uv-a1b2c3d4.tar.gz</Key>
    <LastModified>2026-09-13T01:23:45.000Z</LastModified>
    <ETag>"d41d8cd98f00b204e9800998ecf8427e"</ETag>
    <Size>10485760</Size>
    <StorageClass>STANDARD</StorageClass>
  </Contents>
  <Contents>
    <Key>caches/cargo-e5f6g7h8.tar.gz</Key>
    <LastModified>2026-09-13T02:00:00.000Z</LastModified>
    <ETag>"abc"</ETag>
    <Size>20971520</Size>
    <StorageClass>STANDARD</StorageClass>
  </Contents>
</ListBucketResult>"#;

        let objs = parse_list_bucket_xml(xml);
        assert_eq!(objs.len(), 2);
        assert_eq!(objs[0].key, "caches/uv-a1b2c3d4.tar.gz");
        assert_eq!(objs[0].size_bytes, 10485760);
        assert_eq!(objs[0].last_modified, "2026-09-13T01:23:45.000Z");

        assert_eq!(objs[1].key, "caches/cargo-e5f6g7h8.tar.gz");
        assert_eq!(objs[1].size_bytes, 20971520);
    }

    #[test]
    fn presigned_url_contains_required_s3_v4_parameters() {
        let creds = Credentials {
            ak: "TESTAK1234567890".into(),
            sk: "TESTSK1234567890SECRETKEY".into(),
            security_token: None,
        };

        let url = generate_presigned_url(
            &creds,
            "ap-southeast-3",
            "qecs-cache-demo",
            "caches/uv-123.tar.gz",
            "GET",
            900,
        );

        assert!(url.starts_with(
            "https://qecs-cache-demo.obs.ap-southeast-3.myhuaweicloud.com/caches/uv-123.tar.gz?"
        ));
        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(url.contains("X-Amz-Credential=TESTAK1234567890%2F"));
        assert!(url.contains("X-Amz-Expires=900"));
        assert!(url.contains("X-Amz-SignedHeaders=host"));
        assert!(url.contains("X-Amz-Signature="));
    }

    #[test]
    fn presigned_url_includes_security_token_when_present() {
        let creds = Credentials {
            ak: "TESTAK1234567890".into(),
            sk: "TESTSK1234567890SECRETKEY".into(),
            security_token: Some("TESTTOKEN123".into()),
        };

        let url = generate_presigned_url(
            &creds,
            "ap-southeast-3",
            "qecs-cache-demo",
            "caches/uv-123.tar.gz",
            "GET",
            900,
        );

        assert!(url.contains("X-Amz-Security-Token=TESTTOKEN123"));
    }
}
