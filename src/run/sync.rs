//! Workdir synchronization over SSH: pipelined streaming tarball upload and artifact retrieval.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Stdio;

use anyhow::Context;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use tar::{Archive, Builder};

/// Transparently decompress a stream whether it is compressed with zstd or gzip.
pub fn decompress_stream<'a, R: Read + 'a>(mut reader: R) -> anyhow::Result<Box<dyn Read + 'a>> {
    let mut magic = [0u8; 4];
    let n = reader.read(&mut magic)?;
    let combined = std::io::Cursor::new(magic[..n].to_vec()).chain(reader);
    if n >= 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        Ok(Box::new(flate2::read::GzDecoder::new(combined)))
    } else {
        let decoder =
            zstd::stream::read::Decoder::new(combined).context("initializing zstd decoder")?;
        Ok(Box::new(decoder))
    }
}

/// Default directory and file patterns ignored when packing the workspace.
pub const DEFAULT_IGNORES: &[&str] = &[
    "!.git",
    "!.git/**",
    "!.qecs",
    "!.qecs/**",
    "!target",
    "!target/**",
    "!node_modules",
    "!node_modules/**",
    "!__pycache__",
    "!__pycache__/**",
    "!.venv",
    "!.venv/**",
    "!venv",
    "!venv/**",
    "!.superpowers",
    "!.superpowers/**",
    "!.env*",
    "!*.pem",
    "!*.key",
    "!id_rsa*",
    "!id_ed25519*",
];

/// Build an `ignore::Walk` iterator configured for workspace packing.
///
/// Respects `.gitignore`, `.qecsignore`, and user global git ignores even without a `.git` dir,
/// while excluding heavy build/cache directories and sensitive secrets.
pub fn build_walker(root: &Path) -> anyhow::Result<ignore::Walk> {
    let mut ob = OverrideBuilder::new(root);
    for pattern in DEFAULT_IGNORES {
        ob.add(pattern)
            .with_context(|| format!("adding default override pattern `{pattern}`"))?;
    }
    let overrides = ob.build().context("building ignore overrides")?;

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false) // Don't blindly hide dotfiles (.cargo, .github, etc.)
        .parents(true) // Respect parent ignore rules
        .git_ignore(true) // Respect .gitignore
        .git_global(true) // Respect user's global gitignore
        .git_exclude(true) // Respect .git/info/exclude
        .require_git(false) // Parse .gitignore even in non-git directories
        .follow_links(false) // Never follow symlinks (security)
        .overrides(overrides);

    // Support custom .qecsignore in gitignore format
    builder.add_custom_ignore_filename(".qecsignore");

    Ok(builder.build())
}

/// Helper to check if a filename matches sensitive secret patterns.
fn is_sensitive_filename(name: &str) -> bool {
    name.starts_with(".env")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
}

/// Pack the given `root` directory into a stream, writing directly to `writer`.
///
/// Traverses using ripgrep's `ignore` crate, compressing on the fly with multi-threaded zstd.
/// Memory usage is constant (buffers only current chunks), eliminating RAM spikes.
pub fn pack_directory_stream<W: Write>(root: &Path, writer: W) -> anyhow::Result<()> {
    let mut encoder =
        zstd::stream::write::Encoder::new(writer, 1).context("initializing zstd encoder")?;
    let _ = encoder.multithread(0);
    {
        let mut tar = Builder::new(&mut encoder);
        tar.follow_symlinks(false);

        let walker = build_walker(root)?;
        for result in walker {
            let entry = match result {
                Ok(e) => e,
                Err(err) => {
                    log::debug!("skipping unreadable entry during pack: {err}");
                    continue;
                }
            };

            // Never follow or include symlinks (prevents symlink-based secret smuggling)
            if entry.path_is_symlink() {
                continue;
            }

            let path = entry.path();
            let rel_path = match path.strip_prefix(root) {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Skip root itself
            if rel_path.as_os_str().is_empty() {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy();
            if is_sensitive_filename(&file_name) {
                continue;
            }

            if let Some(ft) = entry.file_type() {
                if ft.is_file() {
                    let mut file = File::open(path)
                        .with_context(|| format!("opening file `{}`", path.display()))?;
                    tar.append_file(rel_path, &mut file)
                        .with_context(|| format!("adding `{}` to tar", rel_path.display()))?;
                } else if ft.is_dir() {
                    tar.append_dir(rel_path, path)
                        .with_context(|| format!("adding dir `{}` to tar", rel_path.display()))?;
                }
            }
        }

        tar.finish().context("finishing tarball stream")?;
    }

    let mut inner = encoder
        .finish()
        .context("compressing tarball stream with zstd")?;
    inner.flush().context("flushing compressed stream")?;
    Ok(())
}

/// Pack the given `root` directory into an in-memory tarball, respecting ignore patterns.
/// Retained for backward compatibility and tests.
pub fn pack_directory(root: &Path) -> anyhow::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    pack_directory_stream(root, &mut buffer)?;
    Ok(buffer)
}

/// Stream workspace directly to remote VM via SSH stdin without loading entire tarball into RAM.
pub fn stream_workdir(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    root: &Path,
    remote_dir: &str,
) -> anyhow::Result<()> {
    stream_workdir_full(ip, port, key_path, proxy_command, None, root, remote_dir)
}

/// Stream workspace directly to remote VM via SSH stdin with optional ControlMaster socket.
pub fn stream_workdir_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    control_socket: Option<&Path>,
    root: &Path,
    remote_dir: &str,
) -> anyhow::Result<()> {
    let remote_cmd = format!(
        "mkdir -p '{remote_dir}' && (command -v zstd >/dev/null 2>&1 && zstd -dc || tar -xzf -) | tar -xf - -C '{remote_dir}'"
    );

    let mut child = crate::connect::build_ssh_command_full(
        ip,
        port,
        key_path,
        proxy_command,
        None,
        control_socket,
    )
    .arg(&remote_cmd)
    .stdin(Stdio::piped())
    .stdout(Stdio::inherit())
    .stderr(Stdio::inherit())
    .spawn()
    .context("spawning SSH to stream workspace")?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture SSH stdin"))?;

    // Pipe directly with 512KB buffer
    let buf_writer = std::io::BufWriter::with_capacity(512 * 1024, stdin);
    pack_directory_stream(root, buf_writer)?;

    let status = child
        .wait()
        .context("waiting for SSH streaming upload to complete")?;
    if !status.success() {
        anyhow::bail!("SSH workspace streaming upload failed with exit status: {status}");
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

/// Build shell test to verify remote output artifacts exist.
/// Supports a single directory, single file, or comma-separated targets/globs.
pub fn build_remote_artifact_check_cmd(remote_dir: &str, target_spec: &str) -> String {
    let q_remote = format!("'{}'", remote_dir.replace('\'', "'\\''"));
    let targets: Vec<&str> = target_spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if targets.len() == 1 && !targets[0].contains('*') && !targets[0].contains('?') {
        let t = targets[0].replace('\'', "'\\''");
        let remote_clean = remote_dir.replace('\'', "'\\''");
        format!(
            "[ -d '{remote_clean}/{t}' ] && [ \"$(ls -A '{remote_clean}/{t}' 2>/dev/null)\" ] || [ -f '{remote_clean}/{t}' ]"
        )
    } else {
        let targets_joined = targets
            .iter()
            .map(|t| format!("'{}'", t.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "cd {q_remote} && {{ for t in {targets_joined}; do for f in $t; do if [ -e \"$f\" ]; then exit 0; fi; done; done; exit 1; }}"
        )
    }
}

/// Build shell command to stream matching remote output artifacts as a gzipped tarball.
/// Supports a single directory (extracts its contents), single file, or comma-separated targets/globs.
pub fn build_remote_artifact_stream_cmd(remote_dir: &str, target_spec: &str) -> String {
    let q_remote = format!("'{}'", remote_dir.replace('\'', "'\\''"));
    let targets: Vec<&str> = target_spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if targets.len() == 1 && !targets[0].contains('*') && !targets[0].contains('?') {
        let t = targets[0].replace('\'', "'\\''");
        let remote_clean = remote_dir.replace('\'', "'\\''");
        format!(
            "cd {q_remote} && if [ -d '{t}' ]; then tar -czf - -C '{remote_clean}/{t}' .; else tar -czf - -C {q_remote} '{t}'; fi"
        )
    } else {
        let targets_joined = targets
            .iter()
            .map(|t| format!("'{}'", t.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "cd {q_remote} && shopt -s nullglob && FILES=() && for t in {targets_joined}; do for f in $t; do [ -e \"$f\" ] && FILES+=(\"$f\"); done; done && [ ${{#FILES[@]}} -gt 0 ] && tar -czf - \"${{FILES[@]}}\""
        )
    }
}

/// Shell test that the remote output directory exists and is non-empty.
pub fn remote_output_check_cmd(remote_dir: &str, output_subdir: &str) -> String {
    build_remote_artifact_check_cmd(remote_dir, output_subdir)
}

/// Shell command that streams the remote output directory back as a gzipped tarball.
pub fn remote_output_stream_cmd(remote_dir: &str, output_subdir: &str) -> String {
    build_remote_artifact_stream_cmd(remote_dir, output_subdir)
}

/// Download the recipe's output artifacts (relative to `remote_dir`) if any exist.
/// Returns `Ok(true)` if artifacts were found and downloaded, `Ok(false)` if none were generated.
pub fn download_output(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    remote_dir: &str,
    target_spec: &str,
    local_out: &Path,
) -> anyhow::Result<bool> {
    download_output_full(
        ip,
        port,
        key_path,
        proxy_command,
        None,
        remote_dir,
        target_spec,
        local_out,
    )
}

/// Download the recipe's output artifacts with optional ControlMaster socket and atomic staging.
#[allow(clippy::too_many_arguments)]
pub fn download_output_full(
    ip: &str,
    port: u16,
    key_path: &Path,
    proxy_command: Option<&str>,
    control_socket: Option<&Path>,
    remote_dir: &str,
    target_spec: &str,
    local_out: &Path,
) -> anyhow::Result<bool> {
    let check_cmd = build_remote_artifact_check_cmd(remote_dir, target_spec);
    let check_status = crate::connect::build_ssh_command_full(
        ip,
        port,
        key_path,
        proxy_command,
        None,
        control_socket,
    )
    .arg(&check_cmd)
    .status()
    .context("checking remote output artifacts")?;

    if !check_status.success() {
        return Ok(false);
    }

    let remote_stream_cmd = build_remote_artifact_stream_cmd(remote_dir, target_spec);
    let mut child = crate::connect::build_ssh_command_full(
        ip,
        port,
        key_path,
        proxy_command,
        None,
        control_socket,
    )
    .arg(&remote_stream_cmd)
    .stdout(Stdio::piped())
    .stderr(Stdio::inherit())
    .spawn()
    .context("spawning SSH to download output artifacts")?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to capture SSH stdout"))?;

    // Atomic unpack into a temporary directory
    let temp_dir = tempfile::Builder::new()
        .prefix("qecs-dl-")
        .tempdir()
        .context("creating temporary artifact unpack directory")?;

    let decompressor = decompress_stream(stdout)?;
    let mut archive = Archive::new(decompressor);
    archive
        .unpack(temp_dir.path())
        .context("extracting output tarball")?;

    let status = child
        .wait()
        .context("waiting for SSH download to complete")?;
    if !status.success() {
        anyhow::bail!("SSH output download failed with exit status: {status}");
    }

    // Merge artifacts into local_out: recursively merge directories and overwrite matching files
    fs::create_dir_all(local_out).context("creating local output directory")?;
    for entry in fs::read_dir(temp_dir.path())? {
        let entry = entry?;
        let dest = local_out.join(entry.file_name());
        if entry.path().is_dir() {
            copy_recursive(&entry.path(), &dest)?;
        } else {
            if dest.is_dir() {
                let _ = fs::remove_dir_all(&dest);
            }
            fs::copy(entry.path(), &dest)?;
        }
    }

    Ok(true)
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_recursive(&entry.path(), &target)?;
        } else {
            if target.is_dir() {
                let _ = fs::remove_dir_all(&target);
            }
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
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
        let decoder = decompress_stream(&archive_bytes[..]).unwrap();
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
    #[cfg(unix)]
    fn symlinks_are_not_followed_into_the_tarball() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret");
        fs::write(&secret, "SECRET\n").unwrap();

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("real.txt"), "hi\n").unwrap();
        symlink(&secret, root.join("link-to-secret")).unwrap();

        let bytes = pack_directory(root).unwrap();
        let decoder = decompress_stream(&bytes[..]).unwrap();
        let mut archive = Archive::new(decoder);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path().unwrap().to_string_lossy().into_owned())
            .collect();

        assert!(names.iter().any(|n| n == "real.txt"));
        assert!(
            !names.iter().any(|n| n.contains("link-to-secret")),
            "symlink leaked into archive: {names:?}"
        );
    }

    #[test]
    fn packs_directory_with_qecsignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::write(root.join("run.py"), "print('run')\n").unwrap();
        fs::write(root.join("dataset.csv"), "1,2,3\n").unwrap();
        fs::write(root.join(".qecsignore"), "*.csv\n").unwrap();

        let bytes = pack_directory(root).unwrap();
        let decoder = decompress_stream(&bytes[..]).unwrap();
        let mut archive = Archive::new(decoder);
        let entries: Vec<PathBuf> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path().unwrap().to_path_buf()))
            .collect();

        assert!(entries.iter().any(|p| p == Path::new("run.py")));
        assert!(!entries.iter().any(|p| p == Path::new("dataset.csv")));
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

    #[test]
    fn multi_target_artifact_commands() {
        let check = build_remote_artifact_check_cmd("/workspace", "models/*.pt,results.json");
        assert!(check.contains("for t in 'models/*.pt' 'results.json'"));
        assert!(check.contains("cd '/workspace'"));

        let stream = build_remote_artifact_stream_cmd("/workspace", "models/*.pt,results.json");
        assert!(stream.contains("shopt -s nullglob"));
        assert!(stream.contains("for t in 'models/*.pt' 'results.json'"));
    }

    #[test]
    fn packs_empty_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        fs::create_dir_all(root.join("empty_dir")).unwrap();
        fs::write(root.join("file.txt"), "hello").unwrap();

        let bytes = pack_directory(root).unwrap();
        let decoder = decompress_stream(&bytes[..]).unwrap();
        let mut archive = Archive::new(decoder);
        let paths: Vec<PathBuf> = archive
            .entries()
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path().unwrap().to_path_buf()))
            .collect();

        assert!(paths.iter().any(|p| p == Path::new("empty_dir")));
        assert!(paths.iter().any(|p| p == Path::new("file.txt")));
    }

    #[test]
    fn copy_recursive_merges_without_wiping() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();

        // Create pre-existing file in dst/sub
        fs::create_dir_all(dst_dir.path().join("sub")).unwrap();
        fs::write(dst_dir.path().join("sub").join("old.txt"), "keep me").unwrap();

        // Create new file in src/sub
        fs::create_dir_all(src_dir.path().join("sub")).unwrap();
        fs::write(src_dir.path().join("sub").join("new.txt"), "new content").unwrap();

        copy_recursive(src_dir.path(), dst_dir.path()).unwrap();

        // Both old and new files must exist in dst
        assert!(dst_dir.path().join("sub").join("old.txt").exists());
        assert!(dst_dir.path().join("sub").join("new.txt").exists());
    }
}
