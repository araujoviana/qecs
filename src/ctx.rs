use crate::cli::{Cli, GlobalArgs};
use crate::config::Config;
use crate::creds::{self, CredInput, Credentials};
use crate::hwc::client::SignedClient;
use crate::telemetry::Telemetry;

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
        let creds = creds::resolve(CredInput {
            flag_ak: cli.global.ak.as_deref(),
            flag_sk: cli.global.sk.as_deref(),
            profile: cli.global.profile.as_deref(),
            config: &config,
            allow_prompt,
        })?;
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
        self.global
            .region
            .clone()
            .unwrap_or_else(|| self.config.region.clone())
    }

    pub fn signed(&self) -> SignedClient {
        SignedClient::new(self.http.clone(), self.creds.clone())
    }
}
