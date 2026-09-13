//! Deterministic detector ladder for `qecs run`.
//!
//! Heuristically inspects a target directory to determine the build and execution recipe.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::presets::Preset;

/// A resolved build and execution recipe for an ephemeral cloud run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecipe {
    /// Identifier for the detected environment (e.g. "docker", "python-uv", "rust").
    pub name: String,
    /// Setup commands to execute in the remote workspace before the run command.
    pub setup_commands: Vec<String>,
    /// The primary command to execute.
    pub run_command: String,
    /// The suggested machine preset (defaults to Normal, or Gpu if GPU libraries/containers detected).
    pub preset: Preset,
    /// The root directory containing the project.
    pub workdir: PathBuf,
    /// Relative path inside the workspace from which to pull output artifacts back (defaults to "out").
    pub output_dir: String,
}

/// Optional manifest file (`qecs.toml`) in the workspace to override parts of the recipe.
#[derive(Debug, Clone, Deserialize, Default)]
struct ManifestFile {
    qecs: Option<ManifestConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ManifestConfig {
    preset: Option<String>,
    setup: Option<Vec<String>>,
    run: Option<String>,
    output_dir: Option<String>,
    entrypoint: Option<String>,
}

/// Detect a run recipe for the given path.
///
/// If `path` is a file, the file's parent directory becomes the `workdir` and the file becomes
/// the explicit entrypoint.
/// If `path` is a directory, the directory is inspected according to the detector ladder.
pub fn detect_recipe(path: &Path) -> anyhow::Result<RunRecipe> {
    let (workdir, explicit_entry) = if path.is_file() {
        let parent = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("main.py")
            .to_string();
        (parent, Some(filename))
    } else {
        (path.to_path_buf(), None)
    };

    let mut recipe = run_detector_ladder(&workdir, explicit_entry.as_deref())?;

    // Apply qecs.toml overrides if present
    let manifest_path = workdir.join("qecs.toml");
    if manifest_path.is_file()
        && let Ok(content) = fs::read_to_string(&manifest_path)
        && let Ok(manifest) = toml::from_str::<ManifestFile>(&content)
        && let Some(cfg) = manifest.qecs
    {
        if let Some(p) = cfg.preset
            && let Ok(parsed_preset) = p.parse::<Preset>()
        {
            recipe.preset = parsed_preset;
        }
        if let Some(setup) = cfg.setup {
            recipe.setup_commands = setup;
        }
        if let Some(run) = cfg.run {
            recipe.run_command = run;
        }
        if let Some(out) = cfg.output_dir {
            recipe.output_dir = out;
        }
    }

    Ok(recipe)
}

fn run_detector_ladder(workdir: &Path, explicit_entry: Option<&str>) -> anyhow::Result<RunRecipe> {
    let output_dir = "out".to_string();

    // 1. Dockerfile / compose.yaml / compose.yml
    if workdir.join("compose.yaml").is_file() || workdir.join("compose.yml").is_file() {
        let file = if workdir.join("compose.yaml").is_file() {
            "compose.yaml"
        } else {
            "compose.yml"
        };
        return Ok(RunRecipe {
            name: "docker-compose".into(),
            setup_commands: vec![],
            run_command: format!("docker compose -f {file} up --abort-on-container-exit"),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    if workdir.join("Dockerfile").is_file() {
        return Ok(RunRecipe {
            name: "docker".into(),
            setup_commands: vec!["docker build -t qecs-job .".into()],
            run_command: "docker run --rm -v $(pwd):/workspace -w /workspace qecs-job".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 2. Python
    let pyproject = workdir.join("pyproject.toml");
    let requirements = workdir.join("requirements.txt");
    let pipfile = workdir.join("Pipfile");
    let conda_env = workdir.join("environment.yml");

    let has_py_files = contains_extension(workdir, "py");
    let needs_gpu = scan_python_gpu_need(workdir);
    let default_preset = if needs_gpu {
        Preset::Gpu
    } else {
        Preset::Normal
    };

    if pyproject.is_file() {
        let content = fs::read_to_string(&pyproject).unwrap_or_default();
        let entry = resolve_python_entry(workdir, explicit_entry);

        if workdir.join("uv.lock").is_file() || content.contains("[tool.uv]") {
            let mut setup_commands = Vec::new();
            let sys_pkgs = scan_required_system_packages(workdir);
            if !sys_pkgs.is_empty() {
                setup_commands.push(format!(
                    "sudo apt-get update -qq && sudo apt-get install -y --no-install-recommends {}",
                    sys_pkgs.join(" ")
                ));
            }
            setup_commands.push(
                "which uv >/dev/null 2>&1 || curl -LsSf https://astral.sh/uv/install.sh | sh"
                    .into(),
            );
            setup_commands.push("export PATH=\"$HOME/.local/bin:$PATH\"".into());
            setup_commands.push("uv sync".into());

            return Ok(RunRecipe {
                name: "python-uv".into(),
                setup_commands,
                run_command: format!("uv run {entry}"),
                preset: default_preset,
                workdir: workdir.to_path_buf(),
                output_dir,
            });
        }

        if workdir.join("poetry.lock").is_file() || content.contains("[tool.poetry]") {
            return Ok(RunRecipe {
                name: "python-poetry".into(),
                setup_commands: vec![
                    "pip install --upgrade poetry".into(),
                    "poetry install".into(),
                ],
                run_command: format!("poetry run python {entry}"),
                preset: default_preset,
                workdir: workdir.to_path_buf(),
                output_dir,
            });
        }
    }

    if pipfile.is_file() {
        let entry = resolve_python_entry(workdir, explicit_entry);
        return Ok(RunRecipe {
            name: "python-pipenv".into(),
            setup_commands: vec![
                "pip install --upgrade pipenv".into(),
                "pipenv install".into(),
            ],
            run_command: format!("pipenv run python {entry}"),
            preset: default_preset,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    if conda_env.is_file() {
        let entry = resolve_python_entry(workdir, explicit_entry);
        return Ok(RunRecipe {
            name: "python-conda".into(),
            setup_commands: vec![
                "which micromamba >/dev/null 2>&1 || (curl -Ls https://micro.mamba.pm/api/micromamba/linux-64/latest | tar -xj -C /usr/local/bin --strip-components=1 bin/micromamba)".into(),
                "micromamba create -y -f environment.yml -n qecs-env".into(),
            ],
            run_command: format!("micromamba run -n qecs-env python {entry}"),
            preset: default_preset,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    if requirements.is_file() {
        let entry = resolve_python_entry(workdir, explicit_entry);
        let mut setup_commands = Vec::new();
        let sys_pkgs = scan_required_system_packages(workdir);
        if !sys_pkgs.is_empty() {
            setup_commands.push(format!(
                "sudo apt-get update -qq && sudo apt-get install -y --no-install-recommends {}",
                sys_pkgs.join(" ")
            ));
        }
        setup_commands.push(
            "which uv >/dev/null 2>&1 || curl -LsSf https://astral.sh/uv/install.sh | sh".into(),
        );
        setup_commands.push("export PATH=\"$HOME/.local/bin:$PATH\"".into());
        setup_commands.push("uv venv .venv".into());
        setup_commands.push(". .venv/bin/activate && uv pip install -r requirements.txt".into());

        return Ok(RunRecipe {
            name: "python-pip".into(),
            setup_commands,
            run_command: format!(". .venv/bin/activate && python3 {entry}"),
            preset: default_preset,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    if has_py_files || explicit_entry.is_some_and(|e| e.ends_with(".py")) {
        let entry = resolve_python_entry(workdir, explicit_entry);
        let mut setup_commands = Vec::new();
        let sys_pkgs = scan_required_system_packages(workdir);
        if !sys_pkgs.is_empty() {
            setup_commands.push(format!(
                "sudo apt-get update -qq && sudo apt-get install -y --no-install-recommends {}",
                sys_pkgs.join(" ")
            ));
        }
        return Ok(RunRecipe {
            name: "python-bare".into(),
            setup_commands,
            run_command: format!("python3 {entry}"),
            preset: default_preset,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 3. Node (package.json)
    if workdir.join("package.json").is_file() {
        let (pm, setup, run) = if workdir.join("bun.lockb").is_file() {
            ("bun", "bun install", "bun run start")
        } else if workdir.join("pnpm-lock.yaml").is_file() {
            ("pnpm", "pnpm install", "pnpm start")
        } else if workdir.join("yarn.lock").is_file() {
            ("yarn", "yarn install", "yarn start")
        } else {
            ("npm", "npm install", "npm start")
        };

        return Ok(RunRecipe {
            name: format!("node-{pm}"),
            setup_commands: vec![setup.into()],
            run_command: run.into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 4. Java / JVM
    if workdir.join("pom.xml").is_file() {
        return Ok(RunRecipe {
            name: "java-maven".into(),
            setup_commands: vec!["mvn -q package -DskipTests".into()],
            run_command: "java -jar $(ls -t target/*.jar | head -n 1)".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    if workdir.join("build.gradle").is_file() || workdir.join("build.gradle.kts").is_file() {
        return Ok(RunRecipe {
            name: "java-gradle".into(),
            setup_commands: vec!["./gradlew build -x test".into()],
            run_command: "java -jar $(ls -t build/libs/*.jar | head -n 1)".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 5. C / C++
    if workdir.join("CMakeLists.txt").is_file() {
        return Ok(RunRecipe {
            name: "cmake".into(),
            setup_commands: vec!["cmake -B build && cmake --build build".into()],
            run_command: "./build/$(ls -t build/ | head -n 1)".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 6. Go
    if workdir.join("go.mod").is_file() {
        return Ok(RunRecipe {
            name: "go".into(),
            setup_commands: vec!["go build -o app .".into()],
            run_command: "./app".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 7. Rust
    if workdir.join("Cargo.toml").is_file() {
        let sys_pkgs = scan_required_system_packages(workdir);
        let mut setup = Vec::new();
        if !sys_pkgs.is_empty() {
            setup.push(format!(
                "sudo apt-get update -qq && sudo apt-get install -y -qq {}",
                sys_pkgs.join(" ")
            ));
        }
        setup.push("cargo build --release".into());
        return Ok(RunRecipe {
            name: "rust-cargo".into(),
            setup_commands: setup,
            run_command: "cargo run --release".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 8. Ruby
    if workdir.join("Gemfile").is_file() {
        return Ok(RunRecipe {
            name: "ruby-bundler".into(),
            setup_commands: vec!["bundle install".into()],
            run_command: "bundle exec ruby main.rb".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 9. .NET
    if contains_extension(workdir, "csproj") || contains_extension(workdir, "sln") {
        return Ok(RunRecipe {
            name: "dotnet".into(),
            setup_commands: vec!["dotnet build -c Release".into()],
            run_command: "dotnet run -c Release".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 10. Scripts at root
    if workdir.join("run.sh").is_file() {
        return Ok(RunRecipe {
            name: "script-run".into(),
            setup_commands: vec!["chmod +x run.sh".into()],
            run_command: "./run.sh".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }
    if workdir.join("entrypoint.sh").is_file() {
        return Ok(RunRecipe {
            name: "script-entrypoint".into(),
            setup_commands: vec!["chmod +x entrypoint.sh".into()],
            run_command: "./entrypoint.sh".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    // 11. Makefile with a `run:` target
    let makefile = workdir.join("Makefile");
    if makefile.is_file() {
        let content = fs::read_to_string(&makefile).unwrap_or_default();
        if content.lines().any(|l| l.trim_start().starts_with("run:")) {
            return Ok(RunRecipe {
                name: "make-run".into(),
                setup_commands: vec![],
                run_command: "make run".into(),
                preset: Preset::Normal,
                workdir: workdir.to_path_buf(),
                output_dir,
            });
        }
        return Ok(RunRecipe {
            name: "make".into(),
            setup_commands: vec!["make".into()],
            run_command: "./a.out".into(),
            preset: Preset::Normal,
            workdir: workdir.to_path_buf(),
            output_dir,
        });
    }

    anyhow::bail!(
        "could not infer a run recipe for `{}`. Add a `qecs.toml` or `run.sh` entrypoint.",
        workdir.display()
    )
}

fn resolve_python_entry(workdir: &Path, explicit: Option<&str>) -> String {
    if let Some(entry) = explicit {
        return entry.to_string();
    }

    let candidates = [
        "main.py",
        "app.py",
        "__main__.py",
        "run.py",
        "train.py",
        "job.py",
    ];
    for c in candidates {
        if workdir.join(c).is_file() {
            return c.to_string();
        }
    }

    // Fallback: find any .py file in directory
    if let Ok(entries) = fs::read_dir(workdir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension()
                && ext == "py"
                && let Some(name) = path.file_name().and_then(|s| s.to_str())
            {
                return name.to_string();
            }
        }
    }

    "main.py".to_string()
}

fn contains_extension(dir: &Path, extension: &str) -> bool {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension()
                && ext == extension
            {
                return true;
            }
        }
    }
    false
}

/// Scan Python files for GPU libraries (torch, vllm, transformers, etc.).
fn scan_python_gpu_need(workdir: &Path) -> bool {
    let gpu_indicators = [
        "torch",
        "torchaudio",
        "torchvision",
        "transformers",
        "vllm",
        "accelerate",
        "diffusers",
        "whisperx",
        "faster_whisper",
    ];

    if let Ok(entries) = fs::read_dir(workdir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension()
                && ext == "py"
                && let Ok(content) = fs::read_to_string(&path)
            {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                        for ind in &gpu_indicators {
                            if trimmed.contains(ind) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Heuristically inspect manifests in `workdir` to detect system libraries (.so)
/// required by popular Python/Node/Rust native packages.
pub fn scan_required_system_packages(workdir: &Path) -> Vec<&'static str> {
    let mut packages = std::collections::BTreeSet::new();

    let mut manifest_text = String::new();
    for file in &[
        "requirements.txt",
        "pyproject.toml",
        "Pipfile",
        "environment.yml",
        "package.json",
        "Cargo.toml",
    ] {
        let p = workdir.join(file);
        if let Ok(content) = fs::read_to_string(&p) {
            manifest_text.push_str(&content.to_lowercase());
            manifest_text.push('\n');
        }
    }

    if manifest_text.is_empty() {
        return Vec::new();
    }

    // OpenCV -> libgl1, libglib2.0-0
    if manifest_text.contains("opencv") || manifest_text.contains("cv2") {
        packages.insert("libgl1");
        packages.insert("libglib2.0-0");
    }

    // Audio/Video -> ffmpeg, libsndfile1
    if manifest_text.contains("torchaudio")
        || manifest_text.contains("whisper")
        || manifest_text.contains("soundfile")
        || manifest_text.contains("librosa")
        || manifest_text.contains("ffmpeg")
    {
        packages.insert("ffmpeg");
        packages.insert("libsndfile1");
    }

    // PostgreSQL -> libpq-dev
    if manifest_text.contains("psycopg") || manifest_text.contains("asyncpg") {
        packages.insert("libpq-dev");
    }

    // Graphviz -> graphviz, libgraphviz-dev
    if manifest_text.contains("graphviz") || manifest_text.contains("pygraphviz") {
        packages.insert("graphviz");
        packages.insert("libgraphviz-dev");
    }

    // Cryptography / SSL -> libssl-dev, libffi-dev
    if manifest_text.contains("cryptography") || manifest_text.contains("pyopenssl") {
        packages.insert("libssl-dev");
        packages.insert("libffi-dev");
    }

    // MySQL -> default-libmysqlclient-dev
    if manifest_text.contains("mysqlclient") {
        packages.insert("default-libmysqlclient-dev");
    }

    // XML -> libxml2-dev, libxslt1-dev
    if manifest_text.contains("lxml") {
        packages.insert("libxml2-dev");
        packages.insert("libxslt1-dev");
    }

    packages.into_iter().collect()
}

/// Compute a deterministic cache key from the project's lockfile or dependency manifest.
pub fn compute_cache_key(workdir: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};

    // 1. uv.lock
    let uv_lock = workdir.join("uv.lock");
    if let Ok(bytes) = fs::read(&uv_lock) {
        let hash = hex::encode(Sha256::digest(&bytes));
        return Some(format!("uv-{}", &hash[..16]));
    }

    // 2. Cargo.lock
    let cargo_lock = workdir.join("Cargo.lock");
    if let Ok(bytes) = fs::read(&cargo_lock) {
        let hash = hex::encode(Sha256::digest(&bytes));
        return Some(format!("cargo-{}", &hash[..16]));
    }

    // 3. requirements.txt
    let reqs = workdir.join("requirements.txt");
    if let Ok(bytes) = fs::read(&reqs) {
        let hash = hex::encode(Sha256::digest(&bytes));
        return Some(format!("pip-{}", &hash[..16]));
    }

    // 4. pnpm-lock.yaml
    let pnpm_lock = workdir.join("pnpm-lock.yaml");
    if let Ok(bytes) = fs::read(&pnpm_lock) {
        let hash = hex::encode(Sha256::digest(&bytes));
        return Some(format!("pnpm-{}", &hash[..16]));
    }

    // 5. package-lock.json
    let pkg_lock = workdir.join("package-lock.json");
    if let Ok(bytes) = fs::read(&pkg_lock) {
        let hash = hex::encode(Sha256::digest(&bytes));
        return Some(format!("npm-{}", &hash[..16]));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_dockerfile() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "docker");
        assert_eq!(
            recipe.run_command,
            "docker run --rm -v $(pwd):/workspace -w /workspace qecs-job"
        );
    }

    #[test]
    fn detects_compose_yaml() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  app:\n    image: redis\n",
        )
        .unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "docker-compose");
        assert!(
            recipe
                .run_command
                .contains("docker compose -f compose.yaml up")
        );
    }

    #[test]
    fn detects_python_uv() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pyproject.toml"), "[tool.uv]\n").unwrap();
        fs::write(dir.path().join("app.py"), "print('hello')\n").unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "python-uv");
        assert_eq!(recipe.run_command, "uv run app.py");
        assert_eq!(recipe.preset, Preset::Normal);
    }

    #[test]
    fn detects_python_gpu_requirement() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("requirements.txt"), "torch\n").unwrap();
        fs::write(
            dir.path().join("train.py"),
            "import torch\nprint(torch.cuda.is_available())\n",
        )
        .unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "python-pip");
        assert_eq!(recipe.preset, Preset::Gpu);
        assert_eq!(
            recipe.run_command,
            ". .venv/bin/activate && python3 train.py"
        );
    }

    #[test]
    fn detects_rust_cargo() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "rust-cargo");
        assert_eq!(recipe.run_command, "cargo run --release");
    }

    #[test]
    fn detects_node_pnpm() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{\"name\":\"demo\"}\n").unwrap();
        fs::write(dir.path().join("pnpm-lock.yaml"), "lockfileVersion: 5.4\n").unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "node-pnpm");
        assert_eq!(recipe.run_command, "pnpm start");
    }

    #[test]
    fn detects_script_run_sh() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "script-run");
        assert_eq!(recipe.run_command, "./run.sh");
    }

    #[test]
    fn qecs_toml_overrides_recipe() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("qecs.toml"),
            "[qecs]\npreset = \"beefy\"\nrun = \"cargo bench\"\noutput_dir = \"benchmark-results\"\n",
        )
        .unwrap();

        let recipe = detect_recipe(dir.path()).unwrap();
        assert_eq!(recipe.name, "rust-cargo");
        assert_eq!(recipe.preset, Preset::Beefy);
        assert_eq!(recipe.run_command, "cargo bench");
        assert_eq!(recipe.output_dir, "benchmark-results");
    }

    #[test]
    fn explicit_file_path_sets_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let py_file = dir.path().join("custom_task.py");
        fs::write(&py_file, "print('custom')\n").unwrap();

        let recipe = detect_recipe(&py_file).unwrap();
        assert_eq!(recipe.name, "python-bare");
        assert_eq!(recipe.run_command, "python3 custom_task.py");
    }

    #[test]
    fn scan_required_system_packages_detects_libraries() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("requirements.txt"),
            "opencv-python>=4.8\ntorchaudio\npsycopg2-binary\n",
        )
        .unwrap();

        let pkgs = scan_required_system_packages(dir.path());
        assert!(pkgs.contains(&"libgl1"));
        assert!(pkgs.contains(&"libglib2.0-0"));
        assert!(pkgs.contains(&"ffmpeg"));
        assert!(pkgs.contains(&"libsndfile1"));
        assert!(pkgs.contains(&"libpq-dev"));
    }

    #[test]
    fn compute_cache_key_detects_lockfiles() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(compute_cache_key(dir.path()), None);

        fs::write(dir.path().join("requirements.txt"), "torch==2.1.0\n").unwrap();
        let key = compute_cache_key(dir.path()).unwrap();
        assert!(key.starts_with("pip-"));

        fs::write(dir.path().join("uv.lock"), "version = 1\n").unwrap();
        let uv_key = compute_cache_key(dir.path()).unwrap();
        assert!(uv_key.starts_with("uv-"));

        fs::write(dir.path().join("Cargo.lock"), "version = 3\n").unwrap();
        // uv.lock has higher priority in compute_cache_key
        assert!(compute_cache_key(dir.path()).unwrap().starts_with("uv-"));
    }
}
