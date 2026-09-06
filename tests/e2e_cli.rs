//! End-to-end: drive the built `qecs` binary as a user would.
use assert_cmd::Command;
use predicates::prelude::*;

fn qecs() -> Command {
    Command::cargo_bin("qecs").unwrap()
}

#[test]
fn help_exits_zero_and_describes_the_tool() {
    qecs()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Ephemeral Huawei Cloud compute"));
}

#[test]
fn version_matches_cargo_pkg_version() {
    qecs()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn presets_prints_every_preset_and_the_region_line() {
    qecs()
        .args(["presets"])
        .env("XDG_CONFIG_HOME", "/nonexistent-qecs-e2e")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("normal")
                .and(predicate::str::contains("ram"))
                .and(predicate::str::contains("compute"))
                .and(predicate::str::contains("gpu"))
                .and(predicate::str::contains("beefy"))
                .and(predicate::str::contains("pi2.4xlarge.4"))
                .and(predicate::str::contains("region: ap-southeast-3")),
        );
}

#[test]
fn presets_json_is_a_five_element_array_with_expected_keys() {
    let out = qecs()
        .args(["presets", "--json"])
        .env("XDG_CONFIG_HOME", "/nonexistent-qecs-e2e")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let arr = v.as_array().expect("top-level array");
    assert_eq!(arr.len(), 5);
    for item in arr {
        assert!(item.get("preset").is_some());
        assert!(item.get("flavor").is_some());
        assert!(item.get("disk_gb").is_some());
        assert!(item.get("needs_gpu").is_some());
    }
}

#[test]
fn region_flag_overrides_the_default() {
    qecs()
        .args(["--region", "sa-brazil-1", "presets"])
        .env("XDG_CONFIG_HOME", "/nonexistent-qecs-e2e")
        .assert()
        .success()
        .stdout(predicate::str::contains("region: sa-brazil-1"));
}

#[test]
fn config_file_flavor_override_is_reflected() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join("qecs");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("config.toml"),
        "[presets.gpu]\nflavor = \"pi2.8xlarge.4\"\n",
    )
    .unwrap();
    qecs()
        .args(["presets"])
        .env("XDG_CONFIG_HOME", dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("pi2.8xlarge.4"));
}

#[test]
fn unimplemented_subcommand_fails_with_a_clear_message() {
    qecs()
        .arg("ls")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("not implemented yet"));
}

#[test]
fn kill_without_target_is_a_usage_error() {
    qecs().arg("kill").assert().failure().code(2);
}
