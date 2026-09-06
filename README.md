# qecs

Provision a strong, ephemeral Huawei Cloud VM, run one job on it, and throw it
away before it costs money.

## Status

Early. The command surface exists and Huawei Cloud authentication works.
Provisioning lands next.

## Install

    cargo install --path .
    bash scripts/install-hooks.sh   # contributors only

## Usage

    qecs presets     # show presets and their flavors
    qecs setup       # write config, check credentials

More commands arrive with the provisioning branch.

## Development

    bash scripts/install-hooks.sh   # once: enables the auto version-bump hook
    cargo test --all               # unit + e2e (built binary) + integration (mock HWC) + hook tests
    cargo bench                     # criterion micro-benchmarks (signer, config, state)

`cargo run --release --example workflow_timing` (with `QECS_AK`, `QECS_SK`,
`QECS_PROJECT_ID`, `QECS_REGION` set) times the real list-flavors call phase by
phase. Every local phase combined is well under a millisecond; the network is
essentially all of the wall time.

Commits must follow Conventional Commits: the `post-commit` hook bumps the
`Cargo.toml` version from the type (`feat:` minor, `feat!:`/`BREAKING CHANGE`
major, everything else patch).

## License

MIT.
