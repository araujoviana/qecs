//! Telemetry recorder: opt-in, best-effort JSON Lines trace of a single qecs run.
//!
//! When enabled, every run streams a `.jsonl` trace to
//! `~/.local/state/qecs/traces/<timestamp>-<subcommand>-<run-id>.jsonl`. Writing is
//! strictly best-effort: any IO failure is logged once at `warn` and then swallowed;
//! telemetry never fails a command and never touches stdout/stderr.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

/// One signed HTTP request to a Huawei Cloud endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct HwcCall {
    pub method: String,
    pub host: String,
    pub path: String,
    pub status: u16,
    pub request_id: Option<String>,
    pub ttfb_ms: u64,
    pub total_ms: u64,
    pub resp_bytes: u64,
    pub phase: Option<String>,
}

/// The rollup of a single polling loop (the per-iteration requests still appear as
/// their own `hwc_call` events).
#[derive(Debug, Clone, Serialize)]
pub struct PollEvent {
    pub label: String,
    pub iterations: u32,
    pub total_ms: u64,
    pub final_interval_ms: u64,
    pub outcome: &'static str,
}

/// Run-level context filled in as the pipeline learns it; folded into the final
/// `run` event. Absent fields are omitted from the trace.
#[derive(Debug, Default, Clone, Serialize)]
pub struct RunMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub az: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flavor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_baked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cred_source: Option<String>,
}

struct Inner {
    run_id: String,
    subcommand: String,
    path: PathBuf,
    started: std::time::Instant,
    started_wall: DateTime<Utc>,
    seq: AtomicU64,
    writer: Mutex<Option<BufWriter<File>>>,
    meta: Mutex<RunMeta>,
    warned: AtomicBool,
}

/// Resolves whether telemetry is enabled: CLI flag wins (true), then env var (QECS_TELEMETRY),
/// then config file standing setting.
pub fn resolve_enabled(flag: bool, env: Option<&str>, config_enabled: bool) -> bool {
    if flag {
        return true;
    }
    match env.map(|s| s.trim().to_ascii_lowercase()) {
        Some(s) if matches!(s.as_str(), "1" | "true" | "yes" | "on") => true,
        Some(s) if matches!(s.as_str(), "0" | "false" | "no" | "off") => false,
        _ => config_enabled,
    }
}

/// Formats the stable subcommand name for telemetry run envelope and trace filename.
pub fn subcommand_label(cmd: &crate::cli::Commands) -> &'static str {
    use crate::cli::{Commands, ImageAction};
    match cmd {
        Commands::Run(_) => "run",
        Commands::Up(_) => "up",
        Commands::Shell(_) => "shell",
        Commands::Ls => "ls",
        Commands::Info(_) => "info",
        Commands::Logs(_) => "logs",
        Commands::Wait(_) => "wait",
        Commands::Kill(_) | Commands::Down(_) => "kill",
        Commands::Gc => "gc",
        Commands::Setup => "setup",
        Commands::Presets => "presets",
        Commands::Image(a) => match a.action {
            ImageAction::Build(_) => "image-build",
            ImageAction::Ls => "image-ls",
            ImageAction::Delete(_) => "image-delete",
        },
        Commands::Cache(a) => match a.action {
            crate::cli::CacheAction::Ls => "cache-ls",
            crate::cli::CacheAction::Clean { .. } => "cache-clean",
            crate::cli::CacheAction::Destroy { .. } => "cache-destroy",
        },
        Commands::Attach(_) => "attach",
        Commands::Completion(_) => "completion",
    }
}

/// Canonical phase names (stable identifiers per spec).
pub const PHASES: &[&str] = &[
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

tokio::task_local! {
    pub(crate) static PHASE: &'static str;
}

pub(crate) fn current_phase() -> Option<&'static str> {
    PHASE.try_with(|p| *p).ok()
}

pub struct PhaseGuard {
    tel: Telemetry,
    name: &'static str,
    start: std::time::Instant,
    started_wall: DateTime<Utc>,
    ok: bool,
    err: Option<String>,
}

impl PhaseGuard {
    /// Mark this phase as failed, recording the first line of `e` (truncated) as
    /// the trace `err`. Accepts anything `Display` so `anyhow::Error`, `io::Error`
    /// and `&str` all work.
    pub fn fail<E: std::fmt::Display + ?Sized>(&mut self, e: &E) {
        self.ok = false;
        self.err = Some(truncate_err(&e.to_string(), 200));
    }

    pub fn ok(self) {
        // Drop emits with ok = true
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let dur_ms = self.start.elapsed().as_millis() as u64;
        let panicking = std::thread::panicking();
        let ok = self.ok && !panicking;
        let err = if panicking {
            Some("panicked".to_string())
        } else {
            self.err.take()
        };
        let mut body = json!({
            "name": self.name,
            "started_ts": self.started_wall.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "dur_ms": dur_ms,
            "ok": ok,
        });
        if let Some(e) = err {
            body["err"] = json!(e);
        }
        self.tel.emit("phase", body);
    }
}

fn truncate_err(s: &str, max: usize) -> String {
    let top_line = s.lines().next().unwrap_or(s);
    if top_line.len() <= max {
        top_line.to_string()
    } else {
        let mut end = max;
        while !top_line.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        top_line[..end].to_string()
    }
}

/// Clone-cheap recorder handle. Clones share one `Inner` (and one trace file);
/// `finish` consumes one handle, surviving clones drop as harmless no-ops.
#[derive(Clone)]
pub struct Telemetry {
    inner: Arc<Inner>,
}

/// 4 random bytes as 8 lowercase hex chars; falls back to the wall clock if
/// `/dev/urandom` is unreadable. Mirrors `provision::generate_short_id`.
fn run_id() -> String {
    let mut bytes = [0u8; 4];
    if let Ok(mut f) = File::open("/dev/urandom") {
        use std::io::Read;
        if f.read_exact(&mut bytes).is_ok() {
            return hex::encode(bytes);
        }
    }
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0);
    bytes.copy_from_slice(&(nanos as u32).to_le_bytes());
    hex::encode(bytes)
}

/// `~/.local/state/qecs/traces` (or `$XDG_STATE_HOME/qecs/traces`).
fn traces_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qecs/traces")
}

impl Telemetry {
    /// Returns `None` when telemetry is disabled or the trace file cannot be
    /// created (the failure is logged once at `warn`).
    pub fn init(enabled: bool, subcommand: &str) -> Option<Telemetry> {
        if !enabled {
            return None;
        }
        let dir = traces_dir();
        if let Err(e) = fs::create_dir_all(&dir) {
            log::warn!("telemetry: cannot create {}: {e}", dir.display());
            return None;
        }
        let now = Utc::now();
        let run_id = run_id();
        let name = format!(
            "{}-{}-{}.jsonl",
            now.format("%Y-%m-%dT%H-%M-%SZ"),
            subcommand,
            run_id
        );
        let path = dir.join(&name);
        let file = match File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("telemetry: cannot open trace file {name}: {e}");
                return None;
            }
        };
        Some(Telemetry {
            inner: Arc::new(Inner {
                run_id,
                subcommand: subcommand.to_string(),
                path,
                started: std::time::Instant::now(),
                started_wall: now,
                seq: AtomicU64::new(0),
                writer: Mutex::new(Some(BufWriter::new(file))),
                meta: Mutex::new(RunMeta::default()),
                warned: AtomicBool::new(false),
            }),
        })
    }

    pub fn trace_path(&self) -> &Path {
        &self.inner.path
    }

    pub fn phase(&self, name: &'static str) -> PhaseGuard {
        PhaseGuard {
            tel: self.clone(),
            name,
            start: std::time::Instant::now(),
            started_wall: Utc::now(),
            ok: true,
            err: None,
        }
    }

    pub async fn phase_async<F, T>(&self, name: &'static str, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let _g = self.phase(name);
        PHASE.scope(name, fut).await
    }

    /// Run a fallible async step as its own phase. Enters the `PHASE` task-local so
    /// every `hwc_call` made inside `fut` is attributed to `name`, and records
    /// `ok:false` with the error head if `fut` resolves to `Err`.
    pub async fn phase_try<F, T, E>(&self, name: &'static str, fut: F) -> Result<T, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        let mut guard = self.phase(name);
        let out = PHASE.scope(name, fut).await;
        if let Err(e) = &out {
            guard.fail(e);
        }
        out
    }

    /// Run a fallible synchronous step as its own phase, recording `ok:false` with
    /// the error head on `Err`.
    pub fn phase_sync<F, T, E>(&self, name: &'static str, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
        E: std::fmt::Display,
    {
        let mut guard = self.phase(name);
        let out = f();
        if let Err(e) = &out {
            guard.fail(e);
        }
        out
    }

    fn emit(&self, kind: &str, body: Value) {
        let seq = self.inner.seq.fetch_add(1, Ordering::Relaxed);
        let mut obj = serde_json::Map::new();
        obj.insert("run_id".into(), json!(self.inner.run_id));
        obj.insert("seq".into(), json!(seq));
        obj.insert(
            "ts".into(),
            json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        );
        obj.insert("kind".into(), json!(kind));
        if let Value::Object(m) = body {
            for (k, v) in m {
                obj.insert(k, v);
            }
        }

        let mut guard = self.inner.writer.lock().unwrap_or_else(|e| e.into_inner());
        let Some(w) = guard.as_mut() else { return };
        let res = (|| -> std::io::Result<()> {
            serde_json::to_writer(&mut *w, &Value::Object(obj))?;
            w.write_all(b"\n")?;
            w.flush()
        })();
        if let Err(e) = res {
            if !self.inner.warned.swap(true, Ordering::Relaxed) {
                log::warn!("telemetry: trace write failed: {e}");
            }
            *guard = None;
        }
    }

    /// Record one signed HTTP request.
    pub fn record_hwc(&self, ev: HwcCall) {
        self.emit(
            "hwc_call",
            serde_json::to_value(ev).unwrap_or_else(|_| json!({})),
        );
    }

    /// Record one polling-loop rollup.
    pub fn record_poll(&self, ev: PollEvent) {
        self.emit(
            "poll",
            serde_json::to_value(ev).unwrap_or_else(|_| json!({})),
        );
    }

    /// Mutate the run-level metadata folded into the final `run` event.
    pub fn set_meta(&self, f: impl FnOnce(&mut RunMeta)) {
        let mut m = self.inner.meta.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut m);
    }

    /// Write the single `run` event and release the trace file. Returns the total
    /// event count (for the `--verbose` summary line).
    pub fn finish(self, exit_code: i32) -> u64 {
        let wall_ms = self.inner.started.elapsed().as_millis() as u64;
        let meta = self
            .inner
            .meta
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let args: Vec<String> = run_args(&self.inner.subcommand);

        let mut body = serde_json::to_value(&meta).unwrap_or_else(|_| json!({}));
        if let Value::Object(map) = &mut body {
            map.insert("subcommand".into(), json!(self.inner.subcommand));
            map.insert("args".into(), json!(args));
            map.insert("qecs_version".into(), json!(env!("CARGO_PKG_VERSION")));
            map.insert("os".into(), json!(std::env::consts::OS));
            map.insert("arch".into(), json!(std::env::consts::ARCH));
            map.insert("exit_code".into(), json!(exit_code));
            map.insert("wall_ms".into(), json!(wall_ms));
        }
        self.emit("run", body);

        let total = self.inner.seq.load(Ordering::Relaxed);
        // Flush and close the file now; a surviving clone then drops as a no-op.
        *self.inner.writer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        total
    }
}

/// Best-effort process args with the leading subcommand token removed.
fn run_args(subcommand: &str) -> Vec<String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == subcommand) {
        args.remove(i);
    }
    args
}

/// No-op-friendly delegation so call sites can hold `Option<Telemetry>` and call
/// through without an `if let` at every hook.
pub trait TelemetryExt {
    fn record_hwc(&self, ev: HwcCall);
    fn record_poll(&self, ev: PollEvent);
    fn set_meta(&self, f: impl FnOnce(&mut RunMeta));
    fn phase(&self, name: &'static str) -> Option<PhaseGuard>;
    fn phase_async<F, T>(&self, name: &'static str, fut: F) -> impl std::future::Future<Output = T>
    where
        F: std::future::Future<Output = T>;
    fn phase_try<F, T, E>(
        &self,
        name: &'static str,
        fut: F,
    ) -> impl std::future::Future<Output = Result<T, E>>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display;
    fn phase_sync<F, T, E>(&self, name: &'static str, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
        E: std::fmt::Display;
}

impl TelemetryExt for Option<Telemetry> {
    fn record_hwc(&self, ev: HwcCall) {
        if let Some(t) = self {
            t.record_hwc(ev);
        }
    }
    fn record_poll(&self, ev: PollEvent) {
        if let Some(t) = self {
            t.record_poll(ev);
        }
    }
    fn set_meta(&self, f: impl FnOnce(&mut RunMeta)) {
        if let Some(t) = self {
            t.set_meta(f);
        }
    }
    fn phase(&self, name: &'static str) -> Option<PhaseGuard> {
        self.as_ref().map(|t| t.phase(name))
    }
    async fn phase_async<F, T>(&self, name: &'static str, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        match self {
            Some(t) => t.phase_async(name, fut).await,
            None => fut.await,
        }
    }
    async fn phase_try<F, T, E>(&self, name: &'static str, fut: F) -> Result<T, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        match self {
            Some(t) => t.phase_try(name, fut).await,
            None => fut.await,
        }
    }
    fn phase_sync<F, T, E>(&self, name: &'static str, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
        E: std::fmt::Display,
    {
        match self {
            Some(t) => t.phase_sync(name, f),
            None => f(),
        }
    }
}

impl TelemetryExt for Option<&Telemetry> {
    fn record_hwc(&self, ev: HwcCall) {
        if let Some(t) = self {
            t.record_hwc(ev);
        }
    }
    fn record_poll(&self, ev: PollEvent) {
        if let Some(t) = self {
            t.record_poll(ev);
        }
    }
    fn set_meta(&self, f: impl FnOnce(&mut RunMeta)) {
        if let Some(t) = self {
            t.set_meta(f);
        }
    }
    fn phase(&self, name: &'static str) -> Option<PhaseGuard> {
        self.map(|t| t.phase(name))
    }
    async fn phase_async<F, T>(&self, name: &'static str, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        match self {
            Some(t) => t.phase_async(name, fut).await,
            None => fut.await,
        }
    }
    async fn phase_try<F, T, E>(&self, name: &'static str, fut: F) -> Result<T, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        match self {
            Some(t) => t.phase_try(name, fut).await,
            None => fut.await,
        }
    }
    fn phase_sync<F, T, E>(&self, name: &'static str, f: F) -> Result<T, E>
    where
        F: FnOnce() -> Result<T, E>,
        E: std::fmt::Display,
    {
        match self {
            Some(t) => t.phase_sync(name, f),
            None => f(),
        }
    }
}

#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static M: std::sync::Mutex<()> = std::sync::Mutex::new(());
    M.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
pub(crate) struct EnvScope {
    home: Option<String>,
    xdg: Option<String>,
}

#[cfg(test)]
impl EnvScope {
    pub(crate) fn new(home: &std::path::Path) -> Self {
        let prev = EnvScope {
            home: std::env::var("HOME").ok(),
            xdg: std::env::var("XDG_STATE_HOME").ok(),
        };
        unsafe {
            std::env::set_var("HOME", home);
            std::env::remove_var("XDG_STATE_HOME");
        }
        prev
    }
}

#[cfg(test)]
impl Drop for EnvScope {
    fn drop(&mut self) {
        unsafe {
            match &self.home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match &self.xdg {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_guard_records_duration_and_emits_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").unwrap();
        {
            let _guard = t.phase("ensure-vpc");
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        t.finish(0);

        let file = single_trace(tmp.path());
        let lines: Vec<String> = std::fs::read_to_string(file)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(lines.len(), 2);
        let l0: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(l0["kind"], "phase");
        assert_eq!(l0["name"], "ensure-vpc");
        assert!(l0["dur_ms"].as_u64().unwrap() >= 15);
        assert_eq!(l0["ok"], true);
        assert!(l0["started_ts"].is_string());
    }

    #[test]
    fn event_roundtrips_to_jsonl() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").expect("telemetry should init");
        t.record_poll(PollEvent {
            label: "wait-active x".into(),
            iterations: 3,
            total_ms: 900,
            final_interval_ms: 500,
            outcome: "ready",
        });
        t.finish(0);

        let dir = tmp.path().join(".local/state/qecs/traces");
        let file = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .expect("one jsonl file");
        let body = std::fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "two lines");

        let l0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let l1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(l0["kind"], "poll");
        assert_eq!(l0["iterations"], 3);
        assert_eq!(l0["seq"], 0);
        assert_eq!(l1["kind"], "run");
        assert_eq!(l1["exit_code"], 0);
        assert_eq!(l1["seq"], 1);
    }

    #[test]
    fn disabled_init_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        assert!(Telemetry::init(false, "run").is_none());
        assert!(!traces_dir().exists(), "no traces dir when disabled");
    }

    #[test]
    fn seq_is_contiguous_across_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "up").unwrap();
        t.record_hwc(HwcCall {
            method: "GET".into(),
            host: "iam.example.com".into(),
            path: "/v3/auth/projects".into(),
            status: 200,
            request_id: Some("req-1".into()),
            ttfb_ms: 10,
            total_ms: 20,
            resp_bytes: 128,
            phase: None,
        });
        t.record_poll(PollEvent {
            label: "wait-active srv".into(),
            iterations: 2,
            total_ms: 100,
            final_interval_ms: 50,
            outcome: "ready",
        });
        let total = t.finish(0);
        assert_eq!(total, 3);

        let file = single_trace(tmp.path());
        let seqs: Vec<i64> = std::fs::read_to_string(file)
            .unwrap()
            .lines()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[test]
    fn filename_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").unwrap();
        t.finish(0);

        let dir = tmp.path().join(".local/state/qecs/traces");
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert_eq!(entries.len(), 1);
        let name = entries[0].file_name().into_string().unwrap();
        assert!(matches_filename(&name), "bad trace filename: {name}");
    }

    #[test]
    fn option_ext_noops_on_none() {
        let t: Option<Telemetry> = None;
        t.record_poll(PollEvent {
            label: "x".into(),
            iterations: 0,
            total_ms: 0,
            final_interval_ms: 0,
            outcome: "ready",
        });
        t.record_hwc(HwcCall {
            method: "GET".into(),
            host: "h".into(),
            path: "/".into(),
            status: 0,
            request_id: None,
            ttfb_ms: 0,
            total_ms: 0,
            resp_bytes: 0,
            phase: None,
        });
        t.set_meta(|m| m.region = Some("r".into()));
    }

    #[test]
    fn finish_on_one_clone_then_drop_other_is_safe() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").unwrap();
        let survivor = t.clone();
        t.finish(0);
        drop(survivor); // must not panic, must not write a second `run` line

        let body = std::fs::read_to_string(single_trace(tmp.path())).unwrap();
        let runs = body
            .lines()
            .filter(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"] == "run")
            .count();
        assert_eq!(runs, 1, "exactly one run line");
    }

    #[test]
    fn set_meta_survives_to_run_event() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").unwrap();
        t.set_meta(|m| m.region = Some("ap-southeast-3".into()));
        t.finish(0);

        let file = single_trace(tmp.path());
        let body = std::fs::read_to_string(file).unwrap();
        let run_line: serde_json::Value = serde_json::from_str(
            body.lines()
                .find(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"] == "run")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(run_line["region"], "ap-southeast-3");
    }

    #[test]
    fn resolve_enabled_precedence() {
        assert!(resolve_enabled(true, Some("0"), false));
        assert!(resolve_enabled(false, Some("1"), false));
        assert!(!resolve_enabled(false, Some("off"), true));
        assert!(resolve_enabled(false, None, true));
        assert!(resolve_enabled(false, Some("banana"), true));
        assert!(!resolve_enabled(false, Some("banana"), false));
    }

    #[test]
    fn phase_guard_emits_with_ok_false_on_unwind() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").unwrap();
        let t_clone = t.clone();
        let handle = std::thread::spawn(move || {
            let _guard = t_clone.phase("unwind-test");
            panic!("deliberate panic for unwind test");
        });
        let _ = handle.join();
        t.finish(0);

        let file = single_trace(tmp.path());
        let body = std::fs::read_to_string(file).unwrap();
        let l0: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(l0["kind"], "phase");
        assert_eq!(l0["name"], "unwind-test");
        assert_eq!(l0["ok"], false);
        assert_eq!(l0["err"], "panicked");
    }

    #[test]
    fn phase_guard_fail_sets_err() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").unwrap();
        {
            let mut guard = t.phase("fail-test");
            guard.fail(&anyhow::anyhow!("boom\nsecond line"));
        }
        t.finish(0);

        let file = single_trace(tmp.path());
        let body = std::fs::read_to_string(file).unwrap();
        let l0: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(l0["kind"], "phase");
        assert_eq!(l0["name"], "fail-test");
        assert_eq!(l0["ok"], false);
        assert_eq!(l0["err"], "boom");
    }

    #[tokio::test]
    async fn phase_async_sets_task_local() {
        let tmp = tempfile::tempdir().unwrap();
        let t = {
            let _g = env_lock();
            let _e = EnvScope::new(tmp.path());
            Telemetry::init(true, "run").unwrap()
        };

        assert_eq!(current_phase(), None);
        let inside = t.phase_async("test-phase", async { current_phase() }).await;
        assert_eq!(inside, Some("test-phase"));
        assert_eq!(current_phase(), None);
    }

    #[tokio::test]
    async fn option_phase_noop() {
        let t: Option<Telemetry> = None;
        assert!(t.phase("x").is_none());
        let val = t.phase_async("x", async { 42 }).await;
        assert_eq!(val, 42);
        let ok: Result<i32, String> = t.phase_try("x", async { Ok(7) }).await;
        assert_eq!(ok.unwrap(), 7);
        let s: Result<i32, String> = t.phase_sync("x", || Ok(9));
        assert_eq!(s.unwrap(), 9);
    }

    #[tokio::test]
    async fn phase_try_attributes_phase_and_records_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let t = {
            let _g = env_lock();
            let _e = EnvScope::new(tmp.path());
            Telemetry::init(true, "run").unwrap()
        };

        // Ok path: task-local is set inside the future, cleared after.
        let seen: Result<Option<&'static str>, String> = t
            .phase_try("wait-active", async { Ok(current_phase()) })
            .await;
        assert_eq!(seen.unwrap(), Some("wait-active"));
        assert_eq!(current_phase(), None);

        // Err path: phase is recorded ok:false with the error head.
        let failed: Result<(), anyhow::Error> = t
            .phase_try("create-ecs", async {
                anyhow::bail!("HWC 403 Forbidden\ndetail line")
            })
            .await;
        assert!(failed.is_err());
        t.finish(1);

        let body = std::fs::read_to_string(single_trace(tmp.path())).unwrap();
        let create: serde_json::Value = body
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .find(|v: &serde_json::Value| v["name"] == "create-ecs")
            .expect("create-ecs phase event");
        assert_eq!(create["kind"], "phase");
        assert_eq!(create["ok"], false);
        assert_eq!(create["err"], "HWC 403 Forbidden");
    }

    #[test]
    fn phase_sync_records_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = env_lock();
        let _e = EnvScope::new(tmp.path());

        let t = Telemetry::init(true, "run").unwrap();
        let r: Result<(), anyhow::Error> =
            t.phase_sync("workdir-pack", || anyhow::bail!("tar: permission denied"));
        assert!(r.is_err());
        t.finish(1);

        let body = std::fs::read_to_string(single_trace(tmp.path())).unwrap();
        let l0: serde_json::Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(l0["name"], "workdir-pack");
        assert_eq!(l0["ok"], false);
        assert_eq!(l0["err"], "tar: permission denied");
    }

    #[test]
    fn canonical_phases_match_spec() {
        let expected = &[
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

    // --- test helpers -----------------------------------------------------

    fn single_trace(home: &std::path::Path) -> PathBuf {
        let dir = home.join(".local/state/qecs/traces");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        assert_eq!(files.len(), 1, "exactly one trace file");
        files.pop().unwrap()
    }

    /// `^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}Z-run-[0-9a-f]{8}\.jsonl$`
    fn matches_filename(name: &str) -> bool {
        let Some(stem) = name.strip_suffix(".jsonl") else {
            return false;
        };
        let Some(rest) = stem.strip_suffix(|c: char| c.is_ascii_hexdigit()) else {
            return false;
        };
        // 8 hex chars total at the end
        let hex_tail = &stem[stem.len() - 8..];
        if !hex_tail.bytes().all(|b| b.is_ascii_hexdigit()) {
            return false;
        }
        let _ = rest;
        let Some(ts) = stem.strip_suffix(&format!("-run-{hex_tail}")) else {
            return false;
        };
        let digits_dashes = |s: &str, shape: &str| {
            s.len() == shape.len()
                && s.chars().zip(shape.chars()).all(|(c, k)| match k {
                    'd' => c.is_ascii_digit(),
                    x => c == x,
                })
        };
        digits_dashes(ts, "dddd-dd-ddTdd-dd-ddZ")
    }
}
