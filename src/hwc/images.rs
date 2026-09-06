//! Image Management Service - resolve the current HWC gold image at runtime.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use serde::Deserialize;
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

#[derive(Debug, Clone)]
pub struct Image {
    pub id: String,
    pub name: String,
    pub os_version: String,
    pub min_disk: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawImage {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(rename = "__os_version")]
    pub(crate) os_version: String,
    pub(crate) min_disk: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImagesResp {
    pub(crate) images: Vec<RawImage>,
}

pub(crate) fn images_url(region: &str, platform: Platform) -> String {
    let host = endpoint_host(Service::Ims, region);
    format!(
        "https://{host}/v2/cloudimages?__imagetype=gold&__platform={platform}&__os_bit=64&__support_kvm=true&status=active&limit=50"
    )
}

pub(crate) fn pick_newest(images: Vec<RawImage>, _platform: Platform) -> Option<Image> {
    let mut filtered: Vec<_> = images
        .into_iter()
        .filter(|img| !img.name.contains("Graphic") && !img.name.contains("BareMetal"))
        .collect();

    // Sort by __os_version descending (string sort)
    filtered.sort_by(|a, b| b.os_version.cmp(&a.os_version));

    filtered.into_iter().next().map(|raw| Image {
        id: raw.id,
        name: raw.name,
        os_version: raw.os_version,
        min_disk: raw.min_disk,
    })
}

pub async fn resolve_image(
    c: &SignedClient,
    region: &str,
    platform: Platform,
) -> anyhow::Result<Image> {
    let url = images_url(region, platform);
    let resp: ImagesResp = c
        .send_json(reqwest::Method::GET, &url, None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    pick_newest(resp.images, platform)
        .ok_or_else(|| anyhow::anyhow!("no suitable gold image found for {platform}"))
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
    fn images_url_ubuntu_query_string() {
        let url = images_url("ap-southeast-3", Platform::Ubuntu);
        assert_eq!(
            url,
            "https://ims.ap-southeast-3.myhuaweicloud.com/v2/cloudimages?__imagetype=gold&__platform=Ubuntu&__os_bit=64&__support_kvm=true&status=active&limit=50"
        );
    }
}
