//! One assembled context per invocation: config + creds + a shared HTTP client.
use crate::cli::{Cli, GlobalArgs};
use crate::config::{self, Config};
use crate::creds::{self, CredInput, Credentials};
use crate::hwc::client::SignedClient;

pub struct Ctx {
    pub config: Config,
    pub creds: Credentials,
    pub http: reqwest::Client,
    pub global: GlobalArgs,
}

impl Ctx {
    pub fn load(cli: &Cli) -> anyhow::Result<Ctx> {
        let config = config::load_config(None)?;
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
