//! Generic async polling with capped backoff, for resource-status waits.
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

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

pub async fn poll_until<T, F, Fut>(cfg: &PollConfig, what: &str, mut probe: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<Poll<T>>>,
{
    let start = Instant::now();
    let mut wait = cfg.interval;
    loop {
        match probe().await? {
            Poll::Ready(v) => return Ok(v),
            Poll::Pending => {}
        }
        if start.elapsed() >= cfg.timeout {
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
        let got: u32 = poll_until(&cfg, "thing", || async {
            let c = n.fetch_add(1, Ordering::SeqCst);
            Ok(if c >= 2 {
                Poll::Ready(c)
            } else {
                Poll::Pending
            })
        })
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
        let e = poll_until::<(), _, _>(&cfg, "never", || async { Ok(Poll::Pending) })
            .await
            .unwrap_err();
        assert!(e.to_string().contains("never"));
    }

    #[tokio::test]
    async fn propagates_probe_error() {
        let cfg = PollConfig::default();
        let e = poll_until::<(), _, _>(&cfg, "x", || async { anyhow::bail!("boom") })
            .await
            .unwrap_err();
        assert!(e.to_string().contains("boom"));
    }
}
