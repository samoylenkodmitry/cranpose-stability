//! Offline dependency sources from Cargo.lock, manifests and CARGO_HOME.
//! Nothing is fetched, built or executed; missing sources are reported, not repaired.
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

#[derive(Default)]
struct Locked {
    name: String,
    version: String,
    source: Option<String>,
    dependencies: Vec<String>,
}
/// One crate the index can name: a project package or a dependency read on demand.
pub(crate) struct Crate {
    pub key: String,
    pub label: String,
    pub local: bool,
    pub dir: Option<PathBuf>,
    pub root: Result<PathBuf, String>,
    package: String,
    locked: Option<usize>,
    aliases: Option<BTreeMap<String, String>>,
}
/// Crate names, versions and source roots for one analysis run.
#[derive(Default)]
pub(crate) struct Deps {
    cargo_home: Option<PathBuf>,
    lock: Vec<Locked>,
    lock_names: HashMap<String, Vec<usize>>,
    workspace: Option<(PathBuf, toml::Value)>,
    /// `[patch]` path overrides from the workspace manifest and Cargo configuration.
    patches: HashMap<String, PathBuf>,
    crates: Vec<Crate>,
    by_key: HashMap<String, usize>,
    by_lock: HashMap<usize, usize>,
    by_dir: HashMap<PathBuf, usize>,
    registries: Option<Vec<PathBuf>>,
    checkouts: HashMap<PathBuf, HashMap<String, PathBuf>>,
    workspaces: HashMap<PathBuf, Option<(PathBuf, toml::Value)>>,
}

impl Deps {
    /// `lock_dir` holds Cargo.lock and the workspace manifest, when present.
    pub fn new(cargo_home: Option<PathBuf>, lock_dir: Option<&Path>) -> Self {
        let read = |name: &str| lock_dir.and_then(|d| std::fs::read_to_string(d.join(name)).ok());
        let lock = read("Cargo.lock")
            .map(|s| parse_lock(&s))
            .unwrap_or_default();
        let mut lock_names: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in lock.iter().enumerate() {
            lock_names.entry(p.name.clone()).or_default().push(i);
        }
        let workspace = lock_dir
            .zip(read("Cargo.toml"))
            .and_then(|(d, s)| Some((d.to_path_buf(), toml::from_str::<toml::Value>(&s).ok()?)));
        // Cargo configuration paths are relative to the directory holding `.cargo`;
        // nearer files take precedence, and the manifest's own [patch] comes first.
        let mut configs: Vec<(PathBuf, PathBuf)> = lock_dir
            .into_iter()
            .flat_map(Path::ancestors)
            .flat_map(|d| {
                ["config.toml", "config"].map(|f| (d.to_path_buf(), d.join(".cargo").join(f)))
            })
            .collect();
        if let Some(home) = &cargo_home {
            let base = home.parent().unwrap_or(home).to_path_buf();
            configs.extend(["config.toml", "config"].map(|f| (base.clone(), home.join(f))));
        }
        let mut patches = HashMap::new();
        let tables = workspace.iter().map(|(d, w)| (d.clone(), w.clone())).chain(
            configs.into_iter().filter_map(|(base, file)| {
                Some((
                    base,
                    toml::from_str::<toml::Value>(&std::fs::read_to_string(file).ok()?).ok()?,
                ))
            }),
        );
        for (base, table) in tables {
            let sources = table.get("patch").and_then(toml::Value::as_table);
            for (name, dep) in sources
                .into_iter()
                .flat_map(|t| t.values())
                .filter_map(|t| t.as_table())
                .flatten()
            {
                let package = dep
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(name);
                if let Some(path) = dep.get("path").and_then(toml::Value::as_str) {
                    patches
                        .entry(package.to_string())
                        .or_insert_with(|| join(&base, path));
                }
            }
        }
        Self {
            cargo_home,
            lock,
            lock_names,
            workspace,
            patches,
            ..Self::default()
        }
    }
    pub fn get(&self, key: &str) -> Option<&Crate> {
        self.by_key.get(key).map(|&i| &self.crates[i])
    }
    /// Register an analyzed project package; `local_aliases` fills in its dependencies.
    pub fn add_local(&mut self, key: &str, package: &str, dir: Option<&Path>) {
        if self.by_key.contains_key(key) {
            return;
        }
        let id = self.crates.len();
        self.crates.push(Crate {
            key: key.into(),
            label: format!("crate {key}"),
            local: true,
            dir: dir.map(Path::to_path_buf),
            root: Err(format!("{key} is analyzed from project sources")),
            package: package.into(),
            locked: None,
            aliases: Some(BTreeMap::new()),
        });
        self.by_key.insert(key.into(), id);
        if let Some(dir) = dir {
            self.by_dir.insert(dir.to_path_buf(), id);
        }
    }
    pub fn extend_aliases(&mut self, key: &str, aliases: &BTreeMap<String, String>) {
        if let Some(&i) = self.by_key.get(key) {
            self.crates[i]
                .aliases
                .get_or_insert_default()
                .extend(aliases.iter().map(|(a, b)| (a.clone(), b.clone())));
        }
    }
    /// Dependency aliases of a project package, mapped to crate keys.
    pub fn local_aliases(&mut self, key: &str, manifest: &toml::Value) -> BTreeMap<String, String> {
        let Some(&id) = self.by_key.get(key) else {
            return BTreeMap::new();
        };
        let package = self.crates[id].package.clone();
        self.crates[id].locked = self
            .locked(&package)
            .into_iter()
            .find(|&i| self.lock[i].source.is_none());
        let aliases = self.resolve_manifest(id, manifest, true);
        self.extend_aliases(key, &aliases);
        aliases
    }
    /// The crate key a dependency alias of crate `key` names; manifests are read on first use.
    pub fn alias(&mut self, key: &str, name: &str) -> Option<String> {
        let &id = self.by_key.get(key)?;
        if self.crates[id].aliases.is_none() {
            let manifest = self.crates[id].dir.as_deref().and_then(manifest);
            let aliases = manifest
                .map(|m| self.resolve_manifest(id, &m, false))
                .unwrap_or_default();
            self.crates[id].aliases = Some(aliases);
        }
        self.crates[id].aliases.as_ref()?.get(name).cloned()
    }
    fn locked(&self, name: &str) -> Vec<usize> {
        self.lock_names.get(name).cloned().unwrap_or_default()
    }
    fn resolve_manifest(
        &mut self,
        id: usize,
        m: &toml::Value,
        dev: bool,
    ) -> BTreeMap<String, String> {
        let dir = self.crates[id].dir.clone();
        let sections: &[&str] = if dev {
            &["dependencies", "dev-dependencies"]
        } else {
            &["dependencies"]
        };
        let mut tables: Vec<&toml::Value> = Vec::new();
        for s in sections {
            tables.extend(m.get(s));
            if let Some(targets) = m.get("target").and_then(toml::Value::as_table) {
                tables.extend(targets.values().filter_map(|t| t.get(s)));
            }
        }
        let mut workspace = None;
        let mut aliases = BTreeMap::new();
        for (alias, value) in tables.iter().filter_map(|t| t.as_table()).flatten() {
            let mut base = dir.clone();
            let mut value = value;
            if value.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                let w = workspace
                    .get_or_insert_with(|| dir.as_deref().and_then(|d| self.workspace_of(d)));
                if let Some((wdir, w)) = w.as_ref()
                    && let Some(v) = w
                        .get("workspace")
                        .and_then(|w| w.get("dependencies"))
                        .and_then(|d| d.get(alias))
                {
                    base = Some(wdir.clone());
                    value = v;
                }
            }
            let package = value
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(alias)
                .to_string();
            let path = value
                .get("path")
                .and_then(toml::Value::as_str)
                .zip(base)
                .map(|(p, b)| join(&b, p));
            let req = value
                .as_str()
                .or_else(|| value.get("version").and_then(toml::Value::as_str))
                .unwrap_or_default()
                .to_string();
            let key = self.dependency(id, &package, path, &req);
            aliases.insert(alias.replace('-', "_"), key);
        }
        aliases
    }
    /// The crate key of dependency `package` of crate `id`, registered if needed.
    fn dependency(&mut self, id: usize, package: &str, path: Option<PathBuf>, req: &str) -> String {
        if let Some(path) = path {
            let path = path.canonicalize().unwrap_or(path);
            if let Some(&i) = self.by_dir.get(&path) {
                return self.crates[i].key.clone();
            }
            let locked = self
                .locked(package)
                .into_iter()
                .find(|&i| self.lock[i].source.is_none());
            return self.register(package, locked, Ok(path));
        }
        let owner = self.crates[id].locked;
        let candidates: Vec<usize> = owner
            .map(|o| {
                self.lock[o]
                    .dependencies
                    .iter()
                    .filter(|spec| spec.split(' ').next() == Some(package))
                    .filter_map(|spec| self.find_locked(spec))
                    .collect()
            })
            .unwrap_or_default();
        let locked = candidates
            .iter()
            .copied()
            .find(|&i| compatible(req, &self.lock[i].version))
            .or(candidates.first().copied());
        if let Some(i) = locked {
            if let Some(&c) = self.by_lock.get(&i) {
                return self.crates[c].key.clone();
            }
            if self.lock[i].source.is_none() {
                // A locked path package: a project member or a [patch] override.
                if let Some(c) = self.crates.iter().find(|c| c.local && c.package == package) {
                    return c.key.clone();
                }
                let patched = self.patch_path(package);
                return self.register(package, Some(i), patched);
            }
            let dir = self.locate(i);
            return self.register(package, Some(i), dir);
        }
        if owner.is_some() || self.lock.is_empty() && self.cargo_home.is_none() {
            let why = if owner.is_some() {
                format!(
                    "Cargo.lock does not list {package} for this crate; run `cargo fetch` to refresh it"
                )
            } else {
                format!("{package} is not locked and CARGO_HOME is unknown")
            };
            return self.register(package, None, Err(why));
        }
        let dir = self.registry_best(package, req);
        self.register(package, None, dir)
    }
    fn register(
        &mut self,
        package: &str,
        locked: Option<usize>,
        dir: Result<PathBuf, String>,
    ) -> String {
        if let Ok(d) = &dir
            && let Some(&i) = self.by_dir.get(d)
        {
            return self.crates[i].key.clone();
        }
        let manifest = dir.as_deref().ok().and_then(manifest);
        let lib = manifest
            .as_ref()
            .and_then(|m| m.get("lib")?.get("name")?.as_str().map(str::to_string))
            .unwrap_or_else(|| package.replace('-', "_"));
        let version = locked
            .map(|i| self.lock[i].version.clone())
            .or_else(|| {
                manifest.as_ref().and_then(|m| {
                    m.get("package")?
                        .get("version")?
                        .as_str()
                        .map(str::to_string)
                })
            })
            .unwrap_or_default();
        let label = format!("{package} {version}").trim().to_string();
        let mut key = lib.clone();
        if let Some(&i) = self.by_key.get(&key) {
            if !self.crates[i].local && self.crates[i].label == label {
                return key;
            }
            key = format!("{lib}@{version}");
            if self.by_key.contains_key(&key) {
                return key;
            }
        }
        let root = match &dir {
            Ok(d) => {
                let path = manifest
                    .as_ref()
                    .and_then(|m| m.get("lib")?.get("path")?.as_str().map(str::to_string))
                    .unwrap_or_else(|| "src/lib.rs".into());
                let file = join(d, &path);
                if file.is_file() {
                    Ok(file)
                } else {
                    Err(format!(
                        "{label} has no library source at {}",
                        file.display()
                    ))
                }
            }
            Err(e) => Err(e.clone()),
        };
        let id = self.crates.len();
        self.crates.push(Crate {
            key: key.clone(),
            label,
            local: false,
            dir: dir.ok(),
            root,
            package: package.into(),
            locked,
            aliases: None,
        });
        self.by_key.insert(key.clone(), id);
        if let Some(i) = locked {
            self.by_lock.insert(i, id);
        }
        if let Some(d) = self.crates[id].dir.clone() {
            self.by_dir.insert(d, id);
        }
        key
    }
    fn find_locked(&self, spec: &str) -> Option<usize> {
        let mut parts = spec.split(' ');
        let name = parts.next()?;
        let version = parts.next();
        let source = parts
            .next()
            .map(|s| s.trim_matches(|c| c == '(' || c == ')'));
        self.lock_names.get(name)?.iter().copied().find(|&i| {
            let p = &self.lock[i];
            version.is_none_or(|v| p.version == v)
                && source.is_none_or(|s| p.source.as_deref() == Some(s))
        })
    }
    fn home(&self) -> Result<&Path, String> {
        self.cargo_home
            .as_deref()
            .ok_or_else(|| "CARGO_HOME is unknown, so dependency sources cannot be read".into())
    }
    fn locate(&mut self, i: usize) -> Result<PathBuf, String> {
        let (name, version) = (self.lock[i].name.clone(), self.lock[i].version.clone());
        let source = self.lock[i].source.clone().unwrap_or_default();
        if let Some(git) = source.strip_prefix("git+") {
            return self.git(&name, &version, git);
        }
        let home = self.home()?.to_path_buf();
        self.registries()
            .iter()
            .map(|r| r.join(format!("{name}-{version}")))
            .find(|d| d.join("Cargo.toml").is_file())
            .ok_or_else(|| {
                format!(
                    "{name} {version} is not unpacked in {}; run `cargo fetch`",
                    home.join("registry").join("src").display()
                )
            })
    }
    fn registries(&mut self) -> &[PathBuf] {
        let home = self.cargo_home.clone();
        self.registries.get_or_insert_with(|| {
            home.and_then(|h| std::fs::read_dir(h.join("registry").join("src")).ok())
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect()
        })
    }
    /// Without a lock entry, take the newest unpacked version that satisfies the requirement.
    fn registry_best(&mut self, name: &str, req: &str) -> Result<PathBuf, String> {
        let prefix = format!("{name}-");
        let mut best: Option<(Vec<u64>, PathBuf)> = None;
        for r in self.registries().to_vec() {
            for e in std::fs::read_dir(&r).into_iter().flatten().flatten() {
                let file = e.file_name().to_string_lossy().into_owned();
                if let Some(v) = file.strip_prefix(&prefix)
                    && v.starts_with(|c: char| c.is_ascii_digit())
                    && compatible(req, v)
                {
                    let n = numbers(v);
                    if best.as_ref().is_none_or(|(b, _)| &n > b) {
                        best = Some((n, e.path()));
                    }
                }
            }
        }
        best.map(|(_, p)| p).ok_or_else(|| {
            format!(
                "{name} {req} has no Cargo.lock entry and no unpacked source; run `cargo fetch`"
            )
        })
    }
    fn git(&mut self, name: &str, version: &str, url: &str) -> Result<PathBuf, String> {
        let (repo, rev) = url.split_once('#').unwrap_or((url, ""));
        let repo = repo.split('?').next().unwrap_or(repo).trim_end_matches('/');
        let ident = repo
            .rsplit('/')
            .next()
            .unwrap_or(repo)
            .trim_end_matches(".git")
            .to_lowercase();
        let base = self.home()?.join("git").join("checkouts");
        let mut checkouts = Vec::new();
        for repo_dir in std::fs::read_dir(&base).into_iter().flatten().flatten() {
            let dir_name = repo_dir.file_name().to_string_lossy().to_lowercase();
            if !dir_name.starts_with(&format!("{ident}-")) {
                continue;
            }
            for c in std::fs::read_dir(repo_dir.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let short = c.file_name().to_string_lossy().into_owned();
                if short.len() >= 7 && rev.starts_with(&short) {
                    checkouts.push(c.path());
                }
            }
        }
        for checkout in checkouts {
            let packages = self
                .checkouts
                .entry(checkout.clone())
                .or_insert_with(|| packages_in(&checkout));
            if let Some(dir) = packages.get(name) {
                return Ok(dir.clone());
            }
        }
        Err(format!(
            "the git checkout of {name} {version} ({repo} at {}) is not in {}; run `cargo fetch`",
            rev.get(..7).unwrap_or(rev),
            base.display()
        ))
    }
    fn patch_path(&self, package: &str) -> Result<PathBuf, String> {
        self.patches.get(package).cloned().ok_or_else(|| {
            format!("{package} is a locked path dependency, but no manifest or Cargo configuration names its directory")
        })
    }
    fn workspace_of(&mut self, dir: &Path) -> Option<(PathBuf, toml::Value)> {
        if let Some((w, _)) = &self.workspace
            && dir.starts_with(w)
        {
            return self.workspace.clone();
        }
        if let Some(w) = self.workspaces.get(dir) {
            return w.clone();
        }
        let found = dir.ancestors().skip(1).take(6).find_map(|d| {
            let m = manifest(d)?;
            m.get("workspace")?;
            Some((d.to_path_buf(), m))
        });
        self.workspaces.insert(dir.to_path_buf(), found.clone());
        found
    }
}
/// Join a manifest-relative path and resolve `..` lexically; Windows verbatim paths,
/// such as a canonical project root, do not resolve `..` themselves.
pub(crate) fn join(base: &Path, relative: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for part in Path::new(relative).components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            part => out.push(part),
        }
    }
    out
}
fn manifest(dir: &Path) -> Option<toml::Value> {
    toml::from_str(&std::fs::read_to_string(dir.join("Cargo.toml")).ok()?).ok()
}
/// Package directories inside one git checkout, by package name.
fn packages_in(checkout: &Path) -> HashMap<String, PathBuf> {
    walkdir::WalkDir::new(checkout)
        .max_depth(6)
        .into_iter()
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some(".git" | "target" | "node_modules")
            )
        })
        .flatten()
        .filter(|e| e.file_name() == "Cargo.toml")
        .filter_map(|e| {
            let dir = e.path().parent()?.to_path_buf();
            let name = manifest(&dir)?
                .get("package")?
                .get("name")?
                .as_str()?
                .to_string();
            Some((name, dir))
        })
        .collect()
}
fn parse_lock(text: &str) -> Vec<Locked> {
    let mut out: Vec<Locked> = Vec::new();
    let (mut current, mut list) = (false, false);
    let unquote = |s: &str| s.trim().trim_end_matches(',').trim_matches('"').to_string();
    for line in text.lines().map(str::trim) {
        if list {
            if line.starts_with(']') {
                list = false;
            } else if let Some(p) = out.last_mut().filter(|_| current) {
                p.dependencies.push(unquote(line));
            }
            continue;
        }
        if line.starts_with('[') {
            current = line == "[[package]]";
            if current {
                out.push(Locked::default());
            }
            continue;
        }
        let (Some(p), Some((k, v))) = (out.last_mut().filter(|_| current), line.split_once(" = "))
        else {
            continue;
        };
        match k {
            "name" => p.name = unquote(v),
            "version" => p.version = unquote(v),
            "source" => p.source = Some(unquote(v)),
            "dependencies" if v.trim() == "[" => list = true,
            "dependencies" => p.dependencies.extend(
                v.trim()
                    .trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .map(unquote)
                    .filter(|s| !s.is_empty()),
            ),
            _ => {}
        }
    }
    out
}
fn numbers(v: &str) -> Vec<u64> {
    v.split(['-', '+'])
        .next()
        .unwrap_or(v)
        .split('.')
        .map(|n| n.parse().unwrap_or(0))
        .collect()
}
/// A small subset of Cargo requirements, enough to choose between locked versions.
fn compatible(req: &str, version: &str) -> bool {
    let req = req.split(',').next().unwrap_or_default().trim();
    let exact = req.starts_with('=');
    let req = req.trim_start_matches(['^', '~', '=', '>', '<', ' ']);
    if req.is_empty() || req == "*" {
        return true;
    }
    let (r, v) = (numbers(req.trim_end_matches(".*")), numbers(version));
    if exact {
        return r.iter().zip(&v).all(|(a, b)| a == b);
    }
    // Caret: components up to the first nonzero one must match.
    let fixed = r.iter().position(|&n| n != 0).map_or(r.len(), |i| i + 1);
    r.iter().zip(&v).take(fixed).all(|(a, b)| a == b) && v >= r
}
