//! Generic async polling with capped backoff, for resource-status waits.
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

use crate::telemetry::{PollEvent, Telemetry};

pub enum Poll<T> {
    Ready(T),
    Pending,
}

#[derive(Debug, Clone)]
pub struct PollConfig {
    pub interval: Duration,
    pub max_interval: Duration,
    pub timeout: Duration,
}

impl Default for PollConfig {
    fn default() -> Self {
        PollConfig {
            interval: Duration::from_secs(2),
            max_interval: Duration::from_secs(5),
            timeout: Duration::from_secs(300),
        }
    }
}

pub async fn poll_until<T, F, Fut>(
    cfg: &PollConfig,
    what: &str,
    mut probe: F,
    tel: Option<&Telemetry>,
) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<Poll<T>>>,
{
    let start = Instant::now();
    let mut wait = cfg.interval;
    let mut iterations = 0u32;
    loop {
        iterations = iterations.saturating_add(1);
        let res = probe().await;
        match res {
            Ok(Poll::Ready(v)) => {
                if let Some(t) = tel {
                    t.record_poll(PollEvent {
                        label: what.into(),
                        iterations,
                        total_ms: start.elapsed().as_millis() as u64,
                        final_interval_ms: wait.as_millis() as u64,
                        outcome: "ready",
                    });
                }
                return Ok(v);
            }
            Ok(Poll::Pending) => {}
            Err(e) => {
                if let Some(t) = tel {
                    t.record_poll(PollEvent {
                        label: what.into(),
                        iterations,
                        total_ms: start.elapsed().as_millis() as u64,
                        final_interval_ms: wait.as_millis() as u64,
                        outcome: "error",
                    });
                }
                return Err(e);
            }
        }
        if start.elapsed() >= cfg.timeout {
            if let Some(t) = tel {
                t.record_poll(PollEvent {
                    label: what.into(),
                    iterations,
                    total_ms: start.elapsed().as_millis() as u64,
                    final_interval_ms: wait.as_millis() as u64,
                    outcome: "timeout",
                });
            }
            anyhow::bail!("timed out after {:?} waiting for {what}", cfg.timeout);
        }
        tokio::time::sleep(wait).await;
        wait = std::cmp::min(cfg.max_interval, wait.mul_f32(1.5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn returns_when_probe_is_ready() {
        let n = AtomicU32::new(0);
        let cfg = PollConfig {
            interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(2),
            timeout: Duration::from_secs(1),
        };
        let got: u32 = poll_until(
            &cfg,
            "thing",
            || async {
                let c = n.fetch_add(1, Ordering::SeqCst);
                Ok(if c >= 2 {
                    Poll::Ready(c)
                } else {
                    Poll::Pending
                })
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(got, 2);
    }

    #[tokio::test]
    async fn times_out() {
        let cfg = PollConfig {
            interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(10),
        };
        let e = poll_until::<(), _, _>(&cfg, "never", || async { Ok(Poll::Pending) }, None)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("never"));
    }

    #[tokio::test]
    async fn propagates_probe_error() {
        let cfg = PollConfig::default();
        let e = poll_until::<(), _, _>(&cfg, "x", || async { anyhow::bail!("boom") }, None)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn poll_emits_timeout_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let tel = {
            unsafe {
                std::env::set_var("HOME", tmp.path());
                std::env::remove_var("XDG_STATE_HOME");
            }
            Telemetry::init(true, "test").expect("init telemetry")
        };

        let cfg = PollConfig {
            interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(10),
        };
        let _ = poll_until::<(), _, _>(
            &cfg,
            "timeout-probe",
            || async { Ok(Poll::Pending) },
            Some(&tel),
        )
        .await
        .unwrap_err();

        tel.finish(1);

        let traces_dir = tmp.path().join(".local/state/qecs/traces");
        let file = std::fs::read_dir(&traces_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .expect("one trace file");
        let body = std::fs::read_to_string(file).unwrap();
        let poll_line: serde_json::Value = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .find(|v: &serde_json::Value| v["kind"] == "poll")
            .expect("poll event present");

        assert_eq!(poll_line["label"], "timeout-probe");
        assert_eq!(poll_line["outcome"], "timeout");
        assert!(poll_line["iterations"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn poll_emits_error_outcome() {
        let tmp = tempfile::tempdir().unwrap();
        let tel = {
            unsafe {
                std::env::set_var("HOME", tmp.path());
                std::env::remove_var("XDG_STATE_HOME");
            }
            Telemetry::init(true, "test").expect("init telemetry")
        };

        let cfg = PollConfig::default();
        let _ = poll_until::<(), _, _>(
            &cfg,
            "err-probe",
            || async { anyhow::bail!("crash") },
            Some(&tel),
        )
        .await
        .unwrap_err();

        tel.finish(1);

        let traces_dir = tmp.path().join(".local/state/qecs/traces");
        let file = std::fs::read_dir(&traces_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .expect("one trace file");
        let body = std::fs::read_to_string(file).unwrap();
        let poll_line: serde_json::Value = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .find(|v: &serde_json::Value| v["kind"] == "poll")
            .expect("poll event present");

        assert_eq!(poll_line["label"], "err-probe");
        assert_eq!(poll_line["outcome"], "error");
        assert_eq!(poll_line["iterations"], 1);
    }

    #[tokio::test]
    async fn poll_emits_event_with_iteration_count() {
        let tmp = tempfile::tempdir().unwrap();
        let tel = {
            unsafe {
                std::env::set_var("HOME", tmp.path());
                std::env::remove_var("XDG_STATE_HOME");
            }
            crate::telemetry::Telemetry::init(true, "test").expect("init telemetry")
        };

        let n = AtomicU32::new(0);
        let cfg = PollConfig {
            interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(2),
            timeout: Duration::from_secs(1),
        };
        let got: u32 = poll_until(
            &cfg,
            "wait-active x",
            || async {
                let c = n.fetch_add(1, Ordering::SeqCst);
                Ok(if c >= 2 {
                    Poll::Ready(c)
                } else {
                    Poll::Pending
                })
            },
            Some(&tel),
        )
        .await
        .unwrap();
        assert_eq!(got, 2);

        tel.finish(0);

        let traces_dir = tmp.path().join(".local/state/qecs/traces");
        let file = std::fs::read_dir(&traces_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .expect("one trace file");
        let body = std::fs::read_to_string(file).unwrap();
        let poll_line: serde_json::Value = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .find(|v: &serde_json::Value| v["kind"] == "poll")
            .expect("poll event present");

        assert_eq!(poll_line["label"], "wait-active x");
        assert_eq!(poll_line["iterations"], 3);
        assert_eq!(poll_line["outcome"], "ready");
    }
}
