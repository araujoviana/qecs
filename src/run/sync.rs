//! Workdir synchronization over SSH: in-memory tarball streaming and artifact retrieval.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use tar::{Archive, Builder};

/// Default directory and file patterns ignored when packing the workspace.
const DEFAULT_IGNORES: &[&str] = &[
    ".git",
    ".qecs",
    "target",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".env",
    ".superpowers",
];

/// Collect ignore patterns from standard list plus `.gitignore` and `.qecsignore`.
fn collect_ignore_patterns(root: &Path) -> Vec<String> {
    let mut patterns: Vec<String> = DEFAULT_IGNORES.iter().map(|s| s.to_string()).collect();

    for ignore_file in &[".gitignore", ".qecsignore"] {
        let path = root.join(ignore_file);
        if let Ok(content) = fs::read_to_string(&path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    // Normalize leading / or trailing /
                    let p = trimmed.trim_start_matches('/').trim_end_matches('/');
                    patterns.push(p.to_string());
                }
            }
        }
    }

    patterns
}

/// Determine whether a given relative path should be excluded.
fn should_exclude(rel_path: &Path, patterns: &[String]) -> bool {
    let rel_str = rel_path.to_string_lossy();

    // Sensitive files like private keys and env files
    if rel_str.starts_with(".env") || rel_str.ends_with(".pem") || rel_str.ends_with(".key") {
        return true;
    }

    for comp in rel_path.components() {
        let comp_str = comp.as_os_str().to_string_lossy();
        for pattern in patterns {
            if comp_str == *pattern
                || rel_str == *pattern
                || rel_str.starts_with(&format!("{pattern}/"))
            {
                return true;
            }
        }
    }

    false
}

/// Pack the given `root` directory into an in-memory gzipped tarball, respecting ignore patterns.
pub fn pack_directory(root: &Path) -> anyhow::Result<Vec<u8>> {
    let patterns = collect_ignore_patterns(root);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    {
        let mut tar = Builder::new(&mut encoder);
        tar.follow_symlinks(false);

        walk_and_pack(&mut tar, root, root, &patterns)?;
        tar.finish().context("finishing tarball")?;
    }

    let compressed = encoder.finish().context("compressing tarball")?;
    Ok(compressed)
}

fn walk_and_pack<W: Write>(
    tar: &mut Builder<W>,
    root: &Path,
    current_dir: &Path,
    patterns: &[String],
) -> anyhow::Result<()> {
    let entries = fs::read_dir(current_dir).context("reading directory")?;

    for entry in entries.flatten() {
        let path = entry.path();
        let rel_path = path.strip_prefix(root).unwrap_or(&path);

        if should_exclude(rel_path, patterns) {
            continue;
        }

        if path.is_dir() {
            walk_and_pack(tar, root, &path, patterns)?;
        } else if path.is_file() || path.is_symlink() {
            let mut file = File::open(&path).context("opening file for tar")?;
            tar.append_file(rel_path, &mut file)
                .with_context(|| format!("adding `{}` to tar", rel_path.display()))?;
        }
    }

    Ok(())
}

/// Upload in-memory workspace tarball directly to remote VM via SSH stdin stream.
pub fn upload_workdir(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    archive: &[u8],
    remote_dir: &str,
) -> anyhow::Result<()> {
    let remote_cmd = format!("mkdir -p '{remote_dir}' && tar -xzf - -C '{remote_dir}'");

    let mut child = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&remote_cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawning SSH to upload workspace")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(archive)
            .context("streaming tarball to SSH")?;
    }

    let status = child.wait().context("waiting for SSH upload to complete")?;
    if !status.success() {
        anyhow::bail!("SSH workspace upload failed with exit status: {status}");
    }

    Ok(())
}

/// Shell test that the remote output directory exists and is non-empty.
fn remote_output_check_cmd(remote_dir: &str, output_subdir: &str) -> String {
    let dir = format!("{remote_dir}/{output_subdir}");
    format!("[ -d '{dir}' ] && [ \"$(ls -A '{dir}' 2>/dev/null)\" ]")
}

/// Shell command that streams the remote output directory back as a gzipped tarball.
fn remote_output_stream_cmd(remote_dir: &str, output_subdir: &str) -> String {
    let dir = format!("{remote_dir}/{output_subdir}");
    format!("tar -czf - -C '{dir}' .")
}

/// Download the recipe's output directory (relative to `remote_dir`) if it exists.
/// Returns `Ok(true)` if artifacts were found and downloaded, `Ok(false)` if none were generated.
pub fn download_output(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    remote_dir: &str,
    output_subdir: &str,
    local_out: &Path,
) -> anyhow::Result<bool> {
    // Check if remote output dir exists and contains files
    let check_cmd = remote_output_check_cmd(remote_dir, output_subdir);
    let check_status = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&check_cmd)
        .status()
        .context("checking remote output directory")?;

    if !check_status.success() {
        return Ok(false);
    }

    // Stream tarball back
    let remote_stream_cmd = remote_output_stream_cmd(remote_dir, output_subdir);
    let mut child = crate::connect::build_ssh_command(ip, port, key_path, proxy_command)
        .arg(&remote_stream_cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawning SSH to download output artifacts")?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture SSH stdout"))?;

    fs::create_dir_all(local_out).context("creating local output directory")?;
    let decoder = GzDecoder::new(stdout);
    let mut archive = Archive::new(decoder);
    archive
        .unpack(local_out)
        .context("extracting output tarball locally")?;

    let status = child
        .wait()
        .context("waiting for SSH download to complete")?;
    if !status.success() {
        anyhow::bail!("SSH output download failed with exit status: {status}");
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn packs_directory_excluding_git_and_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create standard files
        fs::write(root.join("main.py"), "print('hello')\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.py"), "def foo(): pass\n").unwrap();

        // Create ignored files
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "secret\n").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/debug"), "bin\n").unwrap();
        fs::write(root.join(".env"), "SECRET_KEY=123\n").unwrap();
        fs::write(root.join("id_rsa.key"), "private-key\n").unwrap();

        // Create custom .gitignore
        fs::write(root.join(".gitignore"), "custom_ignore.txt\n").unwrap();
        fs::write(root.join("custom_ignore.txt"), "skip me\n").unwrap();

        let archive_bytes = pack_directory(root).unwrap();
        assert!(!archive_bytes.is_empty());

        // Decode and verify contents
        let decoder = GzDecoder::new(&archive_bytes[..]);
        let mut archive = Archive::new(decoder);
        let entries: Vec<PathBuf> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path().unwrap().to_path_buf()))
            .collect();

        assert!(entries.iter().any(|p| p == Path::new("main.py")));
        assert!(entries.iter().any(|p| p == Path::new("src/lib.py")));

        // Verify exclusions
        assert!(!entries.iter().any(|p| p.starts_with(".git")));
        assert!(!entries.iter().any(|p| p.starts_with("target")));
        assert!(!entries.iter().any(|p| p == Path::new(".env")));
        assert!(!entries.iter().any(|p| p == Path::new("id_rsa.key")));
        assert!(!entries.iter().any(|p| p == Path::new("custom_ignore.txt")));
    }

    #[test]
    fn output_commands_target_the_recipe_output_subdir_not_a_hardcoded_out() {
        let check = remote_output_check_cmd("/home/ubuntu/workspace", "results");
        assert!(check.contains("/home/ubuntu/workspace/results"));
        assert!(!check.contains("workspace/out"));

        let stream = remote_output_stream_cmd("/home/ubuntu/workspace", "results");
        assert!(stream.contains("-C '/home/ubuntu/workspace/results'"));
    }

    #[test]
    fn output_commands_default_subdir_is_out() {
        let check = remote_output_check_cmd("/home/ubuntu/workspace", "out");
        assert!(check.contains("/home/ubuntu/workspace/out"));
    }
}
