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

## License

MIT.
