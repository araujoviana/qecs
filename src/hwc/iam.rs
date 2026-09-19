//! IAM: resolve the region-scoped project_id (and domain_id) from AK/SK. Notes §1.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_url};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct ProjectRef {
    pub id: String,
    pub domain_id: String,
    pub region: String,
}

#[derive(Deserialize)]
pub struct ProjectsResp {
    pub projects: Vec<Project>,
}

#[derive(Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub domain_id: String,
}

pub(crate) fn projects_url(region: &str) -> String {
    format!(
        "{}/v3/projects?name={region}",
        endpoint_url(Service::Iam, region)
    )
}

pub(crate) fn pick_project(resp: ProjectsResp, region: &str) -> anyhow::Result<ProjectRef> {
    let p = resp
        .projects
        .into_iter()
        .find(|p| p.name == region)
        .ok_or_else(|| {
            anyhow::anyhow!("no IAM project for region {region} (check AK/SK region access)")
        })?;
    Ok(ProjectRef {
        id: p.id,
        domain_id: p.domain_id,
        region: region.to_string(),
    })
}

pub async fn discover_project(client: &SignedClient, region: &str) -> anyhow::Result<ProjectRef> {
    let resp: ProjectsResp = client
        .send_json(reqwest::Method::GET, &projects_url(region), None)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    pick_project(resp, region)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_is_iam_host_with_name_filter() {
        assert_eq!(
            projects_url("ap-southeast-3"),
            "https://iam.ap-southeast-3.myhuaweicloud.com/v3/projects?name=ap-southeast-3"
        );
    }

    #[test]
    fn pick_project_selects_the_matching_region() {
        let resp = serde_json::from_str::<ProjectsResp>(
            r#"{"projects":[{"id":"other","name":"sa-brazil-1","domain_id":"d1"},{"id":"PROJ","name":"ap-southeast-3","domain_id":"d1"}]}"#
        ).unwrap();
        let p = pick_project(resp, "ap-southeast-3").unwrap();
        assert_eq!(p.id, "PROJ");
        assert_eq!(p.domain_id, "d1");
    }

    #[test]
    fn pick_project_errors_when_no_region_matches() {
        let resp = serde_json::from_str::<ProjectsResp>(
            r#"{"projects":[{"id":"other","name":"sa-brazil-1","domain_id":"d1"}]}"#,
        )
        .unwrap();
        let result = pick_project(resp, "ap-southeast-3");
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("no IAM project"));
    }
}
