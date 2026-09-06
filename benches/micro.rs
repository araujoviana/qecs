//! Micro-benchmarks for the hot-path members. Run: `cargo bench`.
//! These are all far below a millisecond; the workflow bottleneck is the
//! network (see `examples/workflow_timing.rs`).
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use qecs::config;
use qecs::hwc::sign::{self, CanonicalParts};
use qecs::presets::{self, Preset};
use qecs::state::{StateStore, VmRecord};

fn s(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

fn bench_signing(c: &mut Criterion) {
    let query = [
        s("limit", "50"),
        s("marker", "13551d6b-755d-4757-b956-536f674975c0"),
    ];
    let headers = [
        s("Content-Type", "application/json"),
        s("Host", "ecs.ap-southeast-3.myhuaweicloud.com"),
        s("X-Sdk-Date", "20260905T120000Z"),
    ];
    let cp = CanonicalParts {
        method: "GET",
        uri: "/v1/9ae376399d0d4487a461c5b7ead77833/cloudservers/flavors",
        query: &query,
        headers: &headers,
        body: b"",
    };

    c.bench_function("sign::canonical_request", |b| {
        b.iter(|| sign::canonical_request(black_box(&cp)))
    });

    let cr = sign::canonical_request(&cp);
    let sts = sign::string_to_sign("20260905T120000Z", &cr);
    c.bench_function("sign::signature (hmac-sha256)", |b| {
        b.iter(|| sign::signature(black_box("a-secret-access-key-value"), black_box(&sts)))
    });

    c.bench_function("sign::full (canonical+sts+sig)", |b| {
        b.iter(|| {
            let cr = sign::canonical_request(black_box(&cp));
            let sts = sign::string_to_sign("20260905T120000Z", &cr);
            sign::signature("a-secret-access-key-value", &sts)
        })
    });
}

fn bench_config(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("config.toml");
    std::fs::write(
        &p,
        "region = \"sa-brazil-1\"\n[presets.gpu]\nflavor = \"pi2.8xlarge.4\"\n",
    )
    .unwrap();
    c.bench_function("config::load_config (merge from file)", |b| {
        b.iter(|| config::load_config(black_box(Some(p.as_path()))).unwrap())
    });
}

fn bench_presets(c: &mut Criterion) {
    let cfg = config::Config::default();
    c.bench_function("presets::resolve x5", |b| {
        b.iter(|| {
            for pre in Preset::ALL {
                black_box(presets::resolve(pre, black_box(&cfg), None));
            }
        })
    });
}

fn bench_state(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::at(dir.path().join("vms.json"));
    let rec = VmRecord {
        id: "id-a".into(),
        name: "qecs-gpu-ab12".into(),
        preset: "gpu".into(),
        flavor: "pi2.4xlarge.4".into(),
        region: "ap-southeast-3".into(),
        az: "ap-southeast-3a".into(),
        eip: Some("1.2.3.4".into()),
        private_ip: Some("192.168.0.10".into()),
        created_at: "2026-09-05T12:00:00Z".into(),
        ttl_secs: 7200,
        connect_port: Some(22),
        job: None,
        tags: vec![],
    };
    c.bench_function("state::upsert + list (fs round trip)", |b| {
        b.iter(|| {
            store.upsert(black_box(rec.clone())).unwrap();
            black_box(store.list().unwrap());
        })
    });
}

criterion_group!(
    benches,
    bench_signing,
    bench_config,
    bench_presets,
    bench_state
);
criterion_main!(benches);
