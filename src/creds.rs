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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredSource {
    Flag,
    Env(&'static str),
    Config,
    EnvFile(String),
    Prompt,
}

impl CredSource {
    pub fn label(&self) -> String {
        match self {
            CredSource::Flag => "flag".to_string(),
            CredSource::Env(var) => format!("env:{var}"),
            CredSource::Config => "config".to_string(),
            CredSource::EnvFile(path) => format!("env-file:{path}"),
            CredSource::Prompt => "prompt".to_string(),
        }
    }
}

pub struct CredInput<'a> {
    pub flag_ak: Option<&'a str>,
    pub flag_sk: Option<&'a str>,
    pub profile: Option<&'a str>,
    pub config: &'a Config,
    pub allow_prompt: bool,
}

pub fn resolve(input: CredInput) -> anyhow::Result<(Credentials, CredSource)> {
    if let (Some(ak), Some(sk)) = (input.flag_ak, input.flag_sk) {
        return Ok((
            Credentials {
                ak: ak.into(),
                sk: sk.into(),
                security_token: None,
            },
            CredSource::Flag,
        ));
    }
    const TOKEN_KEYS: &[&str] = &["QECS_SECURITY_TOKEN", "HWC_SECURITY_TOKEN"];
    for (a, s) in [
        ("QECS_AK", "QECS_SK"),
        ("HUAWEICLOUD_SDK_AK", "HUAWEICLOUD_SDK_SK"),
        ("HWC_AK", "HWC_SK"),
    ] {
        if let Some(c) = from_env_pair(a, s, TOKEN_KEYS) {
            return Ok((c, CredSource::Env(a)));
        }
    }
    if let Some(ic) = &input.config.credentials {
        return Ok((
            Credentials {
                ak: ic.ak.clone(),
                sk: ic.sk.clone(),
                security_token: None,
            },
            CredSource::Config,
        ));
    }
    if let Some((c, src)) = from_env_files(&input.config.env_files, input.profile) {
        return Ok((c, src));
    }
    if input.allow_prompt {
        let c = prompt()?;
        return Ok((c, CredSource::Prompt));
    }
    anyhow::bail!(
        "no credentials found. set QECS_AK/QECS_SK (and optional QECS_SECURITY_TOKEN), run `qecs setup`, or pass --ak/--sk"
    )
}

fn from_env_pair(ak_key: &str, sk_key: &str, token_keys: &[&str]) -> Option<Credentials> {
    let ak = std::env::var(ak_key).ok().filter(|s| !s.is_empty())?;
    let sk = std::env::var(sk_key).ok().filter(|s| !s.is_empty())?;
    let security_token = token_keys
        .iter()
        .find_map(|&k| std::env::var(k).ok().filter(|s| !s.is_empty()));
    Some(Credentials {
        ak,
        sk,
        security_token,
    })
}

fn from_env_files(files: &[PathBuf], profile: Option<&str>) -> Option<(Credentials, CredSource)> {
    for base in files {
        for candidate in candidates(base, profile) {
            if let Some(c) = parse_env_file(&candidate) {
                return Some((
                    c,
                    CredSource::EnvFile(candidate.to_string_lossy().into_owned()),
                ));
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
            .get("QECS_SECURITY_TOKEN")
            .or_else(|| map.get("HWC_SECURITY_TOKEN"))
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
            "QECS_SECURITY_TOKEN",
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
    fn qecs_security_token_beats_hwc() {
        let _g = lock();
        clear();
        unsafe {
            std::env::set_var("QECS_AK", "ak");
            std::env::set_var("QECS_SK", "sk");
            std::env::set_var("QECS_SECURITY_TOKEN", "q");
            std::env::set_var("HWC_SECURITY_TOKEN", "h");
        }
        let cfg = Config::default();
        let (c, _) = resolve(CredInput {
            flag_ak: None,
            flag_sk: None,
            profile: None,
            config: &cfg,
            allow_prompt: false,
        })
        .unwrap();
        assert_eq!(c.security_token.as_deref(), Some("q"));
        clear();
    }

    #[test]
    fn reports_cred_source() {
        let _g = lock();
        clear();
        let cfg = Config::default();

        // 1. Flag
        let (_c, src) = resolve(CredInput {
            flag_ak: Some("flagak"),
            flag_sk: Some("flagsk"),
            profile: None,
            config: &cfg,
            allow_prompt: false,
        })
        .unwrap();
        assert_eq!(src, CredSource::Flag);
        assert_eq!(src.label(), "flag");

        // 2. Env
        unsafe {
            std::env::set_var("QECS_AK", "ak");
            std::env::set_var("QECS_SK", "sk");
        }
        let (_c, src) = resolve(CredInput {
            flag_ak: None,
            flag_sk: None,
            profile: None,
            config: &cfg,
            allow_prompt: false,
        })
        .unwrap();
        assert_eq!(src, CredSource::Env("QECS_AK"));
        assert_eq!(src.label(), "env:QECS_AK");
        clear();

        // 3. Env file
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env.custom");
        std::fs::write(&env_path, "HWC_AK=fileak\nHWC_SK=filesk\n").unwrap();
        let cfg_file = Config {
            env_files: vec![dir.path().join(".env")],
            ..Config::default()
        };
        let (_c, src) = resolve(CredInput {
            flag_ak: None,
            flag_sk: None,
            profile: Some("custom"),
            config: &cfg_file,
            allow_prompt: false,
        })
        .unwrap();
        assert_eq!(
            src,
            CredSource::EnvFile(env_path.to_string_lossy().into_owned())
        );
        assert_eq!(src.label(), format!("env-file:{}", env_path.display()));
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
        let (c, _) = resolve(CredInput {
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
        let (c, _) = resolve(CredInput {
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
        std::fs::write(
            dir.path().join(".env.staging"),
            "HWC_AK=fileak\nHWC_SK=filesk\n",
        )
        .unwrap();
        let cfg = Config {
            env_files: vec![dir.path().join(".env")],
            ..Config::default()
        };
        let (c, _) = resolve(CredInput {
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
