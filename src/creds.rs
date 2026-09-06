//! AK/SK resolution. Order (first hit wins): flags, QECS_*, HUAWEICLOUD_SDK_*,
//! HWC_*, config [credentials], .env files, interactive prompt.
use crate::config::Config;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Credentials {
    pub ak: String,
    pub sk: String,
    pub security_token: Option<String>,
}

pub struct CredInput<'a> {
    pub flag_ak: Option<&'a str>,
    pub flag_sk: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub config: &'a Config,
    pub allow_prompt: bool,
}

pub fn resolve(input: CredInput) -> anyhow::Result<Credentials> {
    if let (Some(ak), Some(sk)) = (input.flag_ak, input.flag_sk) {
        return Ok(Credentials {
            ak: ak.into(),
            sk: sk.into(),
            security_token: None,
        });
    }
    for (a, s, t) in [
        ("QECS_AK", "QECS_SK", "HWC_SECURITY_TOKEN"),
        ("HUAWEICLOUD_SDK_AK", "HUAWEICLOUD_SDK_SK", "HWC_SECURITY_TOKEN"),
        ("HWC_AK", "HWC_SK", "HWC_SECURITY_TOKEN"),
    ] {
        if let Some(c) = from_env_pair(a, s, t) {
            return Ok(c);
        }
    }
    if let Some(ic) = &input.config.credentials {
        return Ok(Credentials {
            ak: ic.ak.clone(),
            sk: ic.sk.clone(),
            security_token: None,
        });
    }
    if let Some(c) = from_env_files(&input.config.env_files, input.profile) {
        return Ok(c);
    }
    if input.allow_prompt {
        return prompt();
    }
    anyhow::bail!("no credentials found. set QECS_AK/QECS_SK, run `qecs setup`, or pass --ak/--sk")
}

fn from_env_pair(ak_key: &str, sk_key: &str, token_key: &str) -> Option<Credentials> {
    let ak = std::env::var(ak_key).ok().filter(|s| !s.is_empty())?;
    let sk = std::env::var(sk_key).ok().filter(|s| !s.is_empty())?;
    Some(Credentials {
        ak,
        sk,
        security_token: std::env::var(token_key).ok().filter(|s| !s.is_empty()),
    })
}

fn from_env_files(files: &[PathBuf], profile: Option<&str>) -> Option<Credentials> {
    for base in files {
        for candidate in candidates(base, profile) {
            if let Some(c) = parse_env_file(&candidate) {
                return Some(c);
            }
        }
    }
    None
}

fn candidates(base: &Path, profile: Option<&str>) -> Vec<PathBuf> {
    match profile {
        Some(p) => {
            let mut name = base.file_name().map(|s| s.to_owned()).unwrap_or_default();
            name.push(".");
            name.push(p);
            vec![base.with_file_name(name)]
        }
        None => vec![base.to_path_buf()],
    }
}

fn parse_env_file(path: &Path) -> Option<Credentials> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut map = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().trim_matches('"').to_string());
        }
    }
    let ak = map.get("HWC_AK").filter(|s| !s.is_empty())?.clone();
    let sk = map.get("HWC_SK").filter(|s| !s.is_empty())?.clone();
    Some(Credentials {
        ak,
        sk,
        security_token: map
            .get("HWC_SECURITY_TOKEN")
            .cloned()
            .filter(|s| !s.is_empty()),
    })
}

fn prompt() -> anyhow::Result<Credentials> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        anyhow::bail!("no credentials and stdin is not a terminal");
    }
    print!("Huawei Cloud AK: ");
    std::io::stdout().flush()?;
    let mut ak = String::new();
    std::io::stdin().read_line(&mut ak)?;
    print!("Huawei Cloud SK: ");
    std::io::stdout().flush()?;
    let mut sk = String::new();
    std::io::stdin().read_line(&mut sk)?;
    Ok(Credentials {
        ak: ak.trim().into(),
        sk: sk.trim().into(),
        security_token: None,
    })
}

pub fn masked(ak: &str) -> String {
    let n = ak.len();
    if n <= 7 {
        return "*".repeat(n);
    }
    format!("{}{}{}", &ak[..4], "*".repeat(n - 7), &ak[n - 3..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static M: std::sync::Mutex<()> = std::sync::Mutex::new(());
        M.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn clear() {
        for k in [
            "QECS_AK",
            "QECS_SK",
            "HUAWEICLOUD_SDK_AK",
            "HUAWEICLOUD_SDK_SK",
            "HWC_AK",
            "HWC_SK",
            "HWC_SECURITY_TOKEN",
        ] {
            unsafe {
                std::env::remove_var(k);
            }
        }
    }

    #[test]
    fn flags_win_over_everything() {
        let _g = lock();
        clear();
        unsafe {
            std::env::set_var("QECS_AK", "envak");
            std::env::set_var("QECS_SK", "envsk");
        }
        let cfg = Config::default();
        let c = resolve(CredInput {
            flag_ak: Some("flagak"),
            flag_sk: Some("flagsk"),
            profile: None,
            config: &cfg,
            allow_prompt: false,
        })
        .unwrap();
        assert_eq!(c.ak, "flagak");
        clear();
    }

    #[test]
    fn qecs_env_beats_hwc_env() {
        let _g = lock();
        clear();
        unsafe {
            std::env::set_var("QECS_AK", "q");
            std::env::set_var("QECS_SK", "q");
            std::env::set_var("HWC_AK", "h");
            std::env::set_var("HWC_SK", "h");
        }
        let cfg = Config::default();
        let c = resolve(CredInput {
            flag_ak: None,
            flag_sk: None,
            profile: None,
            config: &cfg,
            allow_prompt: false,
        })
        .unwrap();
        assert_eq!(c.ak, "q");
        clear();
    }

    #[test]
    fn reads_env_file_with_profile() {
        let _g = lock();
        clear();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.staging"), "HWC_AK=fileak\nHWC_SK=filesk\n").unwrap();
        let cfg = Config {
            env_files: vec![dir.path().join(".env")],
            ..Config::default()
        };
        let c = resolve(CredInput {
            flag_ak: None,
            flag_sk: None,
            profile: Some("staging"),
            config: &cfg,
            allow_prompt: false,
        })
        .unwrap();
        assert_eq!(c.ak, "fileak");
        assert_eq!(c.sk, "filesk");
        clear();
    }

    #[test]
    fn missing_everything_is_an_error_when_prompt_disallowed() {
        let _g = lock();
        clear();
        let cfg = Config {
            env_files: vec![],
            ..Config::default()
        };
        let err = resolve(CredInput {
            flag_ak: None,
            flag_sk: None,
            profile: None,
            config: &cfg,
            allow_prompt: false,
        })
        .unwrap_err();
        assert!(err.to_string().contains("no credentials"));
        clear();
    }

    #[test]
    fn masked_hides_the_middle() {
        assert_eq!(masked("FM9RLCNabcdefNAXISK"), "FM9R************ISK");
        assert_eq!(masked("short"), "*****");
    }
}
