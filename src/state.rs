//! `~/.local/state/qecs/vms.json` - a cache of provisioned VMs. Cloud is truth.
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmRecord {
    pub id: String,
    pub name: String,
    pub preset: String,
    pub flavor: String,
    pub region: String,
    pub az: String,
    pub eip: Option<String>,
    pub private_ip: Option<String>,
    pub created_at: String,
    pub ttl_secs: u64,
    pub connect_port: Option<u16>,
    pub job: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StateStore {
    pub path: PathBuf,
}

pub struct Lock {
    file: File,
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl StateStore {
    pub fn at(path: PathBuf) -> Self {
        StateStore { path }
    }

    pub fn open() -> anyhow::Result<Self> {
        Ok(StateStore { path: state_path() })
    }

    pub fn acquire_lock_with_timeout(&self, timeout: Duration) -> anyhow::Result<Lock> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = self.path.with_file_name("vms.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        let fd = file.as_raw_fd();
        let start = Instant::now();
        loop {
            let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(Lock { file });
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                || err.raw_os_error() == Some(libc::EAGAIN)
            {
                if start.elapsed() > timeout {
                    anyhow::bail!("state file is locked: {}", lock_path.display());
                }
                std::thread::sleep(Duration::from_millis(25));
            } else {
                return Err(err.into());
            }
        }
    }

    pub fn acquire_lock(&self) -> anyhow::Result<Lock> {
        self.acquire_lock_with_timeout(Duration::from_secs(5))
    }

    pub fn list(&self) -> anyhow::Result<Vec<VmRecord>> {
        match std::fs::read_to_string(&self.path) {
            Ok(s) if s.trim().is_empty() => Ok(vec![]),
            Ok(s) => serde_json::from_str(&s)
                .map_err(|e| anyhow::anyhow!("parsing {}: {e}", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get(&self, name: &str) -> anyhow::Result<Option<VmRecord>> {
        Ok(self.list()?.into_iter().find(|r| r.name == name))
    }

    fn write_all(&self, recs: &[VmRecord]) -> anyhow::Result<()> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp =
            self.path
                .with_file_name(format!("vms.json.tmp.{}.{}", std::process::id(), nanos));
        std::fs::write(&tmp, serde_json::to_vec_pretty(recs)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    pub fn upsert(&self, rec: VmRecord) -> anyhow::Result<()> {
        let _lock = self.acquire_lock()?;
        let mut recs = self.list()?;
        match recs.iter_mut().find(|r| r.name == rec.name) {
            Some(slot) => *slot = rec,
            None => recs.push(rec),
        }
        self.write_all(&recs)
    }

    pub fn remove(&self, name: &str) -> anyhow::Result<bool> {
        let _lock = self.acquire_lock()?;
        let mut recs = self.list()?;
        let before = recs.len();
        recs.retain(|r| r.name != name);
        let removed = recs.len() != before;
        if removed {
            self.write_all(&recs)?;
        }
        Ok(removed)
    }
}

pub fn state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qecs/vms.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str) -> VmRecord {
        VmRecord {
            id: format!("id-{name}"),
            name: name.into(),
            preset: "gpu".into(),
            flavor: "pi2.4xlarge.4".into(),
            region: "ap-southeast-3".into(),
            az: "ap-southeast-3a".into(),
            eip: None,
            private_ip: None,
            created_at: "2026-09-05T12:00:00Z".into(),
            ttl_secs: 7200,
            connect_port: None,
            job: None,
            tags: vec![],
        }
    }

    #[test]
    fn upsert_then_list_then_remove() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateStore::at(dir.path().join("vms.json"));
        assert!(s.list().unwrap().is_empty());
        s.upsert(rec("a")).unwrap();
        s.upsert(rec("b")).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
        s.upsert(VmRecord {
            flavor: "changed".into(),
            ..rec("a")
        })
        .unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
        assert_eq!(s.get("a").unwrap().unwrap().flavor, "changed");
        assert!(s.remove("a").unwrap());
        assert!(!s.remove("a").unwrap());
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn corrupt_file_is_reported_not_panicked() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("vms.json");
        std::fs::write(&p, "{not json").unwrap();
        assert!(StateStore::at(p).list().is_err());
    }

    #[test]
    fn lock_is_released_after_write() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateStore::at(dir.path().join("vms.json"));
        s.upsert(rec("a")).unwrap();
        s.upsert(rec("b")).unwrap();
        // Verifying lock can be acquired immediately because write released it
        let lock = s.acquire_lock_with_timeout(Duration::from_millis(100));
        assert!(lock.is_ok());
    }

    #[test]
    fn automatic_unlock_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let s = StateStore::at(dir.path().join("vms.json"));
        {
            let lock1 = s
                .acquire_lock_with_timeout(Duration::from_millis(100))
                .unwrap();
            // A second attempt with zero timeout fails
            let lock2 = s.acquire_lock_with_timeout(Duration::from_millis(50));
            assert!(lock2.is_err());
            drop(lock1);
        }
        // After drop, lock is available
        let lock3 = s.acquire_lock_with_timeout(Duration::from_millis(100));
        assert!(lock3.is_ok());
    }

    #[test]
    fn concurrent_upserts_succeed() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(StateStore::at(dir.path().join("vms.json")));
        let mut handles = Vec::new();

        for i in 0..10 {
            let store = Arc::clone(&s);
            handles.push(std::thread::spawn(move || {
                store.upsert(rec(&format!("node-{i}"))).unwrap();
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(s.list().unwrap().len(), 10);
    }
}
