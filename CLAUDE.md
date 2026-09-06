# CLAUDE.md

Project-specific context for `qecs`. Read this before making changes.

## What qecs is

`qecs` ("quantum ECS") is a Rust CLI that provisions a strong, ephemeral Huawei Cloud
(HWC) ECS VM, runs one job on it, and destroys it before it costs money. Two workflows:

- **Abstracted**: `qecs run <path>` ships a workdir to a fresh VM, infers how to build/run
  it (detector ladder), streams output, pulls artifacts back to `./out`, destroys the VM.
- **Manual**: `qecs up` / `qecs shell` give an interactive ephemeral box, still TTL-guarded.

## Hard constraints

- **x86_64 only.** Linux is first-class; Windows/macOS support must never hold back a Linux
  capability.
- Rust **edition 2024**, toolchain **>= 1.98**. `[profile.release]` keeps `lto`,
  `codegen-units = 1`, `strip`, `panic = "abort"` - do not loosen these.
- **Fast beats readable.** Provisioning is aggressively parallelized (`tokio::join!`); cold-start
  latency wins over code clarity. Reuse the one `reqwest::Client` everywhere.
- **CLI stays dead-simple.** The 90% path is `qecs run` and `qecs up` with zero flags,
  non-interactive whenever creds+config are present. Scriptability is first-class:
  `--json` on read commands, stable exit codes (0 ok, 1 runtime error, 2 usage).
- **No em dashes and no AI-isms** in user-facing copy, README, or docs.
- **NEVER** put AI/Claude attribution in commits, PRs, README, or anywhere.
- `docs/superpowers/` and `.claude/` are gitignored and must never be pushed. Also gitignored:
  `.env*`, `keys/`, `*.tfstate*`, `.qecs/`, `/target`.

## HWC specifics

- **Auth**: AK/SK via a hand-rolled `SDK-HMAC-SHA256` signer in `src/hwc/sign.rs` (SigV4-like:
  `X-Sdk-Date` header, `Authorization: SDK-HMAC-SHA256 Access=..,SignedHeaders=..,Signature=..`).
  It is TDD'd against Huawei's official canonical-request reference vector in
  `tests/sign_vectors.rs`. **Do not "refactor" it without re-running those vectors.** Quirk:
  `canonical_uri` appends a trailing `/` - that is deliberate HWC behavior, not a bug.
- **Credential resolution** (first hit wins), in `src/creds.rs`: `--ak/--sk` flags;
  `QECS_AK/QECS_SK` (with optional `QECS_SECURITY_TOKEN` / `HWC_SECURITY_TOKEN`);
  `HUAWEICLOUD_SDK_AK/SK`; `HWC_AK/HWC_SK`; config `[credentials]`;
  `.env`/`.env.<profile>` files (default paths include `~/.config/qecs/.env` and
  `~/Projetos/python-projs/mcp-hwc/.env`, keys `HWC_AK`/`HWC_SK`/optional `QECS_SECURITY_TOKEN`/`HWC_SECURITY_TOKEN`);
  interactive prompt (TTY only, suppressed by `--json`/`--quiet`).
- **Default region `ap-southeast-3`** (best GPU stock; latency irrelevant for
  upload-once/download-once jobs). Config-overridable; non-GPU work can pin `sa-brazil-1`
  (~10x faster per round trip from Brazil, so CPU/RAM presets should prefer it).
- **Network**: HWC responses dominate wall time (RTT + server-side work), local work is
  sub-millisecond. `reqwest` has the `gzip` feature on (auto-decompress, no header handling
  needed; the flavors list is 1.3 MB -> ~64 KB on the wire). For flavor validation use
  `flavors::find_flavor` (server-side `flavor_id` + `availability_zone` filters, ~1.7 s,
  a few KB) -- **never** the full `list_flavors` catalog on the provision path. HWC sends no
  `ETag`/`Last-Modified`, so any on-disk cache must be TTL-only. Full brainstorm:
  `docs/superpowers/notes/2026-09-05-network-optimizations.md`.
- **project_id** is region-scoped. Read it from any flavor's `links[].href` in the ECS flavors
  response, or via IAM (branch 2's first task). Live test / timing example take it from
  `QECS_PROJECT_ID` as a stopgap.
- **Presets** (`src/presets.rs`, `src/config.rs`) map friendly names to flavor + disk, all
  overridable in `~/.config/qecs/config.toml` (`[presets.gpu] flavor = "..."`), `--flavor`
  overrides ad hoc: normal `s7n.2xlarge.2`/100GB, ram `m7.4xlarge.8`/100, compute
  `c7.8xlarge.2`/100, gpu `pi2.4xlarge.4`/200, beefy `p2s.8xlarge.8`/300. Ephemeral means
  large-without-consequence, so defaults skew strong. `flavors.rs::flavor_available_in_az`
  validates a flavor is sellable in the target AZ before any create call.

## Environment variables

The rule: every qecs-specific setting is `QECS_<SCREAMING_SNAKE>`, matching its flag and config key. Third-party-compatibility aliases exist only for credentials, are documented as a lower-precedence fallback, and are named after the tool they mimic.

| Canonical | Aliases (creds only, lower precedence) | Meaning |
|-----------|----------------------------------------|---------|
| `QECS_AK` / `QECS_SK` | `HUAWEICLOUD_SDK_AK/SK`, `HWC_AK/SK` | access / secret key |
| `QECS_SECURITY_TOKEN` | `HWC_SECURITY_TOKEN` | STS token (canonical token var) |
| `QECS_REGION` | - | region, below `--region`, above config |
| `QECS_PROFILE` | - | `.env.<profile>` selector, = `--profile` |
| `QECS_TELEMETRY` | - | enable telemetry |

Developer and tooling variables:
- `QECS_NO_BUMP=1`: bypasses the auto-version bump hook on manual `git commit --amend` (developer / tooling only).
- `QECS_PROJECT_ID`: overrides project discovery in tests and timing harnesses (test-harness only).

## Workflow / conventions

- **Conventional commits are REQUIRED.** `.githooks/post-commit` auto-bumps the `[package]`
  version from the commit type and folds it into the same commit via `--amend`:
  `feat!:`/`fix!:`/any `!:`/`BREAKING CHANGE` -> major; `feat:` -> minor; everything else
  (incl. no prefix) -> patch. Run `bash scripts/install-hooks.sh` once
  (`git config core.hooksPath .githooks`). For a manual `git commit --amend`, set
  `QECS_NO_BUMP=1` to avoid a double bump. Tags/releases are cut from CI on version change.
- **Feature-branch roadmap** (branches 0-9, full detail in the spec). Branches 0-9 are **done**
  (scaffold, HWC API, provision-core, connect, run, lifecycle, gpu, image-bake, tunnel, telemetry).
  All subcommands are implemented and covered by tests.
- Parallel/subagent work uses **git worktrees** (native EnterWorktree or `git worktree add`).
- The spec and plan live in `docs/superpowers/` (gitignored, local only) - read them for full
  detail on any subsystem:
  - `docs/superpowers/specs/2026-09-05-qecs-architecture-design.md`
  - `docs/superpowers/plans/2026-09-05-qecs-scaffold-and-hwc-api.md`

## Module map

`cli` (clap surface, kept small), `config` (TOML defaults + deep merge), `creds` (AK/SK chain),
`ctx` (per-invocation config + creds + shared http), `state` (advisory-locked
`~/.local/state/qecs/vms.json`; cloud is source of truth), `presets`, `ui` (colog logging /
indicatif spinner / tabled tables), `error` (`log_error_chain`),
`hwc/{sign,client,endpoints,flavors,ecs,vpc,ims}`. `hwc/{ecs,vpc,ims}` are currently just
serde response models; real calls land in branch 2.

## Build & test

Do not assume the `target` lock is free - another process may hold it.

- `cargo build` / `cargo run -- <cmd>` / `cargo run -- presets`
- `cargo test --all` - unit tests (`#[cfg(test)]` in `src/**`), plus:
  - `tests/sign_vectors.rs` - signer reference vectors
  - `tests/version_bump.rs` - drives the real git hook in a temp repo
  - `tests/e2e_cli.rs` - drives the built binary via `assert_cmd`
  - `tests/hwc_integration.rs` - `SignedClient` against a `wiremock` server
- `tests/live_flavors.rs` is `#[ignore]` - real HWC call:
  `QECS_AK=.. QECS_SK=.. QECS_PROJECT_ID=.. QECS_REGION=ap-southeast-3 cargo test --test live_flavors -- --ignored --nocapture`
- `cargo bench` - criterion micro-benchmarks in `benches/micro.rs` (signer, config, presets, state)
- `cargo run --release --example workflow_timing` (with `QECS_*` env) - phase-by-phase timing of
  the real list-flavors workflow; shows the network dominates and local work is sub-millisecond
- CI (`.github/workflows/ci.yml`): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --all`, `cargo bench --no-run`. Keep clippy clean at `-D warnings`.
  `.github/workflows/release.yml` builds `x86_64-unknown-linux-gnu` and attaches the binary on `v*` tags.
