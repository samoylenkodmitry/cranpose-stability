use crate::{Config, Report, SourceFile, analyze::analyze_with, deps::Deps};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Overlay {
    pub path: PathBuf,
    pub source: String,
}

/// Editor input. Overlays replace disk content, including files not saved yet.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectRequest {
    pub root: PathBuf,
    #[serde(default)]
    pub overlays: Vec<Overlay>,
    #[serde(default)]
    pub only: Vec<String>,
    /// Where Cargo keeps dependency sources. Defaults to `CARGO_HOME`, then `~/.cargo`.
    #[serde(default)]
    pub cargo_home: Option<PathBuf>,
}

/// Load configuration and source without running Cargo, rustc or build scripts.
/// Dependency sources are read from Cargo's local cache on demand; nothing is fetched.
pub fn analyze_project(request: &ProjectRequest) -> Result<Report> {
    let root = request.root.canonicalize().context("project root")?;
    if !root.is_dir() {
        bail!("project root must be a directory");
    }
    let config = load_config(&root)?;
    let excludes = config
        .exclude
        .iter()
        .map(|p| globset::Glob::new(p).map(|g| g.compile_matcher()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut overlays = BTreeMap::new();
    for overlay in &request.overlays {
        let path = if overlay.path.is_absolute() {
            overlay.path.clone()
        } else {
            root.join(&overlay.path)
        };
        // Lexical traversal is rejected even for new unsaved files.
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("overlay must stay inside the project root");
        }
        let path = if path.exists() {
            path.canonicalize()?
        } else {
            path.parent()
                .context("overlay parent")?
                .canonicalize()?
                .join(path.file_name().context("overlay filename")?)
        };
        if !path.starts_with(&root) {
            bail!("overlay must stay inside the project root");
        }
        if overlay.source.len() > 4 * 1024 * 1024 {
            bail!("overlay exceeds 4 MiB");
        }
        overlays.insert(path, overlay.source.clone());
    }
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some(
                    ".git"
                        | "target"
                        | ".idea"
                        | ".gradle"
                        | ".intellijPlatform"
                        | "node_modules"
                        | ".venv"
                )
            ) && !excludes
                .iter()
                .any(|g| g.is_match(relative(&root, e.path())))
        })
    {
        let entry = entry.context("read project directory")?;
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "rs") {
            paths.push(entry.into_path());
        }
    }
    paths.extend(
        overlays
            .keys()
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .cloned(),
    );
    paths.sort();
    paths.dedup();
    if paths.len() > 20_000 {
        bail!("more than 20,000 Rust files; narrow the project root or configure exclude");
    }
    let lock_dir = root
        .ancestors()
        .take(8)
        .find(|d| d.join("Cargo.lock").is_file())
        .unwrap_or(&root);
    let cargo_home = request
        .cargo_home
        .clone()
        .or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        });
    let mut deps = Deps::new(cargo_home, Some(lock_dir));
    let mut files = Vec::new();
    let mut manifests = BTreeMap::new();
    for path in paths {
        let rel = relative(&root, &path);
        if excludes.iter().any(|g| g.is_match(&rel)) {
            continue;
        }
        let source = if let Some(s) = overlays.remove(&path) {
            s
        } else {
            if path.metadata()?.len() > 4 * 1024 * 1024 {
                bail!("{rel}: source exceeds 4 MiB");
            }
            std::fs::read_to_string(&path).with_context(|| format!("read {rel}"))?
        };
        let package = path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&root))
            .find(|p| p.join("Cargo.toml").is_file())
            .unwrap_or(&root)
            .to_path_buf();
        if !manifests.contains_key(&package) {
            let manifest = std::fs::read_to_string(package.join("Cargo.toml"))
                .ok()
                .and_then(|s| toml::from_str::<toml::Value>(&s).ok());
            let name = manifest
                .as_ref()
                .and_then(|v| v.get("package")?.get("name")?.as_str())
                .map(str::to_string);
            // Qualified names keep the package name; binaries may use a renamed library name.
            let key = name
                .as_ref()
                .map(|n| n.replace('-', "_"))
                .unwrap_or_else(|| "crate".into());
            deps.add_local(&key, name.as_deref().unwrap_or_default(), Some(&package));
            if let Some(lib) = manifest
                .as_ref()
                .and_then(|v| v.get("lib")?.get("name")?.as_str())
                .filter(|lib| *lib != key)
            {
                deps.extend_aliases(&key, &[(lib.to_string(), key.clone())].into());
            }
            manifests.insert(package.clone(), (key, manifest));
        }
        files.push((rel, source, package, path));
    }
    // Every project package is registered before dependencies are mapped, so path
    // dependencies between members stay project crates.
    let mut aliases = BTreeMap::new();
    for (dir, (key, manifest)) in &manifests {
        let map = manifest
            .as_ref()
            .map(|m| deps.local_aliases(key, m))
            .unwrap_or_default();
        aliases.insert(dir.clone(), map);
    }
    let mut sources = Vec::new();
    for (rel, source, package, path) in files {
        let (crate_name, _) = &manifests[&package];
        let local = path
            .strip_prefix(&package)
            .context("package-relative path")?;
        let mut module = local
            .with_extension("")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if module.first().is_some_and(|p| p == "src") {
            module.remove(0);
        }
        if module
            .last()
            .is_some_and(|p| matches!(p.as_str(), "lib" | "main" | "mod"))
        {
            module.pop();
        }
        sources.push(SourceFile {
            path: rel,
            source,
            crate_name: crate_name.clone(),
            module,
            crate_aliases: aliases[&package].clone(),
        });
    }
    let mut report = analyze_with(&sources, &config, deps);
    if !request.only.is_empty() {
        report
            .composables
            .retain(|c| request.only.contains(&c.path));
        report
            .diagnostics
            .retain(|d| request.only.contains(&d.path));
    }
    Ok(report)
}
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Read the optional shared project configuration.
pub fn load_config(root: &Path) -> Result<Config> {
    let path = root.join("cranpose-stability.toml");
    if !path.exists() {
        return Ok(Config::default());
    }
    let config: Config =
        toml::from_str(&std::fs::read_to_string(&path)?).context("cranpose-stability.toml")?;
    for a in &config.allow {
        if a.reason.trim().is_empty() {
            bail!("every allow entry needs a nonempty reason");
        }
        if !["CP001", "CP002", "CP003", "CP004", "CP005"].contains(&a.rule.as_str()) {
            bail!("unknown suppression rule {}", a.rule);
        }
        globset::Glob::new(&a.path)?;
    }
    Ok(config)
}
