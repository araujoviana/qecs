<p align="center">
  <img src="assets/logo.png" alt="qecs logo" width="140">
</p>

<h1 align="center">qecs</h1>

<p align="center"><strong>Ephemeral Huawei Cloud compute.</strong> Boot a high-spec box, run one job, watch it disappear.</p>

<p align="center">
  <a href="https://github.com/araujoviana/qecs/actions/workflows/ci.yml"><img src="https://github.com/araujoviana/qecs/actions/workflows/ci.yml/badge.svg" alt="CI status"></a>
  <img src="https://img.shields.io/badge/platform-linux--x86__64-4c566a" alt="Platform: Linux x86_64">
  <img src="https://img.shields.io/badge/rust-edition%202024-b5622b" alt="Rust edition 2024">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-2f6f5e" alt="License: MIT"></a>
</p>

`qecs` packages your working directory, boots an on-demand Pay-Per-Use VM, runs your workload with live terminal output, pulls your artifacts back, and destroys the machine. No orphaned VMs, no leftover disks, no manual SSH wrestling.

## Why

A GPU box left running bills you while it's idle and drifts state between jobs. `qecs` treats a VM like a build artifact instead of a pet: provisioned for one job, destroyed the moment it exits, billed only for the minutes it actually ran. Every VM also carries a hard TTL baked into its own cloud-init, so a crashed laptop or a dropped connection can't leave a meter running unattended.

## Quick Start

```bash
# 1. Install & set credentials
cargo install --path .
qecs setup

# 2. Run your local project in the cloud (auto-detects Python/uv, Rust, Node, Docker)
qecs run .

# 3. Pass arguments, inject environment variables, or run ad-hoc commands
qecs run . -e MODEL=llama -- --batch-size 32
qecs run . -c "pytest tests/ -v"

# 4. Spin up a GPU instance and grab training checkpoints
qecs run --preset gpu -a "checkpoints/*.pt" .
```

## Presets

Every preset spins up an on-demand instance with high network bandwidth and an automated TTL self-destruct guard:

| Preset | Default Flavor | Specs | Storage |
|---|---|---|---|
| `normal` | `s7n.2xlarge.2` | 8 vCPU / 16 GB | 100 GB GPSSD |
| `ram` | `m7.4xlarge.8` | 16 vCPU / 128 GB | 100 GB GPSSD |
| `compute` | `c7.8xlarge.2` | 32 vCPU / 64 GB | 100 GB GPSSD |
| `gpu` | `pi2.4xlarge.4` | 16 vCPU / 64 GB / 2x T4 | 200 GB GPSSD |
| `beefy` | `p2s.8xlarge.8` | 32 vCPU / 256 GB / 4x V100 | 300 GB GPSSD |

Override with `--flavor <FLAVOR_ID>` or customize defaults in `~/.config/qecs/config.toml`.

## Core Features

### Automatic Web Tunnels
If your script starts a local server (Gradio on `7860`, Streamlit on `8501`, Jupyter on `8888`, FastAPI on `8000`, TensorBoard on `6006`), `qecs` detects the open port and forwards it over encrypted SSH directly to your browser:

```text
➜ Web UI detected (Gradio): http://localhost:7860
  Forwarded from remote port 7860 via encrypted SSH tunnel
```

### Session Resilience & Detach
All interactive runs execute inside a background multiplexer session. If your Wi-Fi hiccups or laptop sleeps, the remote job keeps running:

- Press `Ctrl+B d` during a run to detach and return to your local terminal.
- Run `qecs attach` to re-enter the session at any time.

### Dependency Caching
Dependencies (pip/uv wheels, cargo crates) cache to a regional OBS bucket over free internal VPC bandwidth. Subsequent runs boot in seconds instead of redownloading gigabytes of wheels:

```bash
qecs cache ls                  # view cached dependency archives
qecs cache clean               # delete cache objects
qecs cache destroy --force     # purge cache bucket completely
```
Use `--no-cache` on `qecs run` to skip remote cache lookups.

### Diagnostics on Failure
If a job exits abnormally, `qecs` inspects kernel logs and process exit status before tearing the machine down:
- Distinguishes Linux OOM killer (exit 137) from user timeouts.
- Catches missing `.so` libraries and suggests the matching `apt` package.
- Use `--keep-on-failure` to pause VM destruction so you can inspect the state.

## CLI Reference

### Workload Execution
- `qecs run [PATH]`: sync directory, provision VM, execute, retrieve artifacts, destroy.
  - `-p, --preset <NAME>`: hardware profile (`normal`, `ram`, `compute`, `gpu`, `beefy`).
  - `-c, --command <CMD>`: override detector ladder with a custom command.
  - `-e, --env <KEY=VAL>`: pass environment variables to the remote process.
  - `--env-file <PATH>`: load variables from a file.
  - `-a, --artifacts <GLOBS>`: comma-separated output patterns to download locally.
  - `-d, --detach`: start job in background and exit immediately.
  - `-k, --keep`: do not terminate the VM after run finishes.
  - `--keep-on-failure`: retain VM only if exit code is non-zero.
  - `-- <ARGS>`: pass trailing arguments directly to your application.
- `qecs attach [TARGET]`: reattach to a running job session.

### Standalone VM Management
- `qecs up`: boot an on-demand VM with hard TTL limit and idle protection.
- `qecs shell [VM_ID]`: open an interactive SSH terminal (or VNC console fallback).
- `qecs ls`: list active VMs, public IPs, and remaining TTL (`-j` for JSON).
- `qecs logs [VM_ID]`: tail execution logs or cloud-init output (`-f` to stream).
- `qecs wait [VM_ID]`: block until a detached background job finishes.
- `qecs kill [VM_ID]` (alias `qecs down`): destroy VM and release public IP. Use `-a, --all` to clean up everything.

### Model Context Protocol (MCP) Server
`qecs` includes a native stdio MCP server so AI coding assistants (Cursor, Claude Desktop, Antigravity) can run cloud tasks autonomously:

```bash
qecs mcp serve
```

Add this to your editor's MCP settings (`claude_desktop_config.json` or `.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "qecs": {
      "command": "qecs",
      "args": ["mcp", "serve"]
    }
  }
}
```

The MCP server exposes tools to run workloads (`qecs_run`), launch machines (`qecs_up`), inspect state (`qecs_ls`, `qecs_info`), stream logs (`qecs_logs`), and terminate resources (`qecs_kill`).

## Configuration

Credentials and defaults live in `~/.config/qecs/config.toml` or environment variables:

| Variable | Description |
|---|---|
| `QECS_AK` | Huawei Cloud Access Key (`HWC_AK` alias supported) |
| `QECS_SK` | Huawei Cloud Secret Key (`HWC_SK` alias supported) |
| `QECS_REGION` | Default region override (e.g. `ap-southeast-3`, `sa-brazil-1`) |
| `QECS_PROFILE` | Profile name to load from `.env.<profile>` |

## Development

```bash
cargo test --all                         # full test suite
cargo clippy --all-targets -- -D warnings # strict linter check
cargo fmt --check                        # style check
```

Commits use Conventional Commits (`feat:`, `fix:`, `docs:`) to drive automatic version bumping via git hooks.

---

## License

[MIT](LICENSE)
