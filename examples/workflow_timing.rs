//! Phase-by-phase timing of the real `qecs` -> Huawei Cloud `list flavors`
//! workflow, so you can see where the time goes (spoiler: the network).
//!
//! Run:
//!   QECS_AK=.. QECS_SK=.. QECS_PROJECT_ID=.. QECS_REGION=ap-southeast-3 \
//!     cargo run --release --example workflow_timing
use std::time::{Duration, Instant};

use qecs::config;
use qecs::creds::{self, CredInput};
use qecs::hwc::endpoints::{Service, endpoint_host};
use qecs::hwc::sign::{self, CanonicalParts};
use tabled::{Table, Tabled, settings::Style};

#[derive(Tabled)]
struct Row {
    phase: String,
    #[tabled(rename = "duration")]
    dur: String,
    #[tabled(rename = "% of total")]
    pct: String,
}

fn ms(d: Duration) -> String {
    format!("{:>9.3} ms", d.as_secs_f64() * 1000.0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let region = std::env::var("QECS_REGION").unwrap_or_else(|_| "ap-southeast-3".into());
    let project = std::env::var("QECS_PROJECT_ID")
        .expect("set QECS_PROJECT_ID (see README: found in a flavor's `links` href)");

    let mut timings: Vec<(String, Duration)> = Vec::new();
    macro_rules! phase {
        ($name:expr, $body:expr) => {{
            let t = Instant::now();
            let out = $body;
            timings.push(($name.to_string(), t.elapsed()));
            out
        }};
    }

    let cfg = phase!("load config", config::load_config(None)?);

    let creds = phase!(
        "resolve credentials",
        creds::resolve(CredInput {
            flag_ak: None,
            flag_sk: None,
            profile: None,
            config: &cfg,
            allow_prompt: false,
        })?
    );

    let http = phase!(
        "build reqwest client",
        reqwest::Client::builder()
            .user_agent(concat!("qecs/", env!("CARGO_PKG_VERSION")))
            .build()?
    );

    let host = endpoint_host(Service::Ecs, &region);
    let url = format!("https://{host}/v1/{project}/cloudservers/flavors");

    let (headers, _signed) = phase!("sign request", {
        let sdk_date = chrono::Utc::now().format(sign::DATE_FORMAT).to_string();
        let hv = vec![
            ("host".to_string(), host.clone()),
            ("x-sdk-date".to_string(), sdk_date.clone()),
        ];
        let cr = sign::canonical_request(&CanonicalParts {
            method: "GET",
            uri: &format!("/v1/{project}/cloudservers/flavors"),
            query: &[],
            headers: &hv,
            body: b"",
        });
        let sts = sign::string_to_sign(&sdk_date, &cr);
        let sig = sign::signature(&creds.sk, &sts);
        let (_c, signed) = sign::canonical_headers(&hv);
        let auth = sign::authorization_header(&creds.ak, &signed, &sig);
        (
            vec![
                ("Host".to_string(), host.clone()),
                ("X-Sdk-Date".to_string(), sdk_date),
                ("Authorization".to_string(), auth),
            ],
            signed,
        )
    });

    let mut req = http.get(&url);
    for (k, v) in &headers {
        req = req.header(k, v);
    }

    let resp = phase!("HTTP send (DNS + TLS + request + TTFB)", req.send().await?);
    let status = resp.status();
    let wire_len = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let body = phase!("download + decompress response body", resp.bytes().await?);
    let parsed: serde_json::Value = phase!("parse JSON", serde_json::from_slice(&body)?);

    let count = parsed
        .get("flavors")
        .and_then(|f| f.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let total: Duration = timings.iter().map(|(_, d)| *d).sum();
    let rows: Vec<Row> = timings
        .iter()
        .map(|(name, d)| Row {
            phase: name.clone(),
            dur: ms(*d),
            pct: format!("{:>5.1}%", d.as_secs_f64() / total.as_secs_f64() * 100.0),
        })
        .chain(std::iter::once(Row {
            phase: "TOTAL".into(),
            dur: ms(total),
            pct: "100.0%".into(),
        }))
        .collect();

    let wire = wire_len
        .map(|n| format!("{n} B on the wire"))
        .unwrap_or_else(|| "wire size unknown".into());
    println!(
        "\nHTTP {status}  |  {count} flavors  |  {} B decoded  ({wire})\n",
        body.len()
    );
    println!("{}", Table::new(rows).with(Style::rounded()));
    println!(
        "\nThe network phases dominate; everything qecs does locally is well under a millisecond. \
         Provision-time validation should use `flavors::find_flavor` (filtered, ~1.7 s), not the full list."
    );
    Ok(())
}
