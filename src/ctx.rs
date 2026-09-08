use crate::cli::{Cli, GlobalArgs};
use crate::config::Config;
use crate::creds::{self, CredInput, Credentials};
use crate::hwc::client::SignedClient;
use crate::telemetry::{Telemetry, TelemetryExt};

pub struct Ctx {
    pub config: Config,
    pub creds: Credentials,
    pub http: reqwest::Client,
    pub global: GlobalArgs,
    pub telemetry: Option<Telemetry>,
}

impl Ctx {
    pub fn load(cli: &Cli, config: Config, telemetry: Option<Telemetry>) -> anyhow::Result<Ctx> {
        let allow_prompt = !cli.global.json && !cli.global.quiet;
        let env_profile = std::env::var("QECS_PROFILE").ok();
        let profile = cli.global.profile.as_deref().or(env_profile.as_deref());
        let (creds, cred_source) = telemetry.phase_sync("creds-resolve", || {
            creds::resolve(CredInput {
                flag_ak: cli.global.ak.as_deref(),
                flag_sk: cli.global.sk.as_deref(),
                profile,
                config: &config,
                allow_prompt,
            })
        })?;
        telemetry.set_meta(|m| m.cred_source = Some(cred_source.label()));
        let http = reqwest::Client::builder()
            .user_agent(concat!("qecs/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Ctx {
            config,
            creds,
            http,
            global: cli.global.clone(),
            telemetry,
        })
    }

    pub fn region(&self) -> String {
        let env = std::env::var("QECS_REGION").ok();
        resolve_region(
            self.global.region.as_deref(),
            env.as_deref(),
            &self.config.region,
        )
    }

    pub fn signed(&self) -> SignedClient {
        SignedClient::new(self.http.clone(), self.creds.clone())
            .with_telemetry(self.telemetry.clone())
    }
}

pub fn resolve_region(flag: Option<&str>, env: Option<&str>, cfg: &str) -> String {
    if let Some(f) = flag.filter(|s| !s.is_empty()) {
        return f.to_string();
    }
    if let Some(e) = env.filter(|s| !s.is_empty()) {
        return e.to_string();
    }
    cfg.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qecs_region_env_overrides_config_not_flag() {
        assert_eq!(
            resolve_region(Some("flag-reg"), Some("env-reg"), "cfg-reg"),
            "flag-reg"
        );
        assert_eq!(resolve_region(None, Some("env-reg"), "cfg-reg"), "env-reg");
        assert_eq!(resolve_region(None, None, "cfg-reg"), "cfg-reg");
        assert_eq!(
            resolve_region(Some(""), Some("env-reg"), "cfg-reg"),
            "env-reg"
        );
    }

    #[test]
    fn qecs_profile_fallback() {
        let global_none = GlobalArgs::default();
        let env_p = Some("staging".to_string());
        let profile = global_none.profile.as_deref().or(env_p.as_deref());
        assert_eq!(profile, Some("staging"));

        let global_with_flag = GlobalArgs {
            profile: Some("prod".into()),
            ..Default::default()
        };
        let profile_flag = global_with_flag.profile.as_deref().or(env_p.as_deref());
        assert_eq!(profile_flag, Some("prod"));
    }
}
