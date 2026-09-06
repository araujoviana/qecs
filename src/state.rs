//! `~/.local/state/qecs/vms.json` - a cache of provisioned VMs. Cloud is truth.
use serde::{Deserialize, Serialize};
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

struct Lock {
    path: PathBuf,
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl StateStore {
    pub fn at(path: PathBuf) -> Self {
        StateStore { path }
    }

    pub fn open() -> anyhow::Result<Self> {
        Ok(StateStore { path: state_path() })
    }

    fn acquire_lock(&self) -> anyhow::Result<Lock> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = self.path.with_extension("json.lock");
        let start = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(_) => return Ok(Lock { path: lock_path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if start.elapsed() > Duration::from_secs(5) {
                        anyhow::bail!("state file is locked: {}", lock_path.display());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e.into()),
            }
        }
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
        let tmp = self.path.with_extension("json.tmp");
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
        assert!(!dir.path().join("vms.json.lock").exists());
    }
}
