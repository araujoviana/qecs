//! `~/.config/qecs/config.toml`: defaults merged with user overrides.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub region: String,
    pub max_lifetime: String,
    pub idle_timeout: String,
    pub name_template: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credentials: Option<InlineCreds>,
    pub presets: PresetTable,
    pub env_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineCreds {
    pub ak: String,
    pub sk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetTable {
    pub normal: PresetCfg,
    pub ram: PresetCfg,
    pub compute: PresetCfg,
    pub gpu: PresetCfg,
    pub beefy: PresetCfg,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PresetCfg {
    pub flavor: String,
    pub disk_gb: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            region: "ap-southeast-3".into(),
            max_lifetime: "2h".into(),
            idle_timeout: "20m".into(),
            name_template: "qecs-{preset}-{shortid}".into(),
            credentials: None,
            presets: PresetTable::default(),
            env_files: default_env_files(),
        }
    }
}

impl Default for PresetTable {
    fn default() -> Self {
        PresetTable {
            normal: PresetCfg {
                flavor: "s7n.2xlarge.2".into(),
                disk_gb: 100,
            },
            ram: PresetCfg {
                flavor: "m7.4xlarge.8".into(),
                disk_gb: 100,
            },
            compute: PresetCfg {
                flavor: "c7.8xlarge.2".into(),
                disk_gb: 100,
            },
            gpu: PresetCfg {
                flavor: "pi2.4xlarge.4".into(),
                disk_gb: 200,
            },
            beefy: PresetCfg {
                flavor: "p2s.8xlarge.8".into(),
                disk_gb: 300,
            },
        }
    }
}

fn default_env_files() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".config/qecs/.env"));
        v.push(home.join("Projetos/python-projs/mcp-hwc/.env"));
    }
    v
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qecs/config.toml")
}

pub fn load_config(explicit: Option<&Path>) -> anyhow::Result<Config> {
    let path = explicit.map(Path::to_path_buf).unwrap_or_else(config_path);
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let defaults = toml::Value::try_from(Config::default())?;
    let user: toml::Value =
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;
    let merged = merge(defaults, user);
    Ok(merged.try_into()?)
}

fn merge(base: toml::Value, over: toml::Value) -> toml::Value {
    match (base, over) {
        (toml::Value::Table(mut b), toml::Value::Table(o)) => {
            for (k, v) in o {
                let nb = b.remove(&k).map(|bv| merge(bv, v.clone())).unwrap_or(v);
                b.insert(k, nb);
            }
            toml::Value::Table(b)
        }
        (_, o) => o,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn defaults_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_config(Some(&dir.path().join("nope.toml"))).unwrap();
        assert_eq!(cfg.region, "ap-southeast-3");
        assert_eq!(cfg.max_lifetime, "2h");
        assert_eq!(cfg.presets.gpu.flavor, "pi2.4xlarge.4");
        assert_eq!(cfg.presets.gpu.disk_gb, 200);
    }

    #[test]
    fn file_overrides_merge_over_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(
            f,
            "region = \"sa-brazil-1\"\n[presets.gpu]\nflavor = \"pi2.2xlarge.4\""
        )
        .unwrap();
        let cfg = load_config(Some(&p)).unwrap();
        assert_eq!(cfg.region, "sa-brazil-1");
        assert_eq!(cfg.presets.gpu.flavor, "pi2.2xlarge.4");
        assert_eq!(cfg.presets.gpu.disk_gb, 200);
        assert_eq!(cfg.idle_timeout, "20m");
    }

    #[test]
    fn malformed_toml_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.toml");
        std::fs::write(&p, "region = ").unwrap();
        assert!(load_config(Some(&p)).is_err());
    }
}
