//! Exercises `.githooks/post-commit`: it must bump the Cargo version by the
//! right semver level AND fold that bump into the same commit.
use std::process::Command;

fn run(dir: &std::path::Path, cmd: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cmd failed: {cmd}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn parse_version(text: &str) -> String {
    text.lines()
        .find(|l| l.trim_start().starts_with("version = "))
        .unwrap()
        .split('"')
        .nth(1)
        .unwrap()
        .to_string()
}

fn version_on_disk(dir: &std::path::Path) -> String {
    parse_version(&std::fs::read_to_string(dir.join("Cargo.toml")).unwrap())
}

fn version_in_head(dir: &std::path::Path) -> String {
    parse_version(&run(dir, "git show HEAD:Cargo.toml"))
}

fn setup() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    let hook_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks/post-commit");
    run(p, "git init -q");
    run(p, "git config user.email t@t && git config user.name t");
    // Baseline commit BEFORE the hook is active, so bumping starts from 0.1.0.
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname = \"qecs\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    run(p, "git add -A && git commit -q -m 'chore: init'");
    std::fs::create_dir(p.join(".githooks")).unwrap();
    std::fs::copy(&hook_src, p.join(".githooks/post-commit")).unwrap();
    run(
        p,
        "chmod +x .githooks/post-commit && git config core.hooksPath .githooks",
    );
    d
}

fn commit(d: &std::path::Path, msg: &str) {
    std::fs::write(d.join("f.txt"), msg).unwrap();
    run(d, "git add -A");
    run(d, &format!("git commit -q -m {}", shell_quote(msg)));
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[test]
fn feat_bumps_minor_in_same_commit() {
    let d = setup();
    commit(d.path(), "feat: add a thing");
    assert_eq!(version_on_disk(d.path()), "0.2.0");
    assert_eq!(version_in_head(d.path()), "0.2.0");
}

#[test]
fn fix_bumps_patch() {
    let d = setup();
    commit(d.path(), "fix: correct a thing");
    assert_eq!(version_in_head(d.path()), "0.1.1");
}

#[test]
fn breaking_bang_bumps_major() {
    let d = setup();
    commit(d.path(), "feat!: change the interface");
    assert_eq!(version_in_head(d.path()), "1.0.0");
}

#[test]
fn breaking_change_body_bumps_major() {
    let d = setup();
    commit(d.path(), "fix: x\n\nBREAKING CHANGE: config format changed");
    assert_eq!(version_in_head(d.path()), "1.0.0");
}

#[test]
fn unprefixed_bumps_patch() {
    let d = setup();
    commit(d.path(), "random message");
    assert_eq!(version_in_head(d.path()), "0.1.1");
}

#[test]
fn successive_commits_accumulate() {
    let d = setup();
    commit(d.path(), "feat: one"); // 0.1.0 -> 0.2.0
    commit(d.path(), "fix: two"); // 0.2.0 -> 0.2.1
    commit(d.path(), "fix: three"); // 0.2.1 -> 0.2.2
    assert_eq!(version_in_head(d.path()), "0.2.2");
}
