# qecs

Provision a strong, ephemeral Huawei Cloud (HWC) VM, run one job on it, and throw it away before it costs money.

## Workflows

qecs supports two primary workflows:

- **Abstracted (`qecs run`)**: Package your local directory, ship it to a fresh VM, auto-detect the project stack (Rust Cargo, Python uv/pip, Docker, npm/pnpm, bash script), stream the remote execution logs, retrieve `./out` artifacts back locally, and automatically destroy the VM.
- **Interactive (`qecs up` / `qecs shell`)**: Bring up an on-demand VM with hard TTL and idle-shutdown protection, shell in, and destroy it when finished.

## Installation

```bash
cargo install --path .
bash scripts/install-hooks.sh   # contributors only: enables version bump hook
```

## Quick Start

```bash
# 1. Configure credentials and preferred region
qecs setup

# 2. View available hardware presets
qecs presets

# 3. Ship and run the current directory on an ephemeral machine
qecs run .

# Or run with GPU preset and detached execution
qecs run --preset gpu --detach .
qecs logs
qecs wait
```

## Presets

Ephemeral VMs default to capable hardware shapes:

| Preset | Default Flavor | Specs | Disk (GPSSD) |
|---|---|---|---|
| `normal` | `s7n.2xlarge.2` | 8 vCPU / 16 GB | 100 GB |
| `ram` | `m7.4xlarge.8` | 16 vCPU / 128 GB | 100 GB |
| `compute` | `c7.8xlarge.2` | 32 vCPU / 64 GB | 100 GB |
| `gpu` | `pi2.4xlarge.4` | 16 vCPU / 64 GB / 2x T4 | 200 GB |
| `beefy` | `p2s.8xlarge.8` | 32 vCPU / 256 GB / 4x V100 | 300 GB |

Presets and flavor mappings can be customized in `~/.config/qecs/config.toml`. Ad-hoc overrides can be passed via `--flavor <name>`.

## CLI Commands

### Execution & Provisioning
- `qecs run [PATH]`: package workdir, provision VM, run job, download artifacts, destroy.
  - `--preset <NAME>`: choose hardware preset (`normal`, `ram`, `compute`, `gpu`, `beefy`).
  - `--flavor <FLAVOR>`: explicit Huawei Cloud flavor ID override.
  - `--ttl <DURATION>`: execution time limit before guard terminates the machine (default: `1h`).
  - `--detach`: submit job in background and exit immediately.
  - `--keep`: keep the VM alive after job execution completes.
  - `--output <PATH>`: custom destination directory for remote `./out` artifacts.
  - `--dry-run`: display resolved detector ladder recipe and configuration without provisioning.
  - `--telemetry`: emit execution phase and network timing trace.
- `qecs up`: provision an interactive ephemeral VM and register in local state.
  - Options: `--preset`, `--name`, `--ttl`, `--dry-run`, `--telemetry`.
- `qecs shell [VM_ID]`: open an SSH shell (or remote VNC console fallback) into an active VM.

### Inspection & Management
- `qecs ls`: list tracked VMs with status, IPs, and remaining TTL. Add `--json` for machine-readable output.
- `qecs info [VM_ID]`: display full metadata for a VM including VPC, subnet, AZ, and console URL.
- `qecs logs [VM_ID]`: stream cloud-init initialization logs or background job output (`--follow`).
- `qecs wait [VM_ID]`: block until a detached background job finishes and optionally retrieve output.
- `qecs kill [VM_ID]` (alias: `qecs down`): delete an active VM and release cloud resources. Use `--all` to terminate all tracked VMs.
- `qecs gc`: synchronize state with the cloud, detect externally deleted VMs, and purge expired instances.

### Setup & Shell Integration
- `qecs setup`: interactive credential resolution check and config generation.
- `qecs presets`: display available presets and active region.
- `qecs image build`: bake a private IMS image with preinstalled toolchains and GPU drivers for accelerated cold starts.
- `qecs completion <shell>`: generate shell tab-completion scripts (`bash`, `zsh`, `fish`, `powershell`, `elvish`).

## Configuration & Environment

Configuration is stored in `~/.config/qecs/config.toml`. Key settings include default region, presets, volume types, and SSH preferences.

### Environment Variables

All qecs settings follow the `QECS_<NAME>` convention:

| Variable | Fallback Alias | Description |
|---|---|---|
| `QECS_AK` | `HUAWEICLOUD_SDK_AK`, `HWC_AK` | Access key |
| `QECS_SK` | `HUAWEICLOUD_SDK_SK`, `HWC_SK` | Secret key |
| `QECS_SECURITY_TOKEN` | `HWC_SECURITY_TOKEN` | STS session token |
| `QECS_REGION` | - | Region override (precedence below `--region`, above config) |
| `QECS_PROFILE` | - | Configuration profile selector |
| `QECS_TELEMETRY` | - | Set to `1` to enable JSONL telemetry traces |

### Telemetry Tracing

When running with `--telemetry` (or `QECS_TELEMETRY=1`), qecs writes structured JSONL traces to `~/.local/state/qecs/traces/`. Traces record:
- Total workflow wall time and metadata (preset, flavor, region, credential source).
- Millisecond-accurate durations for each provisioning and execution phase.
- Per-call HTTP latency, status code, request ID, and payload sizes for Huawei Cloud APIs.
- Poll loop iteration counts and probe timings.

## Development

```bash
# Run unit, mock integration, signer vectors, and CLI e2e tests
cargo test --all

# Run linter and formatter check
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# Compile benchmarks
cargo bench --no-run
```

Commits must follow Conventional Commits (`feat:`, `fix:`, `docs:`, etc.). The repository post-commit hook automatically manages version bumps in `Cargo.toml`.

## License

MIT.
