//! Poll HWC async jobs (Pattern A: create/delete ECS return a `job_id`) to completion.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_host};
use crate::hwc::wait::{Poll, PollConfig, poll_until};
use serde::Deserialize;

/// Result of a completed (SUCCESS) job.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobResult {
    pub server_ids: Vec<String>,
    pub image_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum JobStatus {
    Init,
    Running,
    Success,
    Fail,
    PendingPayment,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(crate) struct JobResp {
    pub(crate) status: JobStatus,
    #[serde(default)]
    pub(crate) fail_reason: Option<String>,
    #[serde(default)]
    pub(crate) entities: JobEntities,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct JobEntities {
    #[serde(default)]
    pub(crate) sub_jobs: Vec<SubJob>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubJob {
    #[serde(default)]
    pub(crate) fail_reason: Option<String>,
    #[serde(default)]
    pub(crate) entities: SubJobEntities,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SubJobEntities {
    #[serde(default)]
    pub(crate) server_id: Option<String>,
    #[serde(default)]
    pub(crate) image_id: Option<String>,
}

/// Build the ECS-host jobs endpoint URL (mirrors `iam::projects_url`).
pub(crate) fn job_url(region: &str, project_id: &str, job_id: &str) -> String {
    let host = endpoint_host(Service::Ecs, region);
    format!("https://{host}/v1/{project_id}/jobs/{job_id}")
}

/// Pure: map a parsed job body to Ready/Pending. Ready carries a `Result` so a
/// FAIL job surfaces through `poll_until`'s probe error path.
pub(crate) fn job_outcome(resp: JobResp) -> Poll<anyhow::Result<JobResult>> {
    match resp.status {
        JobStatus::Success => {
            let mut server_ids = Vec::new();
            let mut image_ids = Vec::new();
            for sj in resp.entities.sub_jobs {
                if let Some(sid) = sj.entities.server_id {
                    server_ids.push(sid);
                }
                if let Some(iid) = sj.entities.image_id {
                    image_ids.push(iid);
                }
            }
            Poll::Ready(Ok(JobResult {
                server_ids,
                image_ids,
            }))
        }
        JobStatus::Fail => {
            let mut msg = resp.fail_reason.unwrap_or_else(|| "job failed".to_string());
            if let Some(sub) = resp
                .entities
                .sub_jobs
                .first()
                .and_then(|sj| sj.fail_reason.as_deref())
                // Avoid repeating the sub-job reason if it's already in the top-level message
                && !msg.contains(sub)
            {
                msg = format!("{msg}: {sub}");
            }
            Poll::Ready(Err(anyhow::anyhow!(msg)))
        }
        JobStatus::Init | JobStatus::Running | JobStatus::PendingPayment | JobStatus::Other => {
            Poll::Pending
        }
    }
}

/// Poll `GET /v1/{project_id}/jobs/{job_id}` until the job reaches SUCCESS or FAIL.
pub async fn poll_job(
    c: &SignedClient,
    region: &str,
    project_id: &str,
    job_id: &str,
    cfg: &PollConfig,
) -> anyhow::Result<JobResult> {
    let url = job_url(region, project_id, job_id);
    poll_until(cfg, &format!("job {job_id}"), || async {
        let resp: JobResp = c
            .send_json(reqwest::Method::GET, &url, None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        match job_outcome(resp) {
            Poll::Ready(Ok(r)) => Ok(Poll::Ready(r)),
            Poll::Ready(Err(e)) => Err(e),
            Poll::Pending => Ok(Poll::Pending),
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_job_is_pending() {
        let j = r#"{"status":"RUNNING","job_id":"j","entities":{"sub_jobs":[]}}"#;
        assert!(matches!(
            job_outcome(serde_json::from_str(j).unwrap()),
            Poll::Pending
        ));
    }

    #[test]
    fn success_job_yields_server_id() {
        let j = r#"{"status":"SUCCESS","job_id":"j","entities":{"sub_jobs":[
            {"status":"SUCCESS","entities":{"server_id":"srv-123"}}]}}"#;
        let Poll::Ready(Ok(r)) = job_outcome(serde_json::from_str(j).unwrap()) else {
            panic!()
        };
        assert_eq!(r.server_ids, ["srv-123"]);
    }

    #[test]
    fn failed_job_surfaces_reason() {
        let j = r#"{"status":"FAIL","job_id":"j","fail_reason":"quota exceeded",
            "entities":{"sub_jobs":[{"status":"FAIL","fail_reason":"quota exceeded","entities":{}}]}}"#;
        let Poll::Ready(Err(e)) = job_outcome(serde_json::from_str(j).unwrap()) else {
            panic!()
        };
        assert!(e.to_string().contains("quota exceeded"));
    }

    #[test]
    fn missing_sub_jobs_field_is_pending_not_panic() {
        let j = r#"{"status":"INIT","job_id":"j"}"#;
        assert!(matches!(
            job_outcome(serde_json::from_str(j).unwrap()),
            Poll::Pending
        ));
    }

    #[test]
    fn job_url_is_ecs_host_with_v1_jobs_path() {
        assert_eq!(
            job_url("ap-southeast-3", "proj-42", "job-abc"),
            "https://ecs.ap-southeast-3.myhuaweicloud.com/v1/proj-42/jobs/job-abc"
        );
    }
}
