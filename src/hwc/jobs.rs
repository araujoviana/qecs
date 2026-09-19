//! Poll HWC async jobs (Pattern A: create/delete ECS return a `job_id`) to completion.
use crate::hwc::client::SignedClient;
use crate::hwc::endpoints::{Service, endpoint_url};
use crate::hwc::wait::{Poll, PollConfig, poll_until};
use crate::telemetry::Telemetry;
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
    /// IMS `createImageByServer` reports the new image id here, not in `sub_jobs`.
    #[serde(default)]
    pub(crate) image_id: Option<String>,
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

/// Build the async-jobs endpoint URL for `service`. The path is identical across
/// services (`/v1/{project_id}/jobs/{job_id}`); only the host differs, and a job
/// must be queried on the same service that issued it (ECS create/delete on the
/// ECS host, IMS image bake on the IMS host).
pub(crate) fn job_url(service: Service, region: &str, project_id: &str, job_id: &str) -> String {
    format!(
        "{}/v1/{project_id}/jobs/{job_id}",
        endpoint_url(service, region)
    )
}

/// Pure: map a parsed job body to Ready/Pending. Ready carries a `Result` so a
/// FAIL job surfaces through `poll_until`'s probe error path.
pub(crate) fn job_outcome(resp: JobResp) -> Poll<anyhow::Result<JobResult>> {
    match resp.status {
        JobStatus::Success => {
            let mut server_ids = Vec::new();
            let mut image_ids = Vec::new();
            if let Some(iid) = resp.entities.image_id {
                image_ids.push(iid);
            }
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
        JobStatus::PendingPayment => Poll::Ready(Err(anyhow::anyhow!(
            "job blocked on payment or pending account order (PENDING_PAYMENT)"
        ))),
        JobStatus::Other => {
            let reason = resp
                .fail_reason
                .unwrap_or_else(|| "unrecognized job state".to_string());
            Poll::Ready(Err(anyhow::anyhow!("unexpected job state: {reason}")))
        }
        JobStatus::Init | JobStatus::Running => Poll::Pending,
    }
}

/// Poll `GET /v1/{project_id}/jobs/{job_id}` until the job reaches SUCCESS or FAIL.
pub async fn poll_job(
    c: &SignedClient,
    service: Service,
    region: &str,
    project_id: &str,
    job_id: &str,
    cfg: &PollConfig,
    tel: Option<&Telemetry>,
) -> anyhow::Result<JobResult> {
    let tel = tel.or(c.telemetry.as_ref());
    let url = job_url(service, region, project_id, job_id);
    poll_until(
        cfg,
        &format!("job {job_id}"),
        || async {
            let resp: JobResp = c
                .send_json(reqwest::Method::GET, &url, None)
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
            match job_outcome(resp) {
                Poll::Ready(Ok(r)) => Ok(Poll::Ready(r)),
                Poll::Ready(Err(e)) => Err(e),
                Poll::Pending => Ok(Poll::Pending),
            }
        },
        tel,
    )
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
            job_url(Service::Ecs, "ap-southeast-3", "proj-42", "job-abc"),
            "https://ecs.ap-southeast-3.myhuaweicloud.com/v1/proj-42/jobs/job-abc"
        );
    }

    #[test]
    fn ims_job_url_targets_the_ims_host() {
        assert_eq!(
            job_url(Service::Ims, "ap-southeast-3", "proj-42", "job-img"),
            "https://ims.ap-southeast-3.myhuaweicloud.com/v1/proj-42/jobs/job-img"
        );
    }

    #[test]
    fn ims_success_job_yields_image_id_from_top_level_entities() {
        // IMS createImageByServer puts image_id directly under entities, not in sub_jobs.
        let j = r#"{"status":"SUCCESS","job_id":"j","job_type":"createImageByServer",
            "entities":{"image_id":"img-9f3a"}}"#;
        let Poll::Ready(Ok(r)) = job_outcome(serde_json::from_str(j).unwrap()) else {
            panic!("expected ready-ok")
        };
        assert_eq!(r.image_ids, ["img-9f3a"]);
        assert!(r.server_ids.is_empty());
    }

    #[test]
    fn pending_payment_fails_fast() {
        let j = r#"{"status":"PENDING_PAYMENT","job_id":"j"}"#;
        let Poll::Ready(Err(e)) = job_outcome(serde_json::from_str(j).unwrap()) else {
            panic!("expected ready-err for pending payment")
        };
        assert!(e.to_string().contains("PENDING_PAYMENT"));
    }

    #[test]
    fn other_unknown_status_fails_fast() {
        let j = r#"{"status":"UNKNOWN_CUSTOM_STATUS","job_id":"j","fail_reason":"abnormal"}"#;
        let Poll::Ready(Err(e)) = job_outcome(serde_json::from_str(j).unwrap()) else {
            panic!("expected ready-err for unknown status")
        };
        assert!(e.to_string().contains("abnormal"));
    }
}
