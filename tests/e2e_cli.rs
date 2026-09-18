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
fn wait_without_active_vms_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    qecs()
        .args(["wait", "job-1"])
        .env("XDG_STATE_HOME", dir.path())
        .env("QECS_AK", "mock")
        .env("QECS_SK", "mock")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn run_dry_run_displays_summary() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hello')\n").unwrap();

    qecs()
        .args(["run", dir.path().to_str().unwrap(), "--dry-run"])
        .env("QECS_AK", "mock")
        .env("QECS_SK", "mock")
        .assert()
        .success()
        .stdout(predicate::str::contains("=== qecs run Dry-Run Plan ==="))
        .stdout(predicate::str::contains("python-bare"))
        .stdout(predicate::str::contains("python3 main.py"));
}

#[test]
fn shell_without_active_vms_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    qecs()
        .arg("shell")
        .env("XDG_STATE_HOME", dir.path())
        .env("QECS_AK", "mock")
        .env("QECS_SK", "mock")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("no active VMs found"));
}

#[test]
fn ls_empty_succeeds_and_displays_no_vms_tracked() {
    let dir = tempfile::tempdir().unwrap();
    qecs()
        .arg("ls")
        .env("XDG_STATE_HOME", dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("No active VMs tracked"));
}

#[test]
fn ls_json_returns_empty_array_when_no_vms() {
    let dir = tempfile::tempdir().unwrap();
    qecs()
        .args(["ls", "--json"])
        .env("XDG_STATE_HOME", dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("[]"));
}

#[test]
fn kill_without_target_when_no_vms_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    qecs()
        .arg("kill")
        .env("XDG_STATE_HOME", dir.path())
        .env("QECS_AK", "mock")
        .env("QECS_SK", "mock")
        .assert()
        .failure();
}

#[test]
fn kill_with_vm_id_resolves_from_state_store() {
    let dir = tempfile::tempdir().unwrap();
    let qecs_dir = dir.path().join("qecs");
    std::fs::create_dir_all(&qecs_dir).unwrap();
    let vms_json = qecs_dir.join("vms.json");
    let record = serde_json::json!([{
        "id": "70d2205e-577a-4e6a-b1fb-3ff6dab36a40",
        "name": "qecs-gpu-9df3",
        "preset": "gpu",
        "flavor": "pi2.4xlarge.4",
        "region": "ap-southeast-3",
        "az": "ap-southeast-3a",
        "eip": "1.2.3.4",
        "private_ip": "192.168.0.10",
        "created_at": "2026-09-18T00:00:00Z",
        "ttl_secs": 7200,
        "tags": []
    }]);
    std::fs::write(&vms_json, serde_json::to_string(&record).unwrap()).unwrap();

    let output = qecs()
        .args(["kill", "70d2205e-577a-4e6a-b1fb-3ff6dab36a40"])
        .env("XDG_STATE_HOME", dir.path())
        .env("QECS_AK", "mock")
        .env("QECS_SK", "mock")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("not found in state or cloud"),
        "Should have resolved the VM by ID, but got: {stderr}"
    );
}

#[test]
fn logs_without_active_vms_reports_error() {
    let dir = tempfile::tempdir().unwrap();
    qecs()
        .args(["logs", "qecs-nonexistent-999"])
        .env("XDG_STATE_HOME", dir.path())
        .env("QECS_AK", "mock")
        .env("QECS_SK", "mock")
        .assert()
        .failure()
        .code(1);
}

#[test]
fn image_help_exits_zero() {
    qecs()
        .args(["image", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Manage and build pre-baked IMS images",
        ));
}

#[test]
fn image_delete_without_id_is_usage_error() {
    qecs().args(["image", "delete"]).assert().failure().code(2);
}

#[test]
fn presets_telemetry_writes_one_trace_file() {
    let tmp = tempfile::tempdir().unwrap();
    qecs()
        .args(["presets", "--telemetry"])
        .env("HOME", tmp.path())
        .env_remove("XDG_STATE_HOME")
        .env_remove("QECS_TELEMETRY")
        .assert()
        .success();

    let traces_dir = tmp.path().join(".local/state/qecs/traces");
    assert!(traces_dir.exists(), "traces dir should exist");
    let entries: Vec<_> = std::fs::read_dir(&traces_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    assert_eq!(entries.len(), 1, "exactly one trace file");

    let content = std::fs::read_to_string(&entries[0]).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert!(!lines.is_empty(), "trace has lines");
    for line in &lines {
        let _: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
    }

    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(last["kind"], "run");
    assert_eq!(last["subcommand"], "presets");
    assert_eq!(last["exit_code"], 0);
}

#[test]
fn presets_without_telemetry_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    qecs()
        .args(["presets"])
        .env("HOME", tmp.path())
        .env_remove("XDG_STATE_HOME")
        .env_remove("QECS_TELEMETRY")
        .assert()
        .success();

    let traces_dir = tmp.path().join(".local/state/qecs/traces");
    assert!(
        !traces_dir.exists(),
        "traces dir should not exist without telemetry"
    );
}

#[test]
fn completion_generates_shell_script() {
    let assert = qecs().args(["completion", "fish"]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("complete -c qecs"));
    assert!(stdout.contains("run"));
    assert!(stdout.contains("presets"));
    assert!(stdout.contains("__fish_qecs_active_vms"));
    assert!(stdout.contains("down"));
    assert!(stdout.contains("normal"));
    assert!(stdout.contains("gpu"));
    assert!(stdout.contains("bash elvish fish powershell zsh"));
}

#[test]
fn completion_generates_down_for_all_shells() {
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let assert = qecs().args(["completion", shell]).assert().success();
        let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
        assert!(
            stdout.contains("down"),
            "shell {shell} completion must include `down` command"
        );
    }
}

#[test]
fn run_help_shows_new_ergonomic_flags() {
    qecs().args(["run", "--help"]).assert().success().stdout(
        predicate::str::contains("--command")
            .and(predicate::str::contains("--env"))
            .and(predicate::str::contains("--env-file"))
            .and(predicate::str::contains("--keep-on-failure"))
            .and(predicate::str::contains("--artifacts"))
            .and(predicate::str::contains("--pty")),
    );
}

#[test]
fn run_dry_run_with_command_and_trailing_args() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hi')\n").unwrap();

    qecs()
        .current_dir(dir.path())
        .args(["run", "--dry-run", "-c", "pytest", "--", "-v", "-s"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("=== qecs run Dry-Run Plan ===")
                .and(predicate::str::contains("Run command:    pytest -v -s")),
        );
}

#[test]
fn run_dry_run_with_env_masks_secrets() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hi')\n").unwrap();

    qecs()
        .current_dir(dir.path())
        .args(["run", "--dry-run", "-e", "HF_TOKEN=hf_abcdef123456"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Environment variables:")
                .and(predicate::str::contains("HF_TOKEN=hf_...456")),
        );
}

#[test]
fn run_dry_run_with_artifacts_flag() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.py"), "print('hi')\n").unwrap();

    qecs()
        .current_dir(dir.path())
        .args(["run", "--dry-run", "-a", "models/*.pt,results.json"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("=== qecs run Dry-Run Plan ===").and(
                predicate::str::contains("Artifacts:      models/*.pt,results.json"),
            ),
        );
}

#[test]
fn run_help_includes_no_cache() {
    qecs()
        .args(["run", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--no-cache"));
}

#[test]
fn cache_help_exits_zero_and_shows_subcommands() {
    qecs().args(["cache", "--help"]).assert().success().stdout(
        predicate::str::contains("ls")
            .and(predicate::str::contains("clean"))
            .and(predicate::str::contains("destroy")),
    );
}

#[test]
fn cache_clean_help_shows_force_flag() {
    qecs()
        .args(["cache", "clean", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"));
}

#[test]
fn cache_destroy_help_shows_force_flag() {
    qecs()
        .args(["cache", "destroy", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"));
}

#[test]
fn attach_help_exits_zero() {
    qecs()
        .args(["attach", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Attach to an interactive session"));
}

#[test]
fn attach_without_active_vms_reports_error() {
    let td = tempfile::tempdir().unwrap();
    qecs()
        .env("XDG_STATE_HOME", td.path())
        .args(["attach"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no active VMs found"));
}

#[test]
fn mcp_help_exits_zero() {
    qecs()
        .args(["mcp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("serve"));
}

#[test]
fn mcp_serve_help_exits_zero() {
    qecs()
        .args(["mcp", "serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Start the MCP JSON-RPC 2.0 stdio server",
        ));
}

#[test]
fn mcp_serve_stdio_initialize_and_tools_list() {
    let request_init = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n";
    let request_tools = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n";
    let input = format!("{request_init}{request_tools}");

    let out = qecs()
        .args(["mcp", "serve"])
        .write_stdin(input)
        .output()
        .unwrap();

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines.len(), 2, "Expected 2 responses for 2 requests");

    // Line 1: Initialize result
    let init_resp: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(init_resp["jsonrpc"], "2.0");
    assert_eq!(init_resp["id"], 1);
    assert_eq!(init_resp["result"]["serverInfo"]["name"], "qecs");
    assert_eq!(init_resp["result"]["protocolVersion"], "2024-11-05");

    // Line 2: Tools list result
    let tools_resp: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(tools_resp["jsonrpc"], "2.0");
    assert_eq!(tools_resp["id"], 2);
    let tools = tools_resp["result"]["tools"]
        .as_array()
        .expect("tools array");
    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(tool_names.contains(&"qecs_run"));
    assert!(tool_names.contains(&"qecs_up"));
    assert!(tool_names.contains(&"qecs_ls"));
    assert!(tool_names.contains(&"qecs_info"));
    assert!(tool_names.contains(&"qecs_logs"));
    assert!(tool_names.contains(&"qecs_wait"));
    assert!(tool_names.contains(&"qecs_kill"));
    assert!(tool_names.contains(&"qecs_presets"));
    assert!(tool_names.contains(&"qecs_cache_clean"));
}
