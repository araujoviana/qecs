//! Presets map a friendly name to a concrete flavor + disk. Config overrides all.
use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Normal,
    Ram,
    Compute,
    Gpu,
    Beefy,
}

impl Preset {
    pub const ALL: [Preset; 5] = [
        Preset::Normal,
        Preset::Ram,
        Preset::Compute,
        Preset::Gpu,
        Preset::Beefy,
    ];
    pub fn needs_gpu(self) -> bool {
        matches!(self, Preset::Gpu | Preset::Beefy)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Normal => "normal",
            Preset::Ram => "ram",
            Preset::Compute => "compute",
            Preset::Gpu => "gpu",
            Preset::Beefy => "beefy",
        }
    }
}

impl FromStr for Preset {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "normal" => Ok(Preset::Normal),
            "ram" => Ok(Preset::Ram),
            "compute" => Ok(Preset::Compute),
            "gpu" => Ok(Preset::Gpu),
            "beefy" => Ok(Preset::Beefy),
            other => {
                anyhow::bail!("unknown preset `{other}` (normal, ram, compute, gpu, beefy)")
            }
        }
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Preset::Normal => "normal",
            Preset::Ram => "ram",
            Preset::Compute => "compute",
            Preset::Gpu => "gpu",
            Preset::Beefy => "beefy",
        })
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSpec {
    pub preset: Preset,
    pub flavor: String,
    pub disk_gb: u32,
    pub needs_gpu: bool,
}

pub fn resolve(preset: Preset, cfg: &Config, flavor_override: Option<&str>) -> ResolvedSpec {
    let pc = match preset {
        Preset::Normal => &cfg.presets.normal,
        Preset::Ram => &cfg.presets.ram,
        Preset::Compute => &cfg.presets.compute,
        Preset::Gpu => &cfg.presets.gpu,
        Preset::Beefy => &cfg.presets.beefy,
    };
    ResolvedSpec {
        preset,
        flavor: flavor_override.unwrap_or(&pc.flavor).to_string(),
        disk_gb: pc.disk_gb,
        needs_gpu: preset.needs_gpu(),
    }
}

pub fn resolve_default_for_run(gpu_detected: bool) -> Preset {
    if gpu_detected {
        Preset::Gpu
    } else {
        Preset::Normal
    }
}

#[derive(tabled::Tabled)]
struct PresetRow {
    preset: String,
    flavor: String,
    disk_gb: u32,
    gpu: bool,
}

pub async fn cmd_presets(cfg: &Config, json: bool) -> anyhow::Result<()> {
    let specs: Vec<_> = Preset::ALL.iter().map(|p| resolve(*p, cfg, None)).collect();
    if json {
        let v: Vec<_> = specs
            .iter()
            .map(|s| {
                serde_json::json!({
                    "preset": s.preset.to_string(),
                    "flavor": s.flavor,
                    "disk_gb": s.disk_gb,
                    "needs_gpu": s.needs_gpu,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let rows = specs
            .into_iter()
            .map(|s| PresetRow {
                preset: s.preset.to_string(),
                flavor: s.flavor,
                disk_gb: s.disk_gb,
                gpu: s.needs_gpu,
            })
            .collect();
        crate::ui::print_table(rows);
        println!(
            "region: {}   (override in {})",
            cfg.region,
            crate::config::config_path().display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!("GPU".parse::<Preset>().unwrap(), Preset::Gpu);
        assert_eq!("beefy".parse::<Preset>().unwrap(), Preset::Beefy);
        assert!("nope".parse::<Preset>().is_err());
    }

    #[test]
    fn resolve_pulls_flavor_from_config() {
        let cfg = Config::default();
        let s = resolve(Preset::Gpu, &cfg, None);
        assert_eq!(s.flavor, "pi2.4xlarge.4");
        assert_eq!(s.disk_gb, 200);
        assert!(s.needs_gpu);
        assert!(!resolve(Preset::Normal, &cfg, None).needs_gpu);
    }

    #[test]
    fn flavor_override_wins() {
        let cfg = Config::default();
        let s = resolve(Preset::Gpu, &cfg, Some("pi2.8xlarge.4"));
        assert_eq!(s.flavor, "pi2.8xlarge.4");
        assert_eq!(s.disk_gb, 200);
    }

    #[test]
    fn run_default_preset_follows_gpu_detection() {
        assert_eq!(resolve_default_for_run(true), Preset::Gpu);
        assert_eq!(resolve_default_for_run(false), Preset::Normal);
    }
}
