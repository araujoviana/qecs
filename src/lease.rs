//! Resource Lease: RAII guard and lifecycle management for ephemeral cloud VMs.

use crate::ctx::Ctx;
use crate::state::VmRecord;

pub struct VmLease {
    ctx: Ctx,
    record: VmRecord,
    disarmed: bool,
    destroyed: bool,
}

impl VmLease {
    pub fn new(ctx: Ctx, record: VmRecord) -> Self {
        Self {
            ctx,
            record,
            disarmed: false,
            destroyed: false,
        }
    }

    pub fn ctx(&self) -> &Ctx {
        &self.ctx
    }

    pub fn record(&self) -> &VmRecord {
        &self.record
    }

    pub fn record_mut(&mut self) -> &mut VmRecord {
        &mut self.record
    }

    pub fn disarm(&mut self) {
        self.disarmed = true;
    }

    pub fn is_disarmed(&self) -> bool {
        self.disarmed
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    pub async fn teardown(&mut self) -> anyhow::Result<()> {
        if self.disarmed || self.destroyed {
            return Ok(());
        }
        let res =
            crate::commands::run::destroy_vm(&self.ctx, &self.record.id, &self.record.name).await;
        if res.is_ok() {
            self.destroyed = true;
        }
        res
    }
}

impl Drop for VmLease {
    fn drop(&mut self) {
        if !self.disarmed && !self.destroyed {
            log::warn!(
                "VmLease for {} ({}) was dropped without teardown() or disarm(). Triggering fallback destruction.",
                self.record.name,
                self.record.id
            );
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let ctx = self.ctx.clone();
                let id = self.record.id.clone();
                let name = self.record.name.clone();
                handle.spawn(async move {
                    log::info!(
                        "Executing fallback background teardown for lease VM {name} ({id})..."
                    );
                    let _ = crate::commands::run::destroy_vm(&ctx, &id, &name).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::GlobalArgs;
    use crate::config::Config;
    use crate::creds::Credentials;

    fn dummy_ctx() -> Ctx {
        Ctx {
            config: Config::default(),
            creds: Credentials {
                ak: "test_ak".to_string(),
                sk: "test_sk".to_string(),
                security_token: None,
            },
            http: reqwest::Client::new(),
            global: GlobalArgs::default(),
            telemetry: None,
        }
    }

    fn dummy_record() -> VmRecord {
        VmRecord {
            id: "server-abc-123".to_string(),
            name: "test-vm-01".to_string(),
            preset: "cpu".to_string(),
            flavor: "s7n.small".to_string(),
            region: "sa-brazil-1".to_string(),
            az: "sa-brazil-1a".to_string(),
            eip: Some("190.0.0.1".to_string()),
            private_ip: Some("192.168.1.10".to_string()),
            created_at: "2026-09-17T00:00:00Z".to_string(),
            ttl_secs: 3600,
            connect_port: None,
            job: None,
            tags: vec![],
        }
    }

    #[test]
    fn test_lease_new_and_record_accessor() {
        let ctx = dummy_ctx();
        let record = dummy_record();
        let mut lease = VmLease::new(ctx, record);

        assert_eq!(lease.record().id, "server-abc-123");
        assert_eq!(lease.record().name, "test-vm-01");
        assert!(!lease.is_disarmed());
        assert!(!lease.is_destroyed());

        lease.record_mut().connect_port = Some(2222);
        assert_eq!(lease.record().connect_port, Some(2222));
    }

    #[test]
    fn test_lease_disarm() {
        let ctx = dummy_ctx();
        let record = dummy_record();
        let mut lease = VmLease::new(ctx, record);

        lease.disarm();
        assert!(lease.is_disarmed());
    }

    #[tokio::test]
    async fn test_lease_disarmed_teardown_is_noop() {
        let ctx = dummy_ctx();
        let record = dummy_record();
        let mut lease = VmLease::new(ctx, record);

        lease.disarm();
        let res1 = lease.teardown().await;
        assert!(res1.is_ok());
        let res2 = lease.teardown().await;
        assert!(res2.is_ok());
        assert!(!lease.is_destroyed());
    }

    #[tokio::test]
    async fn test_lease_already_destroyed_teardown_is_noop() {
        let ctx = dummy_ctx();
        let record = dummy_record();
        let mut lease = VmLease::new(ctx, record);
        lease.destroyed = true;

        let res1 = lease.teardown().await;
        assert!(res1.is_ok());
        let res2 = lease.teardown().await;
        assert!(res2.is_ok());
        assert!(lease.is_destroyed());
    }

    #[tokio::test]
    async fn test_lease_select_cancellation() {
        let ctx = dummy_ctx();
        let record = dummy_record();
        let mut lease = VmLease::new(ctx, record);

        async fn dummy_work(l: &mut VmLease) -> anyhow::Result<()> {
            l.record_mut().connect_port = Some(22);
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            Ok(())
        }

        let res = tokio::select! {
            r = dummy_work(&mut lease) => r,
            _ = async {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            } => {
                let _ = lease.teardown().await;
                Err(anyhow::anyhow!("cancelled"))
            }
        };

        assert!(res.is_err());
    }

    #[tokio::test]
    async fn test_lease_failed_teardown_does_not_set_destroyed() {
        let ctx = dummy_ctx();
        let record = dummy_record();
        let mut lease = VmLease::new(ctx, record);

        let res = lease.teardown().await;
        // With dummy context/credentials, destroy_vm fails
        assert!(res.is_err());
        assert!(!lease.is_destroyed());
    }

    #[tokio::test]
    async fn test_lease_drop_without_teardown_triggers_safely() {
        let ctx = dummy_ctx();
        let record = dummy_record();
        {
            let _lease = VmLease::new(ctx, record);
            // Dropped here without disarm or teardown
        }
        // Yield to allow background spawn to execute without crashing
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
