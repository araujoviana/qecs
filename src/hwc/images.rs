//! Image Management Service - resolve gold images, manage private baked images,
//! and orchestrate accelerated image builds.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Ubuntu,
    Debian,
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Ubuntu => write!(f, "Ubuntu"),
            Platform::Debian => write!(f, "Debian"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Image {
    pub id: String,
    pub name: String,
    pub os_version: String,
    pub min_disk: u32,
    pub status: String,
    pub image_type: String,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawImage {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(rename = "__os_version", default)]
    pub(crate) os_version: String,
    #[serde(default)]
    pub(crate) min_disk: u32,
    #[serde(default)]
    pub(crate) status: String,
    #[serde(rename = "__imagetype", default)]
    pub(crate) image_type: String,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
}

impl From<RawImage> for Image {
    fn from(raw: RawImage) -> Self {
        Image {
            id: raw.id,
            name: raw.name,
            os_version: raw.os_version,
            min_disk: raw.min_disk,
            status: raw.status,
            image_type: raw.image_type,
            created_at: raw.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImagesResp {
    pub(crate) images: Vec<RawImage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateImageResp {
    pub(crate) job_id: String,
}

pub(crate) fn gold_images_url(
    region: &str,
    platform: Platform,
    cond_image: Option<&str>,
) -> String {
    let host = endpoint_host(Service::Ims, region);
    let cond = cond_image.unwrap_or("__support_kvm=true");
    format!(
        "https://{host}/v2/cloudimages?__imagetype=gold&__platform={platform}&__os_bit=64&{cond}&status=active&limit=50"
    )
}

pub(crate) fn private_images_url(region: &str) -> String {
    let host = endpoint_host(Service::Ims, region);
    format!("https://{host}/v2/cloudimages?__imagetype=private&status=active&limit=100")
}

pub(crate) fn image_action_url(region: &str) -> String {
    let host = endpoint_host(Service::Ims, region);
    format!("https://{host}/v2/cloudimages/action")
}

pub(crate) fn single_image_url(region: &str, image_id: &str) -> String {
    let host = endpoint_host(Service::Ims, region);
    format!("https://{host}/v2/cloudimages/{image_id}")
}

/// Backward compatibility alias.
pub(crate) fn images_url(region: &str, platform: Platform) -> String {
    gold_images_url(region, platform, None)
}

pub(crate) fn pick_newest(images: Vec<RawImage>, _platform: Platform) -> Option<Image> {
    let mut filtered: Vec<_> = images
        .into_iter()
        .filter(|img| !img.name.contains("BareMetal"))
        .collect();

    // Prefer non-Graphic images if available (e.g. standard headless servers)
    if filtered.iter().any(|img| !img.name.contains("Graphic")) {
        filtered.retain(|img| !img.name.contains("Graphic"));
    }

    // Sort by __os_version descending (string sort)
    filtered.sort_by(|a, b| b.os_version.cmp(&a.os_version));

    filtered.into_iter().next().map(Into::into)
}

/// Resolve the baseline gold image from Huawei Cloud.
pub async fn resolve_image(
    c: &SignedClient,
    region: &str,
    platform: Platform,
    cond_image: Option<&str>,
) -> anyhow::Result<Image> {
    let url = gold_images_url(region, platform, cond_image);
    let resp: ImagesResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    pick_newest(resp.images, platform)
        .ok_or_else(|| anyhow::anyhow!("no suitable gold image found for {platform}"))
}

/// List active private images owned by the user account in `region`.
pub async fn list_private_images(c: &SignedClient, region: &str) -> anyhow::Result<Vec<Image>> {
    let url = private_images_url(region);
    let resp: ImagesResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.images.into_iter().map(Into::into).collect())
}

/// Request IMS to create a private system image from an existing ECS instance.
/// Returns the asynchronous `job_id`.
pub async fn create_image_from_server(
    c: &SignedClient,
    region: &str,
    server_id: &str,
    name: &str,
    description: Option<&str>,
) -> anyhow::Result<String> {
    let url = image_action_url(region);
    let desc = description.unwrap_or("Created by qecs");
    let body = serde_json::json!({
        "name": name,
        "instance_id": server_id,
        "description": desc,
        "tags": ["managed-by=qecs"]
    });

    let resp: CreateImageResp = c
        .send_json(reqwest::Method::POST, &url, Some(&body))
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(resp.job_id)
}

/// Delete a private image by ID.
pub async fn delete_image(c: &SignedClient, region: &str, image_id: &str) -> anyhow::Result<()> {
    let url = single_image_url(region, image_id);
    c.send_json::<serde_json::Value>(reqwest::Method::DELETE, &url, None)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!(e))
}

pub(crate) fn pick_baked_image(images: Vec<Image>) -> Option<Image> {
    let mut baked: Vec<_> = images
        .into_iter()
        .filter(|img| img.status == "active" && img.name.contains("qecs-gpu"))
        .collect();
    baked.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.name.cmp(&a.name))
    });
    baked.into_iter().next()
}

/// Select a baked private image when appropriate, or fall back to the newest gold image.
pub async fn resolve_baked_or_gold_image(
    c: &SignedClient,
    region: &str,
    needs_gpu: bool,
    cond_image: Option<&str>,
    platform: Platform,
    prefer_baked: bool,
) -> anyhow::Result<Image> {
    if needs_gpu
        && prefer_baked
        && let Ok(private_images) = list_private_images(c, region).await
        && let Some(img) = pick_baked_image(private_images)
    {
        return Ok(img);
    }

    resolve_image(c, region, platform, cond_image).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_newest_plain_ubuntu_skipping_graphic_variant() {
        let raw: Vec<RawImage> = serde_json::from_value(serde_json::json!([
            {"id":"a","name":"Ubuntu 22.04 server 64bit","__os_version":"Ubuntu 22.04 server 64bit","min_disk":10},
            {"id":"b","name":"Ubuntu 24.04 server 64bit","__os_version":"Ubuntu 24.04 server 64bit","min_disk":10},
            {"id":"c","name":"Ubuntu 24.04 server 64bit with Graphic driver","__os_version":"Ubuntu 24.04 server 64bit","min_disk":40}
        ])).unwrap();
        let img = pick_newest(raw, Platform::Ubuntu).unwrap();
        assert_eq!(img.id, "b");
        assert_eq!(img.min_disk, 10);
    }

    #[test]
    fn pick_baked_image_selects_newest_active_gpu_image() {
        let images = vec![
            Image {
                id: "img-old".into(),
                name: "qecs-gpu-20260901".into(),
                os_version: "Ubuntu 22.04".into(),
                min_disk: 40,
                status: "active".into(),
                image_type: "private".into(),
                created_at: Some("2026-09-01T10:00:00Z".into()),
            },
            Image {
                id: "img-inactive".into(),
                name: "qecs-gpu-20260906-building".into(),
                os_version: "Ubuntu 22.04".into(),
                min_disk: 40,
                status: "saving".into(),
                image_type: "private".into(),
                created_at: Some("2026-09-06T12:00:00Z".into()),
            },
            Image {
                id: "img-newest".into(),
                name: "qecs-gpu-20260905".into(),
                os_version: "Ubuntu 22.04".into(),
                min_disk: 40,
                status: "active".into(),
                image_type: "private".into(),
                created_at: Some("2026-09-05T10:00:00Z".into()),
            },
            Image {
                id: "img-other".into(),
                name: "my-custom-app".into(),
                os_version: "Ubuntu 22.04".into(),
                min_disk: 40,
                status: "active".into(),
                image_type: "private".into(),
                created_at: Some("2026-09-06T10:00:00Z".into()),
            },
        ];

        let chosen = pick_baked_image(images).expect("should pick baked image");
        assert_eq!(chosen.id, "img-newest");
    }

    #[test]
    fn images_url_ubuntu_query_string() {
        let url = gold_images_url("ap-southeast-3", Platform::Ubuntu, None);
        assert_eq!(
            url,
            "https://ims.ap-southeast-3.myhuaweicloud.com/v2/cloudimages?__imagetype=gold&__platform=Ubuntu&__os_bit=64&__support_kvm=true&status=active&limit=50"
        );

        let gpu_url = gold_images_url(
            "ap-southeast-3",
            Platform::Ubuntu,
            Some("__support_gpu_t4=true"),
        );
        assert_eq!(
            gpu_url,
            "https://ims.ap-southeast-3.myhuaweicloud.com/v2/cloudimages?__imagetype=gold&__platform=Ubuntu&__os_bit=64&__support_gpu_t4=true&status=active&limit=50"
        );
    }

    #[test]
    fn private_images_url_shape() {
        let url = private_images_url("ap-southeast-3");
        assert_eq!(
            url,
            "https://ims.ap-southeast-3.myhuaweicloud.com/v2/cloudimages?__imagetype=private&status=active&limit=100"
        );
    }

    #[test]
    fn image_action_url_shape() {
        let url = image_action_url("ap-southeast-3");
        assert_eq!(
            url,
            "https://ims.ap-southeast-3.myhuaweicloud.com/v2/cloudimages/action"
        );
    }
}
