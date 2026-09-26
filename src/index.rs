use crate::SourceFile;
use std::collections::BTreeMap;
use syn::{Item, Type, UseTree};

#[derive(Clone)]
pub(crate) struct Context {
    pub crate_name: String,
    pub module: Vec<String>,
    pub crate_aliases: BTreeMap<String, String>,
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
}
#[derive(Clone)]
pub(crate) enum Definition {
    Data {
        fields: Vec<Type>,
        generics: syn::Generics,
        clone: bool,
        eq: bool,
        copy: bool,
    },
    Alias {
        ty: Box<Type>,
        generics: syn::Generics,
    },
}
#[derive(Clone)]
pub(crate) struct Named {
    pub definition: Definition,
    pub context: Context,
}
#[derive(Default)]
pub(crate) struct Scope {
    pub imports: BTreeMap<String, String>,
    pub globs: Vec<String>,
}
#[derive(Default)]
pub(crate) struct Index {
    pub definitions: BTreeMap<String, Vec<Named>>,
    pub scopes: BTreeMap<String, Scope>,
    pub manual_traits: BTreeMap<String, Vec<String>>,
}
pub(crate) struct Parsed {
    pub source: SourceFile,
    pub ast: syn::File,
    pub context: Context,
}

impl Index {
    pub fn collect(&mut self, items: &[Item], context: &Context) {
        for item in items {
            match item {
                Item::Use(item) => self.import(&item.tree, String::new(), context),
                Item::Mod(m) => {
                    if let Some((_, items)) = &m.content {
                        self.collect(items, &context.child(m.ident.to_string()));
                    }
                }
                Item::Struct(s) => self.add(
                    &s.ident.to_string(),
                    Definition::Data {
                        fields: s.fields.iter().map(|f| f.ty.clone()).collect(),
                        generics: s.generics.clone(),
                        clone: derived(&s.attrs, "Clone"),
                        eq: derived(&s.attrs, "PartialEq"),
                        copy: derived(&s.attrs, "Copy"),
                    },
                    context,
                ),
                Item::Enum(e) => self.add(
                    &e.ident.to_string(),
                    Definition::Data {
                        fields: e
                            .variants
                            .iter()
                            .flat_map(|v| v.fields.iter().map(|f| f.ty.clone()))
                            .collect(),
                        generics: e.generics.clone(),
                        clone: derived(&e.attrs, "Clone"),
                        eq: derived(&e.attrs, "PartialEq"),
                        copy: derived(&e.attrs, "Copy"),
                    },
                    context,
                ),
                Item::Type(t) => self.add(
                    &t.ident.to_string(),
                    Definition::Alias {
                        ty: t.ty.clone(),
                        generics: t.generics.clone(),
                    },
                    context,
                ),
                _ => {}
            }
        }
    }
    pub fn collect_impls(&mut self, items: &[Item], context: &Context) {
        for item in items {
            match item {
                Item::Impl(i) => {
                    if let (Some((_, tr, _)), Type::Path(ty)) = (&i.trait_, i.self_ty.as_ref())
                        && let Some(last) = tr.segments.last()
                    {
                        let name = last.ident.to_string();
                        if ["Clone", "PartialEq", "Copy"].contains(&name.as_str()) {
                            let key = self.resolve(&ty.path, context);
                            self.manual_traits.entry(key).or_default().push(name);
                        }
                    }
                }
                Item::Mod(m) => {
                    if let Some((_, items)) = &m.content {
                        self.collect_impls(items, &context.child(m.ident.to_string()));
                    }
                }
                _ => {}
            }
        }
    }
    fn add(&mut self, name: &str, definition: Definition, context: &Context) {
        self.definitions
            .entry(format!("{}::{name}", context.key()))
            .or_default()
            .push(Named {
                definition,
                context: context.clone(),
            });
    }
    fn import(&mut self, tree: &UseTree, prefix: String, context: &Context) {
        match tree {
            UseTree::Path(p) => self.import(&p.tree, format!("{prefix}{}::", p.ident), context),
            UseTree::Group(g) => {
                for item in &g.items {
                    self.import(item, prefix.clone(), context);
                }
            }
            UseTree::Name(n) => {
                let (name, path) = if n.ident == "self" {
                    let path = prefix.trim_end_matches("::").to_string();
                    (
                        path.rsplit("::").next().unwrap_or_default().to_string(),
                        path,
                    )
                } else {
                    (n.ident.to_string(), format!("{prefix}{}", n.ident))
                };
                let path = absolute(&path, context);
                self.scopes
                    .entry(context.key())
                    .or_default()
                    .imports
                    .insert(name, path);
            }
            UseTree::Rename(n) => {
                let path = absolute(&format!("{prefix}{}", n.ident), context);
                self.scopes
                    .entry(context.key())
                    .or_default()
                    .imports
                    .insert(n.rename.to_string(), path);
            }
            UseTree::Glob(_) => {
                self.scopes
                    .entry(context.key())
                    .or_default()
                    .globs
                    .push(absolute(prefix.trim_end_matches("::"), context));
            }
        }
    }
    pub fn resolve(&self, path: &syn::Path, context: &Context) -> String {
        let raw = path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");
        let key = self.resolve_name(&raw, context, 0);
        let (first, tail) = key.split_once("::").unwrap_or((&key, ""));
        let key = if let Some(name) = context.crate_aliases.get(first) {
            if tail.is_empty() {
                name.clone()
            } else {
                format!("{name}::{tail}")
            }
        } else {
            key
        };
        let rooted = format!("{}::{key}", context.crate_name);
        if self.definitions.contains_key(&rooted) {
            self.follow_export(&rooted, 0)
        } else {
            self.follow_export(&key, 0)
        }
    }
    fn resolve_name(&self, raw: &str, context: &Context, depth: usize) -> String {
        if depth > 12 {
            return format!("unresolved::{raw}");
        }
        if raw.starts_with("crate::") || raw.starts_with("self::") || raw.starts_with("super::") {
            return self.follow_export(&absolute(raw, context), depth + 1);
        }
        let (first, tail) = raw.split_once("::").unwrap_or((raw, ""));
        if let Some(scope) = self.scopes.get(&context.key()) {
            if let Some(import) = scope.imports.get(first) {
                let path = if tail.is_empty() {
                    import.clone()
                } else {
                    format!("{import}::{tail}")
                };
                return self.follow_export(&path, depth + 1);
            }
            let candidates: Vec<_> = scope
                .globs
                .iter()
                .map(|g| self.follow_export(&format!("{g}::{raw}"), depth + 1))
                .filter(|key| self.definitions.contains_key(key))
                .collect();
            if candidates.len() == 1 {
                return candidates[0].clone();
            }
            if !candidates.is_empty() {
                return format!("ambiguous::{raw}");
            }
        }
        let local = format!("{}::{raw}", context.key());
        if self.definitions.contains_key(&local) {
            return local;
        }
        // Standard prelude types may be shadowed by a wildcard import we cannot inspect.
        let uncertain_glob = self.scopes.get(&context.key()).is_some_and(|s| {
            s.globs.iter().any(|g| {
                !g.starts_with(&context.crate_name)
                    && !g.starts_with("std::")
                    && !g.starts_with("core::")
            })
        });
        if !raw.contains("::")
            && !uncertain_glob
            && let Some(path) = prelude(raw)
        {
            return path.to_string();
        }
        self.follow_export(raw, depth + 1)
    }
    fn follow_export(&self, path: &str, depth: usize) -> String {
        if depth > 12 {
            return format!("unresolved::{path}");
        }
        if self.definitions.contains_key(path) {
            return path.into();
        }
        if let Some((scope, name)) = path.rsplit_once("::")
            && let Some(import) = self.scopes.get(scope).and_then(|s| s.imports.get(name))
        {
            return self.follow_export(import, depth + 1);
        }
        path.into()
    }
}
fn absolute(raw: &str, context: &Context) -> String {
    let mut parts = raw.split("::").collect::<Vec<_>>();
    let mut base: Vec<String> = Vec::new();
    match parts.first().copied() {
        Some("crate") => {
            base.push(context.crate_name.clone());
            parts.remove(0);
        }
        Some("self") => {
            base.push(context.crate_name.clone());
            base.extend(context.module.clone());
            parts.remove(0);
        }
        Some("super") => {
            base.push(context.crate_name.clone());
            base.extend(context.module.clone());
            while parts.first() == Some(&"super") {
                if base.len() > 1 {
                    base.pop();
                }
                parts.remove(0);
            }
        }
        _ => {}
    }
    base.extend(parts.iter().map(|p| p.to_string()));
    base.join("::")
}
fn derived(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("derive"))
        .any(|a| {
            a.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|paths| {
                paths
                    .iter()
                    .any(|p| p.segments.last().is_some_and(|p| p.ident == name))
            })
        })
}
fn prelude(name: &str) -> Option<&'static str> {
    match name {
        "String" => Some("std::string::String"),
        "Vec" => Some("std::vec::Vec"),
        "Option" => Some("core::option::Option"),
        "Result" => Some("core::result::Result"),
        "Box" => Some("std::boxed::Box"),
        "bool" => Some("core::primitive::bool"),
        "char" => Some("core::primitive::char"),
        "str" => Some("core::primitive::str"),
        "u8" => Some("core::primitive::u8"),
        "u16" => Some("core::primitive::u16"),
        "u32" => Some("core::primitive::u32"),
        "u64" => Some("core::primitive::u64"),
        "u128" => Some("core::primitive::u128"),
        "usize" => Some("core::primitive::usize"),
        "i8" => Some("core::primitive::i8"),
        "i16" => Some("core::primitive::i16"),
        "i32" => Some("core::primitive::i32"),
        "i64" => Some("core::primitive::i64"),
        "i128" => Some("core::primitive::i128"),
        "isize" => Some("core::primitive::isize"),
        "f32" => Some("core::primitive::f32"),
        "f64" => Some("core::primitive::f64"),
        _ => None,
    }
}
