//! Clone + PartialEq contracts and shared-mutation signals of parameter types.
use crate::index::{Context, Definition, Index, Kind, Named};
use crate::{Config, Effect, Stability};
use quote::ToTokens;
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use syn::{GenericArgument, PathArguments, Type, TypeParamBound};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Proof {
    Yes,
    No,
    Unknown,
}
impl Proof {
    fn and(self, other: Proof) -> Proof {
        match (self, other) {
            (Proof::No, _) | (_, Proof::No) => Proof::No,
            (Proof::Unknown, _) | (_, Proof::Unknown) => Proof::Unknown,
            _ => Proof::Yes,
        }
    }
}
/// How informative a reason is; the highest-ranked reason explains a combined verdict.
const PLAIN: u8 = 0;
const NOTE: u8 = 1;
const UNKNOWN: u8 = 2;
const SHARED: u8 = 3;
const INVALID: u8 = 4;
const VALUE: &str = "Compared by value through Clone + PartialEq.";

#[derive(Clone)]
pub(crate) struct Shape {
    clone: Proof,
    copy: Proof,
    eq: Proof,
    interior: bool,
    shared: bool,
    rank: u8,
    reason: String,
    advice: Option<String>,
    ty: Option<String>,
    cut: bool,
}
/// A parameter verdict with the type path that decided it, when there is one.
pub(crate) struct Verdict {
    pub stability: Stability,
    pub effect: Effect,
    pub reason: String,
    pub advice: String,
    pub resolved: Option<String>,
}
impl Shape {
    fn value() -> Self {
        Self {
            clone: Proof::Yes,
            copy: Proof::Unknown,
            eq: Proof::Yes,
            interior: false,
            shared: false,
            rank: PLAIN,
            reason: VALUE.into(),
            advice: None,
            ty: None,
            cut: false,
        }
    }
    fn copy() -> Self {
        Self {
            copy: Proof::Yes,
            ..Self::value()
        }
    }
    fn unknown(reason: impl Into<String>, advice: impl Into<String>) -> Self {
        Self {
            clone: Proof::Unknown,
            eq: Proof::Unknown,
            rank: UNKNOWN,
            reason: reason.into(),
            advice: Some(advice.into()),
            ..Self::value()
        }
    }
    fn invalid(reason: impl Into<String>) -> Self {
        Self {
            clone: Proof::No,
            copy: Proof::No,
            eq: Proof::No,
            rank: INVALID,
            reason: reason.into(),
            ..Self::value()
        }
    }
    fn note(mut self, rank: u8, reason: impl Into<String>, ty: Option<&str>) -> Self {
        self.rank = rank;
        self.reason = reason.into();
        self.advice = None;
        self.ty = ty.map(str::to_string);
        self
    }
    /// Replace an explanation that no longer matches the proofs after a wrapper supplied Clone.
    fn fix(mut self, wrapper: &str) -> Self {
        let stale = match self.rank {
            INVALID => self.clone != Proof::No && self.eq != Proof::No,
            SHARED => !self.shared,
            UNKNOWN => self.clone == Proof::Yes && self.eq == Proof::Yes,
            _ => false,
        };
        if stale {
            self.rank = PLAIN;
            self.advice = None;
            self.reason = match &self.ty {
                Some(ty) => {
                    format!("{wrapper} supplies Clone; {ty} values are compared with PartialEq.")
                }
                None => VALUE.into(),
            };
        }
        self
    }
    pub fn verdict(&self) -> Verdict {
        let (stability, effect, advice) = if self.clone == Proof::No || self.eq == Proof::No {
            (
                Stability::Incompatible,
                Effect::InvalidParameter,
                "Use a value with Clone + PartialEq, or make the composable explicitly no_skip if unconditional execution is intended.",
            )
        } else if self.shared {
            (
                Stability::Unstable,
                Effect::SharedMutation,
                "Pass an immutable value snapshot or a Cranpose snapshot-state handle. A cloned shared pointer does not retain the old inner value.",
            )
        } else if self.clone == Proof::Unknown || self.eq == Proof::Unknown {
            (
                Stability::Unknown,
                Effect::Unproven,
                "Declare the type in stable_types in cranpose-stability.toml if it compares by value.",
            )
        } else {
            (
                Stability::Stable,
                Effect::ValueComparison,
                "Equal values can skip this parameter check; this does not measure runtime cost or guarantee that the whole composable skips.",
            )
        };
        let advice = match (&self.advice, stability) {
            (Some(a), Stability::Unknown | Stability::Unstable) => a.clone(),
            _ => advice.into(),
        };
        Verdict {
            stability,
            effect,
            reason: self.reason.clone(),
            advice,
            resolved: self.ty.clone(),
        }
    }
}
#[derive(Clone)]
struct Binding {
    ty: Type,
    context: Context,
    env: Rc<Env>,
}
/// Type parameters in scope: bound arguments of a definition, or the composable's own bounds.
#[derive(Default)]
struct Env {
    bindings: BTreeMap<String, Binding>,
    bounds: BTreeMap<String, Vec<TypeParamBound>>,
}
struct Cx<'a> {
    index: &'a Index,
    config: &'a Config,
    /// Inside fields only the shared-mutation signal matters, so manual impls are not searched.
    fields: std::cell::Cell<bool>,
}

pub(crate) fn shape(
    ty: &Type,
    generics: &syn::Generics,
    context: &Context,
    index: &Index,
    config: &Config,
) -> Shape {
    let mut env = Env::default();
    for p in &generics.params {
        if let syn::GenericParam::Type(p) = p {
            env.bounds
                .insert(p.ident.to_string(), p.bounds.iter().cloned().collect());
        }
    }
    if let Some(w) = &generics.where_clause {
        for p in &w.predicates {
            if let syn::WherePredicate::Type(p) = p
                && let Type::Path(t) = &p.bounded_ty
                && t.path.segments.len() == 1
            {
                env.bounds
                    .entry(t.path.segments[0].ident.to_string())
                    .or_default()
                    .extend(p.bounds.iter().cloned());
            }
        }
    }
    Cx {
        index,
        config,
        fields: Default::default(),
    }
    .inspect(ty, context, &Rc::new(env), &mut BTreeSet::new(), 0)
}
fn combines(values: Vec<Shape>) -> Shape {
    let mut result = Shape::copy();
    let mut best: Option<Shape> = None;
    for s in values {
        result.clone = result.clone.and(s.clone);
        result.eq = result.eq.and(s.eq);
        result.copy = result.copy.and(s.copy);
        result.interior |= s.interior;
        result.shared |= s.shared;
        result.cut |= s.cut;
        if best.as_ref().is_none_or(|b| s.rank > b.rank) {
            best = Some(s);
        }
    }
    if let Some(b) = best {
        result.rank = b.rank;
        result.reason = b.reason;
        result.advice = b.advice;
        result.ty = b.ty;
    }
    result
}
fn text(ty: &impl ToTokens) -> String {
    ty.to_token_stream()
        .to_string()
        .replace(" :: ", "::")
        .replace(" < ", "<")
        .replace("< ", "<")
        .replace(" >", ">")
        .replace(" ,", ",")
        .replace("& '", "&'")
}
fn type_args(segment: Option<&syn::PathSegment>) -> Vec<Type> {
    match segment.map(|s| &s.arguments) {
        Some(PathArguments::AngleBracketed(a)) => a
            .args
            .iter()
            .filter_map(|a| match a {
                GenericArgument::Type(t) => Some(t.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
fn is_fn_trait(bound: &TypeParamBound) -> bool {
    matches!(bound, TypeParamBound::Trait(t) if t.path.segments.last().is_some_and(|s| ["Fn", "FnMut", "FnOnce"].iter().any(|n| s.ident == n)))
}
fn stable_advice(key: &str) -> String {
    format!("If {key} compares by value, add \"{key}\" to stable_types in cranpose-stability.toml.")
}

impl Cx<'_> {
    fn inspect(
        &self,
        ty: &Type,
        context: &Context,
        env: &Rc<Env>,
        visiting: &mut BTreeSet<String>,
        depth: usize,
    ) -> Shape {
        if depth > 40 {
            return Shape::unknown(
                "Type analysis reached its nesting limit.",
                "Simplify the parameter type or declare it in stable_types in cranpose-stability.toml.",
            );
        }
        let mut recurse = |t: &Type| self.inspect(t, context, env, visiting, depth + 1);
        match ty {
            Type::Paren(t) => recurse(&t.elem),
            Type::Group(t) => recurse(&t.elem),
            Type::Tuple(t) => combines(t.elems.iter().map(recurse).collect()),
            Type::Array(t) => recurse(&t.elem),
            Type::Slice(t) => {
                let s = recurse(&t.elem);
                Shape {
                    clone: Proof::No,
                    copy: Proof::No,
                    ..s
                }
                .note(
                    INVALID,
                    "An unsized slice cannot be stored by value in ParamState.",
                    None,
                )
            }
            Type::Reference(r) => {
                let mut s = recurse(&r.elem);
                if r.mutability.is_some() {
                    return Shape {
                        clone: Proof::No,
                        copy: Proof::No,
                        ..s
                    }
                    .note(
                        INVALID,
                        "Mutable references do not implement Clone, which a skippable parameter slot requires.",
                        None,
                    );
                }
                s.clone = Proof::Yes;
                s.copy = Proof::Yes;
                if s.interior || s.shared {
                    s.shared = true;
                    s = s.note(
                        SHARED,
                        "This reference aliases interior-mutable data. The retained reference can see the new value on both sides of the comparison.",
                        None,
                    );
                }
                s.fix("The reference")
            }
            Type::Ptr(_) => Shape::unknown(
                "Raw pointers compare addresses. Pointee mutation is not observed by this comparison.",
                "Pass an owned value or an identifier instead of a raw pointer.",
            ),
            Type::BareFn(_) => {
                Shape::copy().note(NOTE, "Function pointers compare by address.", None)
            }
            Type::TraitObject(t) if t.bounds.iter().any(is_fn_trait) => Shape::invalid(
                "Closures behind dyn Fn do not implement PartialEq, which a skippable parameter slot requires.",
            ),
            Type::TraitObject(t) => Shape::unknown(
                format!(
                    "The trait object {} has no statically known Clone + PartialEq contract.",
                    text(t)
                ),
                "Use a concrete type or a generic parameter bounded by Clone + PartialEq.",
            ),
            Type::ImplTrait(_) => Shape::unknown(
                "A nested impl Trait type has no nameable Clone + PartialEq contract.",
                "Use a named generic parameter bounded by Clone + PartialEq.",
            ),
            Type::Macro(m) => Shape::unknown(
                format!(
                    "{}! expands to a type only after macro expansion.",
                    text(&m.mac.path)
                ),
                "Use the expanded type directly, or declare it in stable_types in cranpose-stability.toml.",
            ),
            Type::Path(p) => self.path(p, ty, context, env, visiting, depth),
            _ => Shape::unknown(
                format!("{} requires compiler type resolution.", text(ty)),
                "Use a concrete named type.",
            ),
        }
    }
    fn path(
        &self,
        p: &syn::TypePath,
        ty: &Type,
        context: &Context,
        env: &Rc<Env>,
        visiting: &mut BTreeSet<String>,
        depth: usize,
    ) -> Shape {
        let first = p
            .path
            .segments
            .first()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        let generic = env.bindings.contains_key(&first) || env.bounds.contains_key(&first);
        if p.qself.is_some() || (generic && p.path.segments.len() > 1) {
            return Shape::unknown(
                format!(
                    "{} is an associated type; it needs compiler type resolution.",
                    text(ty)
                ),
                "Use a concrete type or a generic parameter bounded by Clone + PartialEq.",
            );
        }
        if p.path.segments.len() == 1 {
            if let Some(b) = env.bindings.get(&first) {
                return self.inspect(&b.ty, &b.context, &b.env, visiting, depth + 1);
            }
            if let Some(bounds) = env.bounds.get(&first) {
                return self.generic(&first, bounds, context);
            }
        }
        let resolved = self.index.resolve(&p.path, context);
        let key = resolved.key.clone();
        if self
            .config
            .stable_types
            .iter()
            .any(|s| s == &key || resolved.via.contains(s))
        {
            return Shape::value().note(
                NOTE,
                format!(
                    "User-declared stability contract for {key}; runtime behavior is not verified."
                ),
                Some(&key),
            );
        }
        let args = type_args(p.path.segments.last());
        if cranpose_handle(&key) {
            return Shape::copy().note(
                NOTE,
                "Cranpose state handle: identity is compared and snapshot reads track changes separately.",
                Some(&key),
            );
        }
        match resolved.kind {
            Kind::Std => self.std(&key, &args, context, env, visiting, depth),
            Kind::Def => self.definition(&key, &args, context, env, visiting, depth),
            Kind::Module => Shape::unknown(
                format!("{key} names a module, not a type."),
                "Check the parameter type's path.",
            ),
            Kind::Missing(why) => {
                let advice = if !key.contains("::") {
                    format!(
                        "Import {key} or write its full path, as the compiler requires; the analyzer follows the same imports."
                    )
                } else if self.index.source_missing(&key) {
                    format!(
                        "Run `cargo fetch` so the dependency source is on disk, or add \"{key}\" to stable_types in cranpose-stability.toml if it compares by value."
                    )
                } else {
                    stable_advice(&key)
                };
                let mut s = Shape::unknown(why, advice);
                s.ty = Some(key);
                s
            }
        }
    }
    /// A generic parameter of the composable: only its declared bounds are known.
    fn generic(&self, name: &str, bounds: &[TypeParamBound], context: &Context) -> Shape {
        let mut traits = BTreeSet::new();
        self.traits(bounds, context, &mut traits, 0);
        let has = |t: &str| traits.contains(t);
        let clone = has("Clone") || has("Copy");
        let eq = has("PartialEq") || has("Eq");
        let mut s = if clone && eq {
            Shape::value().note(
                PLAIN,
                format!("Generic parameter {name} is bounded by Clone + PartialEq."),
                None,
            )
        } else {
            let missing = [(!clone).then_some("Clone"), (!eq).then_some("PartialEq")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" + ");
            Shape::unknown(
                format!("Generic parameter {name} has no {missing} bound."),
                format!(
                    "Add {name}: Clone + PartialEq to the composable's bounds; Cranpose clones and compares skippable parameters."
                ),
            )
        };
        if has("Copy") {
            s.copy = Proof::Yes;
        }
        s
    }
    /// Trait names implied by bounds, following supertraits of traits declared in source.
    fn traits(
        &self,
        bounds: &[TypeParamBound],
        context: &Context,
        out: &mut BTreeSet<String>,
        depth: usize,
    ) {
        for b in bounds {
            let TypeParamBound::Trait(t) = b else {
                continue;
            };
            let Some(last) = t.path.segments.last() else {
                continue;
            };
            if !out.insert(last.ident.to_string()) || depth > 8 {
                continue;
            }
            let r = self.index.resolve(&t.path, context);
            if r.kind == Kind::Def {
                for named in self.index.definitions(&r.key) {
                    if let Definition::Trait { supertraits } = &named.definition {
                        self.traits(supertraits, &named.context, out, depth + 1);
                    }
                }
            }
        }
    }
    fn definition(
        &self,
        key: &str,
        args: &[Type],
        context: &Context,
        env: &Rc<Env>,
        visiting: &mut BTreeSet<String>,
        depth: usize,
    ) -> Shape {
        let defs = self.index.definitions(key);
        let plain: Vec<_> = defs.iter().filter(|d| !d.cfg).cloned().collect();
        let chosen = if plain.is_empty() { defs } else { plain };
        let derives: Vec<_> = chosen
            .iter()
            .map(|d| match &d.definition {
                Definition::Data { derives, .. } => Some(*derives),
                _ => None,
            })
            .collect();
        if derives.windows(2).any(|w| w[0] != w[1]) {
            return Shape::unknown(
                format!(
                    "{key} has {} cfg-dependent declarations that disagree on Clone/PartialEq/Copy derives.",
                    chosen.len()
                ),
                stable_advice(key),
            );
        }
        let cacheable = args.is_empty();
        let cache = format!("{}{key}", if self.fields.get() { "~" } else { "" });
        if cacheable && let Some(s) = self.index.shapes.borrow().get(&cache) {
            return s.clone();
        }
        let shapes: Vec<_> = chosen
            .iter()
            .map(|named| self.named(key, named, args, context, env, visiting, depth))
            .collect();
        let s = if shapes.len() == 1 {
            shapes.into_iter().next().unwrap_or_else(Shape::value)
        } else {
            combines(shapes)
        };
        if cacheable && !s.cut {
            self.index.shapes.borrow_mut().insert(cache, s.clone());
        }
        s
    }
    #[allow(clippy::too_many_arguments)]
    fn named(
        &self,
        key: &str,
        named: &Named,
        args: &[Type],
        context: &Context,
        env: &Rc<Env>,
        visiting: &mut BTreeSet<String>,
        depth: usize,
    ) -> Shape {
        let krate = key.split("::").next().unwrap_or(key);
        let label = self
            .index
            .label(krate)
            .map(|l| format!(" ({l})"))
            .unwrap_or_default();
        let generics = match &named.definition {
            Definition::Data { generics, .. } | Definition::Alias { generics, .. } => generics,
            Definition::Trait { .. } => {
                return Shape::unknown(
                    format!("{key} is a trait, and a bare trait object has no PartialEq contract."),
                    "Use a concrete type or a generic parameter bounded by Clone + PartialEq.",
                );
            }
        };
        let mut bound = Env::default();
        let params: Vec<_> = generics.type_params().collect();
        for (i, param) in params.iter().enumerate() {
            if let Some(t) = args.get(i).or(param.default.as_ref()) {
                let (context, env) = if args.get(i).is_some() {
                    (context.clone(), env.clone())
                } else {
                    (named.context.clone(), Rc::new(Env::default()))
                };
                bound.bindings.insert(
                    param.ident.to_string(),
                    Binding {
                        ty: t.clone(),
                        context,
                        env,
                    },
                );
            }
        }
        let bound = Rc::new(bound);
        let entered = visiting.insert(key.to_string());
        let result = match &named.definition {
            Definition::Alias { ty, .. } if !entered => Shape::unknown(
                format!("Type alias {key} refers to itself."),
                "Break the alias cycle.",
            ),
            Definition::Alias { ty, .. } => {
                self.inspect(ty, &named.context, &bound, visiting, depth + 1)
            }
            Definition::Data {
                fields, derives, ..
            } => {
                // Arguments decide derived impls: derive(Clone) requires every type parameter to be Clone.
                let arg_shapes: Vec<Shape> = params
                    .iter()
                    .map(|p| match bound.bindings.get(&p.ident.to_string()) {
                        Some(b) => self.inspect(&b.ty, &b.context, &b.env, visiting, depth + 1),
                        None => Shape::value(),
                    })
                    .collect();
                let outer = self.fields.replace(true);
                let fields: Vec<Shape> = if entered {
                    fields
                        .iter()
                        .map(|f| self.inspect(f, &named.context, &bound, visiting, depth + 1))
                        .collect()
                } else {
                    Vec::new()
                };
                self.fields.set(outer);
                let mut s = self.data(key, &label, *derives, &params, &arg_shapes, &fields);
                s.cut |= !entered;
                s
            }
            Definition::Trait { .. } => Shape::value(),
        };
        if entered {
            visiting.remove(key);
        }
        result
    }
    /// Combine derived or manual trait evidence for a struct, enum or union.
    fn data(
        &self,
        key: &str,
        label: &str,
        derives: crate::index::Derives,
        params: &[&syn::TypeParam],
        args: &[Shape],
        fields: &[Shape],
    ) -> Shape {
        // Derives bound every type parameter by the trait; a field that lacks it cannot compile.
        let evidence = |trait_name: &str, derived: bool, f: fn(&Shape) -> Proof| -> Evidence {
            if derived {
                if let Some(bad) = fields.iter().find(|s| f(s) == Proof::No) {
                    return Evidence::of(Proof::No, false, Some(bad));
                }
                let blame = args.iter().find(|a| f(a) != Proof::Yes);
                let proof = args.iter().fold(Proof::Yes, |p, a| p.and(f(a)));
                return Evidence::of(proof, false, blame);
            }
            let impls =
                self.index
                    .impls(key, trait_name, trait_name != "Copy" && !self.fields.get());
            let Some(i) = impls.first() else {
                let assumed = if self.fields.get() && trait_name != "Copy" {
                    Proof::Yes
                } else {
                    Proof::Unknown
                };
                return Evidence::of(assumed, false, None);
            };
            let proof = impl_proof(i, params, args, f, trait_name);
            let blame = (proof != Proof::Yes)
                .then(|| args.iter().find(|a| f(a) != Proof::Yes))
                .flatten();
            Evidence::of(proof, true, blame)
        };
        let copy = evidence("Copy", derives.copy, |s| s.copy);
        let clone = if copy.proof == Proof::Yes {
            Evidence::of(Proof::Yes, false, None)
        } else {
            evidence("Clone", derives.clone, |s| s.clone)
        };
        let eq = evidence("PartialEq", derives.eq, |s| s.eq);
        let mut s = Shape {
            clone: clone.proof,
            copy: copy.proof,
            eq: eq.proof,
            interior: fields.iter().any(|f| f.interior || f.shared),
            shared: fields.iter().any(|f| f.shared),
            cut: fields.iter().chain(args).any(|f| f.cut),
            ..Shape::value()
        };
        let missing = |t: &str| {
            let mut s = Shape {
                shared: false,
                ..s.clone()
            }
            .note(
                    UNKNOWN,
                    format!("{key}{label} has no derived or manual {t} implementation in source, which a skippable parameter needs; it may be generated by a macro."),
                    Some(key),
                );
            s.advice = Some(format!(
                "Pass a type that implements Clone + PartialEq (for Cranpose state containers, pass the state handle). If a macro implements them for {key}, add \"{key}\" to stable_types in cranpose-stability.toml."
            ));
            s
        };
        let blamed = |e: &Evidence| {
            let b = e.blame.clone().unwrap_or_else(Shape::value);
            Shape {
                rank: b.rank,
                reason: b.reason,
                advice: b.advice,
                ty: b.ty.or(Some(key.into())),
                ..s.clone()
            }
        };
        if clone.proof == Proof::No {
            return blamed(&clone);
        }
        if eq.proof == Proof::No {
            return blamed(&eq);
        }
        // A missing contract of the type itself outranks what its fields share.
        for (name, e) in [("Clone", &clone), ("PartialEq", &eq)] {
            if e.proof == Proof::Unknown && e.blame.is_none() {
                return missing(name);
            }
        }
        let manual = match (clone.manual, eq.manual) {
            (true, true) => "its manual Clone and PartialEq implementations",
            (false, true) => "its manual PartialEq implementation",
            (true, false) => "its derived PartialEq and manual Clone implementations",
            (false, false) => "",
        };
        // A dependency's manual PartialEq is its author's definition of equality for the shared
        // state behind it (for example an identity handle); project types stay reviewable.
        if s.shared && eq.manual && !label.is_empty() {
            s.shared = false;
            return s.note(
                NOTE,
                format!("{key}{label} is compared with {manual}, which defines equality for the interior-mutable state it shares."),
                Some(key),
            );
        }
        if s.shared {
            s = if eq.manual {
                let mut s = s.note(
                    SHARED,
                    format!("{key}{label} is compared with {manual}, but a retained clone shares its interior-mutable storage, so a mutation through that storage is not a parameter change."),
                    Some(key),
                );
                s.advice = Some("If that PartialEq deliberately compares identity and changes reach the UI through snapshot state, allow CP003 for this parameter with a reason; otherwise pass an immutable snapshot or a Cranpose state handle.".into());
                s
            } else {
                s.note(
                    SHARED,
                    format!("{key}{label} contains shared interior-mutable data; cloning does not preserve its previous value."),
                    Some(key),
                )
            };
            return s;
        }
        for e in [&clone, &eq] {
            if e.proof == Proof::Unknown {
                return blamed(e);
            }
        }
        if manual.is_empty() {
            s.note(
                PLAIN,
                format!("{key}{label} derives Clone + PartialEq and is compared by value."),
                Some(key),
            )
        } else {
            s.note(
                NOTE,
                format!("{key}{label} is compared with {manual}."),
                Some(key),
            )
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn std(
        &self,
        key: &str,
        args: &[Type],
        context: &Context,
        env: &Rc<Env>,
        visiting: &mut BTreeSet<String>,
        depth: usize,
    ) -> Shape {
        let path = std_canonical(key);
        let arg = |i: usize, visiting: &mut BTreeSet<String>| {
            args.get(i)
                .map(|t| self.inspect(t, context, env, visiting, depth + 1))
                .unwrap_or_else(Shape::copy)
        };
        match std_kind(&path) {
            Some(Std::Copy) => Shape::copy(),
            Some(Std::Value) => Shape {
                copy: Proof::No,
                ..Shape::value()
            },
            Some(Std::Unsized) => Shape {
                clone: Proof::No,
                copy: Proof::No,
                ..Shape::value()
            }
            .note(
                INVALID,
                format!(
                    "{} is unsized; pass an owned value, a reference or a shared pointer.",
                    path.rsplit("::").next().unwrap_or(&path)
                ),
                None,
            ),
            Some(Std::Phantom) => Shape::copy(),
            Some(Std::Wrapper(n)) => combines((0..n).map(|i| arg(i, visiting)).collect()),
            Some(Std::Container(n)) => {
                let mut s = combines((0..n).map(|i| arg(i, visiting)).collect());
                s.copy = Proof::No;
                s
            }
            // Box<str>, Box<Path> and Box<[T]> own unsized values and implement Clone.
            Some(Std::Owning) => {
                let s = match args.first() {
                    Some(Type::Slice(slice)) => {
                        self.inspect(&slice.elem, context, env, visiting, depth + 1)
                    }
                    Some(Type::Path(p)) if self.unsized_std(&p.path, context) => Shape::value(),
                    _ => arg(0, visiting),
                };
                Shape {
                    copy: Proof::No,
                    ..s
                }
            }
            Some(Std::Cow) => {
                let s = match args.first() {
                    Some(Type::Slice(slice)) => {
                        self.inspect(&slice.elem, context, env, visiting, depth + 1)
                    }
                    _ => Shape {
                        clone: Proof::Yes,
                        ..arg(0, visiting)
                    },
                };
                Shape {
                    copy: Proof::No,
                    ..s
                }
                .fix("Cow")
            }
            Some(Std::Shared) => {
                let mut s = arg(0, visiting);
                s.clone = Proof::Yes;
                s.copy = Proof::No;
                if s.interior || s.shared {
                    s.shared = true;
                    s = s.note(
                        SHARED,
                        format!("{} shares interior-mutable storage with the retained parameter. Mutation may be missed by equality.", text_key(key)),
                        Some(key),
                    );
                }
                s.fix(text_key(key))
            }
            Some(Std::Weak) => Shape::invalid(format!(
                "{} does not implement PartialEq; compare an upgraded value or an identifier instead.",
                text_key(key)
            )),
            Some(Std::NoEq) => Shape::invalid(format!(
                "{} does not implement the PartialEq a skippable parameter slot requires.",
                text_key(key)
            )),
            Some(Std::Cell) => {
                let mut s = arg(0, visiting);
                if s.copy != Proof::Yes {
                    s.clone = s.clone.and(if s.copy == Proof::No {
                        Proof::No
                    } else {
                        Proof::Unknown
                    });
                    s.eq = s.clone;
                    let rank = if s.clone == Proof::No {
                        INVALID
                    } else {
                        UNKNOWN
                    };
                    s = s.note(
                        rank,
                        "Cell<T> requires T: Copy for Clone and PartialEq; that contract is not established for this inner type.",
                        None,
                    );
                    s.advice =
                        Some("Use a Copy inner type, RefCell<T>, or an immutable value.".into());
                }
                s.copy = Proof::No;
                s.interior = true;
                if !s.shared && s.clone == Proof::Yes && s.eq == Proof::Yes {
                    s = s.note(PLAIN, "An owned cell is cloned by value; shared references to it require separate review.", None);
                }
                s
            }
            Some(Std::OwnedCell) => {
                let mut s = arg(0, visiting);
                s.copy = Proof::No;
                s.interior = true;
                if !s.shared && s.clone == Proof::Yes && s.eq == Proof::Yes {
                    s = s.note(PLAIN, "An owned cell is cloned by value; shared references to it require separate review.", None);
                }
                s
            }
            Some(Std::Locked) => Shape {
                interior: true,
                ..Shape::invalid(format!(
                    "{} does not provide the Clone + PartialEq contract required by a parameter slot.",
                    text_key(key)
                ))
            },
            Some(Std::Atomic) => Shape {
                interior: true,
                ..Shape::invalid(
                    "Atomic values do not implement the required Clone + PartialEq parameter contract.",
                )
            },
            None => {
                let mut s = Shape::unknown(
                    format!("{path} is not in the analyzer's standard-library table."),
                    stable_advice(&path),
                );
                s.ty = Some(path);
                s
            }
        }
    }
}
struct Evidence {
    proof: Proof,
    manual: bool,
    blame: Option<Shape>,
}
impl Evidence {
    fn of(proof: Proof, manual: bool, blame: Option<&Shape>) -> Self {
        Self {
            proof,
            manual,
            blame: blame.cloned(),
        }
    }
}
impl Cx<'_> {
    fn unsized_std(&self, path: &syn::Path, context: &Context) -> bool {
        let r = self.index.resolve(path, context);
        r.kind == Kind::Std && matches!(std_kind(&std_canonical(&r.key)), Some(Std::Unsized))
    }
}
fn text_key(key: &str) -> &str {
    key.rsplit("::").next().unwrap_or(key)
}
/// Whether a manual impl applies to these arguments, from its bounds on the self type's parameters.
fn impl_proof(
    i: &crate::index::Impl,
    params: &[&syn::TypeParam],
    args: &[Shape],
    f: fn(&Shape) -> Proof,
    trait_name: &str,
) -> Proof {
    let self_args = type_args(i.self_ty.path.segments.last());
    let mut proof = Proof::Yes;
    for (pos, a) in self_args.iter().enumerate() {
        let Type::Path(p) = a else { continue };
        let Some(ident) = p.path.get_ident() else {
            continue;
        };
        let Some(param) = i.generics.type_params().find(|t| &t.ident == ident) else {
            continue;
        };
        let mut bounds: Vec<&TypeParamBound> = param.bounds.iter().collect();
        if let Some(w) = &i.generics.where_clause {
            for pred in &w.predicates {
                if let syn::WherePredicate::Type(t) = pred
                    && matches!(&t.bounded_ty, Type::Path(b) if b.path.is_ident(ident))
                {
                    bounds.extend(t.bounds.iter());
                }
            }
        }
        let wanted: &[&str] = match trait_name {
            "PartialEq" => &["PartialEq", "Eq"],
            "Copy" => &["Copy"],
            _ => &["Clone", "Copy"],
        };
        let needs = bounds.iter().any(|b| {
            matches!(b, TypeParamBound::Trait(t) if t.path.segments.last().is_some_and(|s| wanted.iter().any(|w| s.ident == w)))
        });
        if needs && pos < params.len() {
            proof = proof.and(args.get(pos).map_or(Proof::Yes, f));
        }
    }
    proof
}
/// Cranpose snapshot-state handles, under any re-export of their defining crate.
fn cranpose_handle(key: &str) -> bool {
    let krate = key.split("::").next().unwrap_or(key);
    let krate = krate.split('@').next().unwrap_or(krate);
    ["cranpose", "cranpose_core"].contains(&krate)
        && ["MutableState", "State"].contains(&text_key(key))
}
enum Std {
    Copy,
    Value,
    Unsized,
    Phantom,
    /// Clone, PartialEq and Copy follow the first `n` type arguments.
    Wrapper(usize),
    /// Clone and PartialEq follow the first `n` type arguments; never Copy.
    Container(usize),
    Owning,
    Cow,
    Shared,
    Weak,
    NoEq,
    Cell,
    OwnedCell,
    Locked,
    Atomic,
}
fn std_canonical(key: &str) -> String {
    let key = [
        "std::prelude::v1::",
        "std::prelude::rust_2015::",
        "std::prelude::rust_2018::",
        "std::prelude::rust_2021::",
        "std::prelude::rust_2024::",
    ]
    .iter()
    .find_map(|p| key.strip_prefix(p))
    .map(|name| match name {
        "String" => "std::string::String".to_string(),
        "Vec" => "std::vec::Vec".into(),
        "Option" => "std::option::Option".into(),
        "Result" => "std::result::Result".into(),
        "Box" => "std::boxed::Box".into(),
        n => format!("std::prelude::{n}"),
    })
    .unwrap_or_else(|| key.to_string());
    for (from, to) in [
        ("std::collections::hash_map::", "std::collections::"),
        ("std::collections::hash_set::", "std::collections::"),
        ("std::collections::btree_map::", "std::collections::"),
        ("std::collections::btree_set::", "std::collections::"),
        ("std::collections::vec_deque::", "std::collections::"),
        ("std::collections::linked_list::", "std::collections::"),
        ("std::collections::binary_heap::", "std::collections::"),
        ("std::ffi::os_str::", "std::ffi::"),
        ("std::ffi::c_str::", "std::ffi::"),
        ("std::hash::random::", "std::hash::"),
    ] {
        if let Some(rest) = key.strip_prefix(from) {
            return format!("{to}{rest}");
        }
    }
    key
}
/// Whether the standard-library table describes `path`.
pub(crate) fn std_known(path: &str) -> bool {
    std_kind(&std_canonical(path)).is_some()
}
fn std_kind(path: &str) -> Option<Std> {
    let name = path.strip_prefix("std::")?;
    Some(match name {
        "primitive::str" | "path::Path" | "ffi::OsStr" | "ffi::CStr" => Std::Unsized,
        n if n.starts_with("primitive::") => Std::Copy,
        "time::Duration"
        | "time::Instant"
        | "time::SystemTime"
        | "cmp::Ordering"
        | "net::IpAddr"
        | "net::Ipv4Addr"
        | "net::Ipv6Addr"
        | "net::SocketAddr"
        | "net::SocketAddrV4"
        | "net::SocketAddrV6"
        | "any::TypeId"
        | "thread::ThreadId"
        | "alloc::Layout"
        | "ops::RangeFull"
        | "marker::PhantomPinned"
        | "convert::Infallible"
        | "io::ErrorKind"
        | "fmt::Error"
        | "fmt::Result"
        | "fmt::Alignment"
        | "num::FpCategory"
        | "sync::atomic::Ordering" => Std::Copy,
        n if n.starts_with("num::NonZero") && n.len() > "num::NonZero".len() => Std::Copy,
        "string::String" | "path::PathBuf" | "ffi::OsString" | "ffi::CString" => Std::Value,
        "marker::PhantomData" => Std::Phantom,
        "option::Option"
        | "num::Wrapping"
        | "num::Saturating"
        | "num::NonZero"
        | "cmp::Reverse"
        | "ops::RangeTo"
        | "ops::RangeToInclusive"
        | "ops::Bound"
        | "task::Poll" => Std::Wrapper(1),
        "result::Result" => Std::Wrapper(2),
        "ops::Range"
        | "ops::RangeInclusive"
        | "ops::RangeFrom"
        | "vec::Vec"
        | "collections::VecDeque"
        | "collections::LinkedList"
        | "collections::BTreeSet"
        | "collections::HashSet" => Std::Container(1),
        "collections::BTreeMap" | "collections::HashMap" => Std::Container(2),
        "boxed::Box" => Std::Owning,
        "borrow::Cow" => Std::Cow,
        "rc::Rc" | "sync::Arc" => Std::Shared,
        "rc::Weak" | "sync::Weak" => Std::Weak,
        "collections::BinaryHeap" | "sync::mpsc::Sender" | "sync::mpsc::SyncSender" => Std::NoEq,
        "cell::Cell" => Std::Cell,
        // Write-once cells cannot change a value a retained clone already observed.
        "cell::OnceCell" | "sync::OnceLock" => Std::Container(1),
        "cell::RefCell" => Std::OwnedCell,
        "sync::Mutex"
        | "sync::RwLock"
        | "cell::UnsafeCell"
        | "cell::LazyCell"
        | "sync::LazyLock"
        | "sync::Condvar"
        | "sync::Barrier"
        | "sync::mpsc::Receiver" => Std::Locked,
        n if n.starts_with("sync::atomic::Atomic") => Std::Atomic,
        _ => return None,
    })
}
