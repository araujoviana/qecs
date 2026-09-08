//! Provisioning orchestrator: resolves resources concurrently, renders cloud-init,
//! submits the ECS create, polls until ACTIVE, and updates local state.

use anyhow::Context;
use chrono::{Duration as ChronoDuration, Utc};
use std::time::Duration;

use crate::cloudinit;
use crate::ctx::Ctx;
use crate::hwc::ecs::{self, CreateServer};
use crate::hwc::endpoints::Service;
use crate::hwc::flavors;
use crate::hwc::iam;
use crate::hwc::images::{self, Platform};
use crate::hwc::jobs;
use crate::hwc::keypair;
use crate::hwc::security_group;
use crate::hwc::vpc;
use crate::hwc::wait::PollConfig;
use crate::keys;
use crate::presets::{self, Preset};
use crate::state::{StateStore, VmRecord};
use crate::telemetry::TelemetryExt;

#[derive(Debug, Clone, Default)]
pub struct ProvisionOptions {
    pub preset: Option<Preset>,
    pub flavor: Option<String>,
    pub name: Option<String>,
    pub ttl: Option<String>,
    pub dry_run: bool,
    pub no_baked_image: bool,
}

/// Parse a human duration string (e.g. "2h", "30m", "120s", or raw minutes).
pub fn parse_duration(s: &str) -> anyhow::Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("empty duration");
    }

    if let Some(h) = s.strip_suffix('h').or_else(|| s.strip_suffix("hr")) {
        let n: u64 = h.trim().parse().context("invalid hours in duration")?;
        return Ok(Duration::from_secs(n * 3600));
    }
    if let Some(m) = s.strip_suffix('m').or_else(|| s.strip_suffix("min")) {
        let n: u64 = m.trim().parse().context("invalid minutes in duration")?;
        return Ok(Duration::from_secs(n * 60));
    }
    if let Some(sec) = s.strip_suffix('s').or_else(|| s.strip_suffix("sec")) {
        let n: u64 = sec.trim().parse().context("invalid seconds in duration")?;
        return Ok(Duration::from_secs(n));
    }

    // Default to minutes if purely numeric
    if let Ok(n) = s.parse::<u64>() {
        return Ok(Duration::from_secs(n * 60));
    }

    anyhow::bail!("unrecognized duration format `{s}` (expected e.g. `2h`, `30m`, `3600s`)")
}

/// Generate a unique short hex ID (4 hex characters) from system entropy.
fn generate_short_id() -> String {
    let mut bytes = [0u8; 2];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut bytes);
    } else {
        let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0);
        bytes[0] = (nanos & 0xff) as u8;
        bytes[1] = ((nanos >> 8) & 0xff) as u8;
    }
    hex::encode(bytes)
}

/// Generate VM name from template (e.g. `qecs-{preset}-{shortid}`).
pub fn generate_vm_name(template: &str, preset: &str) -> String {
    let short_id = generate_short_id();
    template
        .replace("{preset}", preset)
        .replace("{shortid}", &short_id)
}

/// Find the first AZ in `region` where `flavor_id` is sellable.
/// Returns `(az, cond_image)` where `cond_image` is the image constraint (if any) required by the flavor.
/// If `strict` is true (e.g. for GPU presets) and no AZ has stock, returns an error.
pub async fn pick_az(
    client: &crate::hwc::client::SignedClient,
    region: &str,
    project_id: &str,
    flavor_id: &str,
    strict: bool,
) -> anyhow::Result<(String, Option<String>)> {
    // Try typical AZ suffixes: a, b, c, d, e, f
    let candidates = ["a", "b", "c", "d", "e", "f"];
    for suffix in candidates {
        let az = format!("{region}{suffix}");
        if let Ok(Some(flavor)) =
            flavors::find_flavor(client, region, project_id, flavor_id, &az).await
        {
            return Ok((az, flavor.cond_image));
        }
    }

    if strict {
        anyhow::bail!(
            "flavor `{flavor_id}` is not available or sold out in any AZ for region `{region}`. \
             Try another region with better GPU availability (e.g. ap-southeast-3)."
        );
    }

    // Fall back to first AZ if not found via filter
    Ok((format!("{region}a"), None))
}

/// Orchestrate the provisioning of an ephemeral ECS VM.
/// If `dry_run` is set, validates all parameters with HWC without provisioning, returning `Ok(None)`.
pub async fn provision_vm(ctx: &Ctx, opts: &ProvisionOptions) -> anyhow::Result<Option<VmRecord>> {
    let region = ctx.region();
    let client = ctx.signed();

    // 1. Resolve preset & flavor
    let preset = opts.preset.unwrap_or(Preset::Normal);
    let resolved = presets::resolve(preset, &ctx.config, opts.flavor.as_deref());

    // 2. Discover IAM project id
    let project = ctx
        .telemetry
        .phase_try("iam-project", iam::discover_project(&client, &region))
        .await
        .context("discovering IAM project ID")?;

    // 3. Select target AZ where flavor is in stock (strict validation if GPU required)
    let (az, cond_image) = pick_az(
        &client,
        &region,
        &project.id,
        &resolved.flavor,
        resolved.needs_gpu,
    )
    .await?;

    // 4. Ensure local SSH keypair
    let (_key_paths, public_key) =
        keys::ensure_keypair(None).context("ensuring local qecs SSH keypair")?;

    // 5. Ensure core infrastructure
    // A: Ensure VPC
    let vpc = ctx
        .telemetry
        .phase_try(
            "ensure-vpc",
            vpc::ensure_vpc(&client, &region, &project.id, "qecs", "192.168.0.0/16"),
        )
        .await
        .context("ensuring VPC")?;

    // B: Run independent ensures concurrently (Subnet, Security Group + rules, Keypair, Image)
    let subnet_fut = vpc::ensure_subnet(
        &client,
        &region,
        &project.id,
        &vpc.id,
        "qecs",
        "192.168.0.0/24",
        "192.168.0.1",
        &az,
    );

    let sg_fut = async {
        let sg =
            security_group::ensure_security_group(&client, &region, &project.id, "qecs", &vpc.id)
                .await?;
        security_group::ensure_qecs_rules(&client, &region, &project.id, &sg.id).await?;
        Ok::<_, anyhow::Error>(sg)
    };

    let keypair_fut = keypair::import_keypair(&client, &region, &project.id, "qecs", &public_key);
    let image_fut = images::resolve_baked_or_gold_image(
        &client,
        &region,
        resolved.needs_gpu,
        cond_image.as_deref(),
        Platform::Ubuntu,
        !opts.no_baked_image,
    );

    let (subnet, sg, _kp, img) = tokio::try_join!(
        ctx.telemetry.phase_try("ensure-subnet", subnet_fut),
        ctx.telemetry.phase_try("ensure-sg", sg_fut),
        ctx.telemetry.phase_try("import-keypair", keypair_fut),
        ctx.telemetry.phase_try("resolve-image", image_fut),
    )?;

    // 6. Name and TTL
    let preset_str = preset.to_string();
    ctx.telemetry.set_meta(|m| {
        m.region = Some(region.clone());
        m.az = Some(az.clone());
        m.preset = Some(preset_str.clone());
        m.flavor = Some(resolved.flavor.clone());
        m.image_id = Some(img.id.clone());
        m.image_baked = Some(img.image_type == "private");
    });
    let vm_name = opts
        .name
        .clone()
        .unwrap_or_else(|| generate_vm_name(&ctx.config.name_template, &preset_str));

    let ttl_duration = parse_duration(opts.ttl.as_deref().unwrap_or(&ctx.config.max_lifetime))?;
    let auto_terminate_time = (Utc::now() + ChronoDuration::seconds(ttl_duration.as_secs() as i64))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    // 7. Cloud-init user data
    let idle_duration =
        parse_duration(&ctx.config.idle_timeout).unwrap_or(Duration::from_secs(1200));
    let relay = crate::connect::Relay::from_config(ctx.config.relay.as_ref())?;
    let user_data_b64 = {
        let _p = ctx.telemetry.phase("render-cloudinit");
        let raw = cloudinit::render_cloudinit(
            &public_key,
            ttl_duration.as_secs(),
            idle_duration.as_secs(),
            resolved.needs_gpu,
            Some(&relay),
        );
        cloudinit::base64_encode(raw.as_bytes())
    };

    // 8. Assemble CreateServer spec
    let spec = CreateServer {
        name: &vm_name,
        image_id: &img.id,
        flavor: &resolved.flavor,
        vpc_id: &vpc.id,
        subnet_id: &subnet.id,
        az: &az,
        key_name: "qecs",
        sg_id: &sg.id,
        root_volume_type: "GPSSD",
        root_volume_gb: resolved.disk_gb.max(img.min_disk),
        user_data_b64: Some(&user_data_b64),
        auto_terminate: Some(&auto_terminate_time),
        eip: true,
        tags: &[("managed-by", "qecs"), ("preset", &preset_str)],
    };

    // 9. If dry run: test and return
    if opts.dry_run {
        ecs::create_server_dry_run(&client, &region, &project.id, &spec)
            .await
            .context("HWC dry-run validation failed")?;
        return Ok(None);
    }

    // 10. Provision server
    let job_id = ctx
        .telemetry
        .phase_try(
            "create-ecs",
            ecs::create_server(&client, &region, &project.id, &spec),
        )
        .await
        .context("submitting ECS create request")?;

    let poll_cfg = PollConfig::default();
    let job_res = ctx
        .telemetry
        .phase_try(
            "wait-create-job",
            jobs::poll_job(
                &client,
                Service::Ecs,
                &region,
                &project.id,
                &job_id,
                &poll_cfg,
                ctx.telemetry.as_ref(),
            ),
        )
        .await
        .context("waiting for ECS create job to finish")?;

    let server_id = job_res
        .server_ids
        .first()
        .context("create job completed without returning a server_id")?;

    let server = ctx
        .telemetry
        .phase_try(
            "wait-active",
            ecs::wait_active(
                &client,
                &region,
                &project.id,
                server_id,
                &poll_cfg,
                ctx.telemetry.as_ref(),
            ),
        )
        .await
        .context("waiting for server to reach ACTIVE status")?;

    let rec = VmRecord {
        id: server.id,
        name: server.name,
        preset: preset_str,
        flavor: server.flavor,
        region: region.clone(),
        az: server.az,
        eip: server.public_ip,
        private_ip: server.private_ip,
        created_at: Utc::now().to_rfc3339(),
        ttl_secs: ttl_duration.as_secs(),
        connect_port: Some(22),
        job: None,
        tags: server.tags,
    };

    // Save record in local state store
    let store = StateStore::open()?;
    store.upsert(rec.clone())?;

    Ok(Some(rec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("15").unwrap(), Duration::from_secs(900));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn generates_name_from_template() {
        let name = generate_vm_name("qecs-{preset}-{shortid}", "normal");
        assert!(name.starts_with("qecs-normal-"));
        assert_eq!(name.len(), "qecs-normal-".len() + 4);
    }

    #[tokio::test]
    async fn test_pick_az_strict_fails_when_no_az_has_flavor() {
        let client = crate::hwc::client::SignedClient::new(
            reqwest::Client::new(),
            crate::creds::Credentials {
                ak: "dummy_ak".into(),
                sk: "dummy_sk".into(),
                security_token: None,
            },
        );

        let err = pick_az(
            &client,
            "ap-southeast-3",
            "test_proj",
            "gpu.unavailable",
            true,
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("gpu.unavailable"));
        assert!(err.to_string().contains("not available or sold out"));
    }

    #[tokio::test]
    async fn test_pick_az_non_strict_falls_back_to_first_az() {
        let client = crate::hwc::client::SignedClient::new(
            reqwest::Client::new(),
            crate::creds::Credentials {
                ak: "dummy_ak".into(),
                sk: "dummy_sk".into(),
                security_token: None,
            },
        );

        let (az, cond_image) = pick_az(
            &client,
            "ap-southeast-3",
            "test_proj",
            "normal.flavor",
            false,
        )
        .await
        .unwrap();

        assert_eq!(az, "ap-southeast-3a");
        assert_eq!(cond_image, None);
    }

    #[test]
    fn phase_names_are_the_canonical_set() {
        use crate::telemetry::PHASES;
        let expected = [
            "creds-resolve",
            "config-load",
            "iam-project",
            "ensure-vpc",
            "ensure-subnet",
            "ensure-sg",
            "import-keypair",
            "resolve-image",
            "render-cloudinit",
            "create-ecs",
            "wait-create-job",
            "wait-active",
            "ssh-probe",
            "workdir-pack",
            "workdir-upload",
            "gpu-wait",
            "job-exec",
            "output-download",
            "image-create",
            "destroy",
        ];
        assert_eq!(PHASES, expected);
    }
}
