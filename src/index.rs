//! Name resolution over project sources and dependency sources read on demand.
use crate::{deps::Deps, macros::Rules};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};
use syn::{Item, Type, UseTree};

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct Context {
    pub crate_name: String,
    pub module: Vec<String>,
}
impl Context {
    pub fn key(&self) -> String {
        std::iter::once(self.crate_name.as_str())
            .chain(self.module.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("::")
    }
    pub fn child(&self, name: String) -> Self {
        let mut c = self.clone();
        c.module.push(name);
        c
    }
    /// The scope of items declared in a function body, written `{name}`.
    pub fn block(&self, function: &syn::Ident) -> Self {
        self.child(format!("{{{function}}}"))
    }
    pub fn is_block(&self) -> bool {
        self.module.last().is_some_and(|m| m.starts_with('{'))
    }
    /// The enclosing module, outside any function-body blocks.
    pub fn scope(&self) -> Self {
        let mut c = self.clone();
        while c.is_block() {
            c.module.pop();
        }
        c
    }
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Vis {
    Private,
    Crate,
    Public,
}
#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) struct Derives {
    pub clone: bool,
    pub eq: bool,
    pub copy: bool,
}
pub(crate) enum Definition {
    Data {
        fields: Vec<Type>,
        generics: syn::Generics,
        derives: Derives,
    },
    Alias {
        ty: Box<Type>,
        generics: syn::Generics,
    },
    Trait {
        supertraits: Vec<syn::TypeParamBound>,
    },
}
pub(crate) struct Named {
    pub definition: Definition,
    pub context: Context,
    pub vis: Vis,
    pub cfg: bool,
}
/// A manual `impl Clone/Copy/PartialEq for T`; its self type is resolved on demand.
pub(crate) struct Impl {
    pub trait_name: String,
    pub self_ty: syn::TypePath,
    pub generics: syn::Generics,
    pub context: Context,
}
#[derive(Clone)]
struct Import {
    path: Vec<String>,
    leading: bool,
    vis: Vis,
}
#[derive(Default)]
struct Module {
    context: Option<Context>,
    pending: Vec<(PathBuf, PathBuf)>,
    imports: BTreeMap<String, Import>,
    globs: Vec<Import>,
    children: BTreeMap<String, Vis>,
}
/// The outcome of resolving a path in the type namespace.
#[derive(Clone)]
pub(crate) struct Resolved {
    pub key: String,
    pub kind: Kind,
    /// Public paths passed through while following re-exports.
    pub via: Vec<String>,
}
#[derive(Clone, PartialEq)]
pub(crate) enum Kind {
    Def,
    Std,
    Module,
    Missing(String),
}
impl Resolved {
    fn new(key: String, kind: Kind) -> Self {
        Self {
            key,
            kind,
            via: Vec::new(),
        }
    }
}
#[derive(Default)]
struct CrateImpls {
    pending: Vec<Rc<Impl>>,
    by_key: HashMap<String, Vec<Rc<Impl>>>,
    scanned: HashSet<(String, String)>,
    sources: Option<Rc<Vec<(PathBuf, String)>>>,
}
#[derive(Default)]
pub(crate) struct Index {
    pub deps: RefCell<Deps>,
    modules: RefCell<HashMap<String, Module>>,
    definitions: RefCell<HashMap<String, Vec<Rc<Named>>>>,
    impls: RefCell<HashMap<String, CrateImpls>>,
    files: RefCell<HashSet<PathBuf>>,
    memo: RefCell<HashMap<(String, String, Vis), Option<Resolved>>>,
    active: RefCell<HashSet<(String, String, Vis)>>,
    /// Shapes of types without arguments, shared by all parameters of one run.
    pub shapes: RefCell<HashMap<String, crate::types::Shape>>,
    /// `macro_rules!` definitions by crate, and item invocations still waiting for one.
    macros: RefCell<HashMap<String, HashMap<String, Rc<Rules>>>>,
    waiting: RefCell<HashMap<String, Vec<Invocation>>>,
    expanding: std::cell::Cell<usize>,
}
struct Invocation {
    name: String,
    tokens: proc_macro2::TokenStream,
    context: Context,
    dir: Option<(PathBuf, PathBuf)>,
}

impl Index {
    pub fn new(deps: Deps) -> Self {
        Self {
            deps: RefCell::new(deps),
            ..Self::default()
        }
    }
    /// Index one parsed project file or function-body block.
    /// Project modules come from file layout, not `mod` items.
    pub fn add_local(&self, items: &[Item], context: &Context) {
        self.module_path(context);
        self.collect(items, context, None);
    }
    /// Make `mod name;` in `parent` refer to the project module a nonstandard path loads.
    pub fn link_module(&self, parent: &Context, name: &str, target: &Context) {
        let mut path = vec![if target.crate_name == parent.crate_name {
            "crate".to_string()
        } else {
            target.crate_name.clone()
        }];
        path.extend(target.module.iter().cloned());
        let mut m = self.module(&parent.key());
        m.children.remove(name);
        m.imports.insert(
            name.into(),
            Import {
                path,
                leading: false,
                vis: Vis::Crate,
            },
        );
    }
    pub fn definitions(&self, key: &str) -> Vec<Rc<Named>> {
        self.definitions
            .borrow()
            .get(key)
            .cloned()
            .unwrap_or_default()
    }
    pub fn label(&self, crate_key: &str) -> Option<String> {
        self.deps
            .borrow()
            .get(crate_key)
            .filter(|c| !c.local)
            .map(|c| c.label.clone())
    }
    fn module_path(&self, context: &Context) {
        let mut modules = self.modules.borrow_mut();
        let mut c = Context {
            crate_name: context.crate_name.clone(),
            module: Vec::new(),
        };
        modules.entry(c.key()).or_default().context = Some(c.clone());
        for name in &context.module {
            modules
                .entry(c.key())
                .or_default()
                .children
                .entry(name.clone())
                .or_insert(Vis::Crate);
            c = c.child(name.clone());
            modules.entry(c.key()).or_default().context = Some(c.clone());
        }
    }
    /// Collect items. `dir` is (path-attribute base, child directory) for dependency files.
    fn collect(&self, items: &[Item], context: &Context, dir: Option<(&Path, &Path)>) {
        let key = context.key();
        let external = dir.is_some();
        for item in items {
            let attrs = attributes(item);
            if external && attrs.iter().any(is_test) {
                continue;
            }
            let cfg = attrs.iter().any(|a| a.path().is_ident("cfg"));
            match item {
                Item::Use(u) => self.import(
                    &u.tree,
                    Vec::new(),
                    u.leading_colon.is_some(),
                    vis(&u.vis),
                    &key,
                ),
                Item::ExternCrate(e) => {
                    let name = e.rename.as_ref().map_or(&e.ident, |(_, r)| r).to_string();
                    self.module(&key).imports.insert(
                        name,
                        Import {
                            path: vec![e.ident.to_string()],
                            leading: true,
                            vis: vis(&e.vis),
                        },
                    );
                }
                Item::Mod(m) => {
                    let name = m.ident.to_string();
                    let child = context.child(name.clone());
                    self.module(&key).children.insert(name.clone(), vis(&m.vis));
                    self.module(&child.key()).context = Some(child.clone());
                    match (&m.content, dir) {
                        (Some((_, items)), Some((_, d))) => {
                            let d = d.join(&name);
                            self.collect(items, &child, Some((&d, &d)));
                        }
                        (Some((_, items)), None) => self.collect(items, &child, None),
                        (None, Some((base, d))) => {
                            let file = path_attr(&m.attrs)
                                .map(|p| crate::deps::join(base, &p))
                                .or_else(|| {
                                    [d.join(format!("{name}.rs")), d.join(&name).join("mod.rs")]
                                        .into_iter()
                                        .find(|p| p.is_file())
                                });
                            if let Some(file) = file {
                                let children = if file.file_name().is_some_and(|f| f == "mod.rs")
                                    || path_attr(&m.attrs).is_some()
                                {
                                    file.parent().map(Path::to_path_buf)
                                } else {
                                    Some(d.join(&name))
                                };
                                if let Some(children) = children {
                                    self.module(&child.key()).pending.push((file, children));
                                }
                                // Textually scoped macros must be known before later modules use them.
                                if m.attrs.iter().any(|a| a.path().is_ident("macro_use")) {
                                    self.ensure(&child.key());
                                }
                            }
                        }
                        (None, None) => {}
                    }
                }
                Item::Struct(s) => self.add(
                    &s.ident,
                    data(s.fields.iter(), &s.generics, &s.attrs),
                    context,
                    vis(&s.vis),
                    cfg,
                ),
                Item::Enum(e) => self.add(
                    &e.ident,
                    data(
                        e.variants.iter().flat_map(|v| v.fields.iter()),
                        &e.generics,
                        &e.attrs,
                    ),
                    context,
                    vis(&e.vis),
                    cfg,
                ),
                Item::Union(u) => self.add(
                    &u.ident,
                    data(u.fields.named.iter(), &u.generics, &u.attrs),
                    context,
                    vis(&u.vis),
                    cfg,
                ),
                Item::Type(t) => self.add(
                    &t.ident,
                    Definition::Alias {
                        ty: t.ty.clone(),
                        generics: t.generics.clone(),
                    },
                    context,
                    vis(&t.vis),
                    cfg,
                ),
                Item::Trait(t) => self.add(
                    &t.ident,
                    Definition::Trait {
                        supertraits: t.supertraits.iter().cloned().collect(),
                    },
                    context,
                    vis(&t.vis),
                    cfg,
                ),
                Item::Macro(m) => self.macro_item(m, context, dir),
                Item::Impl(i) => {
                    if let (Some((None, tr, _)), Type::Path(ty)) = (&i.trait_, i.self_ty.as_ref())
                        && let Some(last) = tr.segments.last()
                        && ["Clone", "PartialEq", "Copy"].contains(&last.ident.to_string().as_str())
                        && self_rhs(&last.arguments, ty)
                    {
                        self.impls
                            .borrow_mut()
                            .entry(context.crate_name.clone())
                            .or_default()
                            .pending
                            .push(Rc::new(Impl {
                                trait_name: last.ident.to_string(),
                                self_ty: ty.clone(),
                                generics: i.generics.clone(),
                                context: context.clone(),
                            }));
                    }
                }
                _ => {}
            }
        }
    }
    /// Record a `macro_rules!` definition, or expand an item invocation of a crate-local one.
    fn macro_item(&self, m: &syn::ItemMacro, context: &Context, dir: Option<(&Path, &Path)>) {
        let Some(name) = m.mac.path.segments.last().map(|s| s.ident.to_string()) else {
            return;
        };
        let krate = context.crate_name.clone();
        if let Some(ident) = &m.ident {
            if name != "macro_rules" {
                return;
            }
            let Some(rules) = Rules::parse(m.mac.tokens.clone()) else {
                return;
            };
            let name = ident.to_string();
            self.macros
                .borrow_mut()
                .entry(krate.clone())
                .or_default()
                .insert(name.clone(), Rc::new(rules));
            let ready: Vec<Invocation> = match self.waiting.borrow_mut().get_mut(&krate) {
                Some(w) => {
                    let (ready, rest) = std::mem::take(w).into_iter().partition(|i| i.name == name);
                    *w = rest;
                    ready
                }
                None => Vec::new(),
            };
            for i in ready {
                let dir = i.dir.as_ref().map(|(a, b)| (a.as_path(), b.as_path()));
                self.expand(&i.name, i.tokens, &i.context, dir);
            }
            return;
        }
        if m.mac.path.segments.len() > 2 {
            return;
        }
        if self
            .macros
            .borrow()
            .get(&krate)
            .is_some_and(|r| r.contains_key(&name))
        {
            self.expand(&name, m.mac.tokens.clone(), context, dir);
        } else {
            self.waiting
                .borrow_mut()
                .entry(krate)
                .or_default()
                .push(Invocation {
                    name,
                    tokens: m.mac.tokens.clone(),
                    context: context.clone(),
                    dir: dir.map(|(a, b)| (a.to_path_buf(), b.to_path_buf())),
                });
        }
    }
    fn expand(
        &self,
        name: &str,
        tokens: proc_macro2::TokenStream,
        context: &Context,
        dir: Option<(&Path, &Path)>,
    ) {
        let rules = self
            .macros
            .borrow()
            .get(&context.crate_name)
            .and_then(|r| r.get(name).cloned());
        let depth = self.expanding.get();
        if depth > 16 {
            return;
        }
        if let Some(file) = rules
            .and_then(|r| r.expand(tokens))
            .and_then(|t| syn::parse2::<syn::File>(t).ok())
        {
            self.expanding.set(depth + 1);
            self.collect(&file.items, context, dir);
            self.expanding.set(depth);
        }
    }
    fn module(&self, key: &str) -> std::cell::RefMut<'_, Module> {
        std::cell::RefMut::map(self.modules.borrow_mut(), |m| {
            m.entry(key.to_string()).or_default()
        })
    }
    fn add(
        &self,
        ident: &syn::Ident,
        definition: Definition,
        context: &Context,
        vis: Vis,
        cfg: bool,
    ) {
        self.definitions
            .borrow_mut()
            .entry(format!("{}::{ident}", context.key()))
            .or_default()
            .push(Rc::new(Named {
                definition,
                context: context.clone(),
                vis,
                cfg,
            }));
    }
    fn import(&self, tree: &UseTree, mut prefix: Vec<String>, leading: bool, vis: Vis, key: &str) {
        match tree {
            UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.import(&p.tree, prefix, leading, vis, key);
            }
            UseTree::Group(g) => {
                for item in &g.items {
                    self.import(item, prefix.clone(), leading, vis, key);
                }
            }
            UseTree::Name(n) => {
                let name = if n.ident == "self" {
                    prefix.last().cloned().unwrap_or_default()
                } else {
                    prefix.push(n.ident.to_string());
                    n.ident.to_string()
                };
                self.module(key).imports.insert(
                    name,
                    Import {
                        path: prefix,
                        leading,
                        vis,
                    },
                );
            }
            UseTree::Rename(n) => {
                if n.ident != "self" {
                    prefix.push(n.ident.to_string());
                }
                if n.rename != "_" {
                    self.module(key).imports.insert(
                        n.rename.to_string(),
                        Import {
                            path: prefix,
                            leading,
                            vis,
                        },
                    );
                }
            }
            UseTree::Glob(_) => self.module(key).globs.push(Import {
                path: prefix,
                leading,
                vis,
            }),
        }
    }
    /// Load a module's pending dependency files. Returns false when the module does not exist.
    fn ensure(&self, key: &str) -> bool {
        let pending = match self.modules.borrow_mut().get_mut(key) {
            Some(m) => std::mem::take(&mut m.pending),
            None => Vec::new(),
        };
        if self.modules.borrow().contains_key(key) {
            for (file, children) in pending {
                self.load(&file, key, &children);
            }
            return true;
        }
        match key.rsplit_once("::") {
            Some((parent, _)) => self.ensure(parent) && self.ensure_known(key),
            None => {
                let root = self.deps.borrow().get(key).map(|c| c.root.clone());
                let Some(Ok(root)) = root else {
                    return false;
                };
                let dir = root.parent().map(Path::to_path_buf).unwrap_or_default();
                let context = Context {
                    crate_name: key.into(),
                    module: Vec::new(),
                };
                self.module(key).context = Some(context);
                self.load(&root, key, &dir);
                true
            }
        }
    }
    fn ensure_known(&self, key: &str) -> bool {
        self.modules.borrow().contains_key(key) && self.ensure(key)
    }
    fn load(&self, file: &Path, key: &str, children: &Path) {
        if !self.files.borrow_mut().insert(file.to_path_buf()) {
            return;
        }
        let Some(context) = self
            .modules
            .borrow()
            .get(key)
            .and_then(|m| m.context.clone())
        else {
            return;
        };
        let Ok(source) = std::fs::read_to_string(file) else {
            return;
        };
        if let Ok(ast) = crate::parse::file(&source) {
            let base = file.parent().unwrap_or(children);
            self.collect(&ast.items, &context, Some((base, children)));
        }
    }
    /// Resolve a path written in `context` to a type definition, std type, module or explanation.
    pub fn resolve(&self, path: &syn::Path, context: &Context) -> Resolved {
        let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        self.path(&segments, path.leading_colon.is_some(), context, false, 0)
    }
    /// Follow explicit imports only; used for attribute macros, which have no type definition.
    pub fn macro_path(&self, path: &syn::Path, context: &Context) -> String {
        let mut segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let mut context = context.clone();
        for _ in 0..8 {
            let Some(first) = segments.first().cloned() else {
                break;
            };
            // A block sees the imports of its enclosing module.
            let import = loop {
                let found = self
                    .modules
                    .borrow()
                    .get(&context.key())
                    .and_then(|m| m.imports.get(&first).cloned());
                if found.is_some() || !context.is_block() {
                    break found;
                }
                context.module.pop();
            };
            let Some(import) = import else { break };
            let mut path = import.path.clone();
            path.extend(segments.drain(1..));
            segments = path;
            match segments.first().map(String::as_str) {
                Some("crate") => segments[0] = context.crate_name.clone(),
                Some("self") => {
                    segments.splice(0..1, context.module.iter().cloned());
                    segments.insert(0, context.crate_name.clone());
                }
                Some("super") => break,
                _ => {}
            }
            let Some(dep) = self.extern_crate(&context, &segments[0]) else {
                break;
            };
            segments[0] = dep;
            context = Context {
                crate_name: segments[0].clone(),
                module: Vec::new(),
            };
            // Follow one re-export at a crate root, such as cranpose_ui's `pub use cranpose_macros::composable`.
            if segments.len() != 2 || !self.ensure(&segments[0]) {
                break;
            }
            let reexported = self
                .modules
                .borrow()
                .get(&segments[0])
                .is_some_and(|m| m.imports.contains_key(&segments[1]));
            if !reexported {
                break;
            }
            segments.remove(0);
        }
        segments.join("::")
    }
    fn extern_crate(&self, context: &Context, name: &str) -> Option<String> {
        if ["std", "core", "alloc"].contains(&name) {
            return None;
        }
        let alias = self.deps.borrow_mut().alias(&context.crate_name, name);
        alias.or_else(|| self.deps.borrow().get(name).map(|c| c.key.clone()))
    }
    fn path(
        &self,
        segments: &[String],
        leading: bool,
        context: &Context,
        import: bool,
        depth: usize,
    ) -> Resolved {
        let Some((first, rest)) = segments.split_first() else {
            return Resolved::new(
                String::new(),
                Kind::Missing("An empty path cannot be resolved.".into()),
            );
        };
        if depth > 24 {
            return Resolved::new(
                segments.join("::"),
                Kind::Missing(format!(
                    "Imports of {} form a cycle or are too deeply nested.",
                    segments.join("::")
                )),
            );
        }
        let crate_root = Resolved::new(context.crate_name.clone(), Kind::Module);
        let mut current = match first.as_str() {
            "crate" => crate_root,
            "self" => Resolved::new(context.scope().key(), Kind::Module),
            "super" => Resolved::new(parent(&context.scope().key()), Kind::Module),
            "std" | "core" | "alloc" => Resolved::new("std".into(), Kind::Std),
            _ if leading => match self.extern_crate(context, first) {
                Some(key) => Resolved::new(key, Kind::Module),
                None => unknown_crate(first),
            },
            _ => self.lexical(first, rest.is_empty(), context, import, depth),
        };
        for segment in rest {
            current = match current.kind {
                Kind::Module if segment == "super" => {
                    Resolved::new(parent(&current.key), Kind::Module)
                }
                Kind::Module => {
                    let found = self.lookup(&current.key, segment, Vis::Private, depth + 1);
                    let mut next = found.unwrap_or_else(|| self.absent(&current.key, segment));
                    next.via.splice(0..0, current.via);
                    next
                }
                Kind::Std => Resolved::new(format!("{}::{segment}", current.key), Kind::Std),
                Kind::Def => Resolved::new(
                    format!("{}::{segment}", current.key),
                    Kind::Missing(format!(
                        "{}::{segment} is an associated item; it needs compiler type resolution.",
                        current.key
                    )),
                ),
                Kind::Missing(why) => Resolved {
                    key: format!("{}::{segment}", current.key),
                    kind: Kind::Missing(why),
                    via: current.via,
                },
            };
        }
        current
    }
    /// The first path segment: module items and imports, globs, extern crates, then preludes.
    fn lexical(
        &self,
        name: &str,
        single: bool,
        context: &Context,
        import: bool,
        depth: usize,
    ) -> Resolved {
        let key = context.key();
        // A path prefix naming a dependency cannot also be glob-imported in valid code.
        if !single
            && !self.declares(&key, name)
            && let Some(dep) = self.extern_crate(context, name)
        {
            return Resolved::new(dep, Kind::Module);
        }
        if let Some(found) = self.lookup(&key, name, Vis::Private, depth + 1) {
            return found;
        }
        if context.is_block() {
            let mut outer = context.clone();
            outer.module.pop();
            return self.lexical(name, single, &outer, import, depth + 1);
        }
        if let Some(dep) = self.extern_crate(context, name) {
            return Resolved::new(dep, Kind::Module);
        }
        if single
            && !import
            && let Some(path) = prelude(name)
        {
            return Resolved::new(path.into(), Kind::Std);
        }
        // Rust 2015 paths in `use` start at the crate root.
        if import
            && !context.module.is_empty()
            && let Some(found) = self.lookup(&context.crate_name, name, Vis::Private, depth + 1)
        {
            return found;
        }
        // Explaining an import's own failure through the globs would resolve those imports again.
        let (opaque, searched) = if import {
            Default::default()
        } else {
            self.globs(&key)
        };
        let why = if !opaque.is_empty() {
            format!(
                "{name} is not declared or imported in {key}; it may come from {}.",
                opaque.join(" or ")
            )
        } else if !searched.is_empty() {
            format!(
                "{name} is not declared or imported in {key}, and {} does not export it.",
                searched.join(", ")
            )
        } else if single {
            format!("{name} is not declared, imported or in the standard prelude of {key}.")
        } else {
            format!("{name} is neither a module of {key} nor a dependency in Cargo.toml.")
        };
        Resolved::new(name.into(), Kind::Missing(why))
    }
    /// Whether module `key` declares or explicitly imports `name`.
    fn declares(&self, key: &str, name: &str) -> bool {
        self.ensure(key);
        self.definitions
            .borrow()
            .contains_key(&format!("{key}::{name}"))
            || self
                .modules
                .borrow()
                .get(key)
                .is_some_and(|m| m.children.contains_key(name) || m.imports.contains_key(name))
    }
    /// Glob imports of a module: those whose target could not be read (with the reason),
    /// and those that were searched.
    fn globs(&self, key: &str) -> (Vec<String>, Vec<String>) {
        let (globs, context) = match self.modules.borrow().get(key) {
            Some(m) => (m.globs.clone(), m.context.clone()),
            None => return Default::default(),
        };
        let (mut opaque, mut searched) = (Vec::new(), Vec::new());
        for g in globs {
            let Some(context) = &context else { break };
            let written = format!("`use {}::*`", g.path.join("::"));
            let target = self.path(&g.path, g.leading, context, true, 0);
            let why = match &target.kind {
                Kind::Missing(why) => Some(why.clone()),
                Kind::Module if !self.ensure(&target.key) => self.unavailable(&target.key),
                _ => None,
            };
            match why {
                Some(why) => opaque.push(format!("{written} ({})", why.trim_end_matches('.'))),
                None => searched.push(written),
            }
        }
        (opaque, searched)
    }
    /// Whether the crate of `key` is a dependency whose source could not be located.
    pub fn source_missing(&self, key: &str) -> bool {
        self.unavailable(key).is_some()
    }
    fn unavailable(&self, module: &str) -> Option<String> {
        let krate = module.split("::").next().unwrap_or(module);
        let deps = self.deps.borrow();
        let c = deps.get(krate)?;
        c.root.as_ref().err().filter(|_| !c.local).cloned()
    }
    fn absent(&self, module: &str, name: &str) -> Resolved {
        let krate = module.split("::").next().unwrap_or(module);
        let why = if let Some(why) = self.unavailable(module) {
            why
        } else if self.deps.borrow().get(krate).is_none() {
            format!("Crate {krate} is not available to the analyzer.")
        } else if !self.modules.borrow().contains_key(module) {
            format!("Module {module} was not found in source.")
        } else {
            let label = self
                .label(krate)
                .map(|l| format!(" ({l})"))
                .unwrap_or_default();
            format!(
                "{name} is not declared in {module}{label}; it may be generated by a macro or selected by cfg."
            )
        };
        Resolved::new(format!("{module}::{name}"), Kind::Missing(why))
    }
    /// Look up `name` in module `key`, honoring visibility for the viewer level `level`.
    fn lookup(&self, key: &str, name: &str, level: Vis, depth: usize) -> Option<Resolved> {
        let memo = (key.to_string(), name.to_string(), level);
        if let Some(r) = self.memo.borrow().get(&memo) {
            return r.clone();
        }
        if depth > 24 || !self.active.borrow_mut().insert(memo.clone()) {
            return None;
        }
        let found = self.lookup_uncached(key, name, level, depth);
        self.active.borrow_mut().remove(&memo);
        self.memo.borrow_mut().insert(memo, found.clone());
        found
    }
    fn lookup_uncached(&self, key: &str, name: &str, level: Vis, depth: usize) -> Option<Resolved> {
        if !self.ensure(key) {
            return None;
        }
        let path = format!("{key}::{name}");
        let visible = |v: Vis| v >= level;
        if self
            .definitions
            .borrow()
            .get(&path)
            .is_some_and(|d| d.iter().any(|d| visible(d.vis)))
        {
            return Some(Resolved::new(path, Kind::Def));
        }
        let (child, import, globs, context) = {
            let modules = self.modules.borrow();
            let m = modules.get(key)?;
            (
                m.children.get(name).copied(),
                m.imports.get(name).cloned(),
                m.globs.clone(),
                m.context.clone()?,
            )
        };
        if child.is_some_and(visible) {
            return Some(Resolved::new(path, Kind::Module));
        }
        if let Some(i) = import.filter(|i| visible(i.vis)) {
            let mut r = self.path(&i.path, i.leading, &context, true, depth + 1);
            if i.vis == Vis::Public {
                r.via.insert(0, path);
            }
            return Some(r);
        }
        for g in globs.iter().filter(|g| visible(g.vis)) {
            let target = self.path(&g.path, g.leading, &context, true, depth + 1);
            let found = match target.kind {
                Kind::Module => {
                    let level = level.max(relation(&target.key, key));
                    self.lookup(&target.key, name, level, depth + 1)
                }
                Kind::Std => std_member(&target.key, name).map(|p| Resolved::new(p, Kind::Std)),
                _ => None,
            };
            if let Some(mut r) = found.filter(|r| r.kind != Kind::Module || r.key != path) {
                if g.vis == Vis::Public {
                    r.via.insert(0, path);
                }
                return Some(r);
            }
        }
        None
    }
    /// Manual impls of `trait_name` for the definition `key`, scanning its crate if needed.
    pub fn impls(&self, key: &str, trait_name: &str, scan: bool) -> Vec<Rc<Impl>> {
        let krate = key.split("::").next().unwrap_or(key).to_string();
        let find = || {
            self.resolve_impls(&krate);
            self.impls
                .borrow()
                .get(&krate)
                .and_then(|c| c.by_key.get(key))
                .map(|v| {
                    v.iter()
                        .filter(|i| i.trait_name == trait_name)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        let found: Vec<Rc<Impl>> = find();
        let ident = key.rsplit("::").next().unwrap_or(key).to_string();
        let local = self.deps.borrow().get(&krate).is_none_or(|c| c.local);
        let fresh = self
            .impls
            .borrow_mut()
            .entry(krate.clone())
            .or_default()
            .scanned
            .insert((ident.clone(), trait_name.into()));
        if !found.is_empty() || local || !fresh || !scan {
            return found;
        }
        self.scan(&krate, &ident, trait_name);
        find()
    }
    fn resolve_impls(&self, krate: &str) {
        loop {
            let pending = match self.impls.borrow_mut().get_mut(krate) {
                Some(c) => std::mem::take(&mut c.pending),
                None => return,
            };
            if pending.is_empty() {
                return;
            }
            for i in pending {
                let r = self.resolve(&i.self_ty.path, &i.context);
                if r.kind == Kind::Def {
                    self.impls
                        .borrow_mut()
                        .entry(krate.into())
                        .or_default()
                        .by_key
                        .entry(r.key)
                        .or_default()
                        .push(i);
                }
            }
        }
    }
    /// Load the modules of a dependency whose files may declare the impl.
    fn scan(&self, krate: &str, ident: &str, trait_name: &str) {
        let root = self
            .deps
            .borrow()
            .get(krate)
            .and_then(|c| c.root.clone().ok());
        let Some(src) = root
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
        else {
            return;
        };
        let cached = self
            .impls
            .borrow()
            .get(krate)
            .and_then(|c| c.sources.clone());
        let sources = cached.unwrap_or_else(|| {
            // Very large generated crates are not searched; their types stay unknown.
            let files: Vec<PathBuf> = walkdir::WalkDir::new(&src)
                .into_iter()
                .filter_entry(|e| {
                    !matches!(
                        e.file_name().to_str(),
                        Some("tests" | "benches" | "examples")
                    )
                })
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|e| e == "rs"))
                .map(|e| e.into_path())
                .take(SCAN_LIMIT + 1)
                .collect();
            let sources = Rc::new(if files.len() > SCAN_LIMIT {
                Vec::new()
            } else {
                files
                    .into_iter()
                    .filter_map(|f| Some((std::fs::read_to_string(&f).ok()?, f)))
                    .map(|(text, f)| (f, text))
                    .collect()
            });
            self.impls
                .borrow_mut()
                .entry(krate.into())
                .or_default()
                .sources = Some(sources.clone());
            sources
        });
        for (file, text) in sources.iter() {
            if self.files.borrow().contains(file)
                || !(text.contains("impl")
                    && text.contains(trait_name)
                    && contains_word(text, ident))
            {
                continue;
            }
            let Ok(relative) = file.strip_prefix(&src) else {
                continue;
            };
            let mut module: Vec<String> = relative
                .with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            if module.last().is_some_and(|m| m == "mod" || m == "lib") {
                module.pop();
            }
            let key = std::iter::once(krate.to_string())
                .chain(module)
                .collect::<Vec<_>>()
                .join("::");
            self.ensure_known_path(&key);
        }
    }
    fn ensure_known_path(&self, key: &str) {
        let mut prefix = String::new();
        for part in key.split("::") {
            if !prefix.is_empty() {
                prefix.push_str("::");
            }
            prefix.push_str(part);
            if !self.ensure(&prefix) {
                return;
            }
        }
    }
}
/// Dependencies with more source files than this are not searched for manual impls.
const SCAN_LIMIT: usize = 3000;
fn unknown_crate(name: &str) -> Resolved {
    Resolved::new(
        name.into(),
        Kind::Missing(format!(
            "{name} is not a dependency of this crate in Cargo.toml, so its source cannot be read."
        )),
    )
}
fn parent(key: &str) -> String {
    key.rsplit_once("::").map_or(key, |(p, _)| p).into()
}
/// The visibility a glob import from `importer` needs to see items of `target`.
fn relation(target: &str, importer: &str) -> Vis {
    if importer == target || importer.starts_with(&format!("{target}::")) {
        Vis::Private
    } else if target.split("::").next() == importer.split("::").next() {
        Vis::Crate
    } else {
        Vis::Public
    }
}
fn vis(v: &syn::Visibility) -> Vis {
    match v {
        syn::Visibility::Public(_) => Vis::Public,
        syn::Visibility::Restricted(_) => Vis::Crate,
        syn::Visibility::Inherited => Vis::Private,
    }
}
fn attributes(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}
fn is_test(a: &syn::Attribute) -> bool {
    a.path().is_ident("cfg") && a.parse_args::<syn::Ident>().is_ok_and(|i| i == "test")
}
pub(crate) fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs
        .iter()
        .find(|a| a.path().is_ident("path"))
        .and_then(|a| match &a.meta {
            syn::Meta::NameValue(syn::MetaNameValue {
                value:
                    syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }),
                ..
            }) => Some(s.value()),
            _ => None,
        })
}
/// `impl PartialEq for T` or `impl PartialEq<T> for T`; other right-hand sides do not count.
fn self_rhs(arguments: &syn::PathArguments, ty: &syn::TypePath) -> bool {
    match arguments {
        syn::PathArguments::AngleBracketed(a) => a.args.iter().all(|a| match a {
            syn::GenericArgument::Type(Type::Path(p)) => {
                p.path.is_ident("Self")
                    || p.path.segments.last().map(|s| &s.ident)
                        == ty.path.segments.last().map(|s| &s.ident)
            }
            _ => false,
        }),
        _ => true,
    }
}
fn data<'a>(
    fields: impl Iterator<Item = &'a syn::Field>,
    generics: &syn::Generics,
    attrs: &[syn::Attribute],
) -> Definition {
    let derives = derives(attrs);
    Definition::Data {
        fields: fields.map(|f| f.ty.clone()).collect(),
        generics: generics.clone(),
        derives,
    }
}
/// Derives, including those under `cfg_attr`: a parameter that compiles has the enabled ones.
fn derives(attrs: &[syn::Attribute]) -> Derives {
    let mut d = Derives::default();
    let mut visit = |paths: syn::punctuated::Punctuated<syn::Path, syn::Token![,]>| {
        for p in paths {
            match p.segments.last().map(|s| s.ident.to_string()).as_deref() {
                Some("Clone") => d.clone = true,
                Some("PartialEq") => d.eq = true,
                Some("Copy") => d.copy = true,
                _ => {}
            }
        }
    };
    for a in attrs {
        if a.path().is_ident("derive") {
            if let Ok(p) = a.parse_args_with(syn::punctuated::Punctuated::parse_terminated) {
                visit(p);
            }
        } else if a.path().is_ident("cfg_attr")
            && let Ok(metas) = a.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
        {
            for m in metas.iter().skip(1) {
                if let syn::Meta::List(l) = m
                    && l.path.is_ident("derive")
                    && let Ok(p) = l.parse_args_with(syn::punctuated::Punctuated::parse_terminated)
                {
                    visit(p);
                }
            }
        }
    }
    d
}
fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(i, _)| {
        let ok = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        ok(text[..i].chars().next_back()) && ok(text[i + word.len()..].chars().next())
    })
}
/// Standard prelude types and primitives, used unless a module item, import or glob shadows them.
fn prelude(name: &str) -> Option<&'static str> {
    Some(match name {
        "String" => "std::string::String",
        "Vec" => "std::vec::Vec",
        "Option" => "std::option::Option",
        "Result" => "std::result::Result",
        "Box" => "std::boxed::Box",
        "bool" => "std::primitive::bool",
        "char" => "std::primitive::char",
        "str" => "std::primitive::str",
        "u8" => "std::primitive::u8",
        "u16" => "std::primitive::u16",
        "u32" => "std::primitive::u32",
        "u64" => "std::primitive::u64",
        "u128" => "std::primitive::u128",
        "usize" => "std::primitive::usize",
        "i8" => "std::primitive::i8",
        "i16" => "std::primitive::i16",
        "i32" => "std::primitive::i32",
        "i64" => "std::primitive::i64",
        "i128" => "std::primitive::i128",
        "isize" => "std::primitive::isize",
        "f32" => "std::primitive::f32",
        "f64" => "std::primitive::f64",
        _ => return None,
    })
}
/// A name imported by a glob from a standard-library module, if the analyzer knows that type.
fn std_member(module: &str, name: &str) -> Option<String> {
    let path = format!("{module}::{name}");
    crate::types::std_known(&path).then_some(path)
}
