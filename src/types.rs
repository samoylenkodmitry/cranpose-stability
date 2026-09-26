use crate::index::{Context, Definition, Index};
use crate::{Config, Effect, Stability};
use std::collections::{BTreeMap, BTreeSet};
use syn::{GenericArgument, PathArguments, Type, TypeParamBound};

#[derive(Clone, Copy, PartialEq)]
enum Proof {
    Yes,
    No,
    Unknown,
}
#[derive(Clone)]
pub(crate) struct Shape {
    clone: Proof,
    eq: Proof,
    interior: bool,
    shared: bool,
    reason: String,
}
impl Shape {
    fn value() -> Self {
        Self {
            clone: Proof::Yes,
            eq: Proof::Yes,
            interior: false,
            shared: false,
            reason: "Compared by value through Clone + PartialEq.".into(),
        }
    }
    fn unknown(reason: impl Into<String>) -> Self {
        Self {
            clone: Proof::Unknown,
            eq: Proof::Unknown,
            reason: reason.into(),
            ..Self::value()
        }
    }
    pub fn verdict(&self) -> (Stability, Effect, String, String) {
        if self.clone == Proof::No || self.eq == Proof::No {
            return (Stability::Incompatible,Effect::InvalidParameter,self.reason.clone(),"Use a value with Clone + PartialEq, or make the composable explicitly no_skip if unconditional execution is intended.".into());
        }
        if self.shared {
            return (Stability::Unstable,Effect::SharedMutation,self.reason.clone(),"Pass an immutable value snapshot or a Cranpose snapshot-state handle. A cloned shared pointer does not retain the old inner value.".into());
        }
        if self.clone == Proof::Unknown || self.eq == Proof::Unknown {
            return (Stability::Unknown,Effect::Unproven,self.reason.clone(),"Inspect the type's Clone and PartialEq contract. External or generated implementations require an explicit stable_types contract to establish stability.".into());
        }
        (Stability::Stable,Effect::ValueComparison,self.reason.clone(),"Equal values can skip this parameter check; this does not measure runtime cost or guarantee that the whole composable skips.".into())
    }
}
#[derive(Clone)]
struct Binding {
    ty: Type,
    context: Context,
}
type Bindings = BTreeMap<String, Binding>;

pub(crate) fn shape(
    ty: &Type,
    generics: &syn::Generics,
    context: &Context,
    index: &Index,
    config: &Config,
) -> Shape {
    let mut bounds = BTreeMap::new();
    for p in &generics.params {
        if let syn::GenericParam::Type(p) = p {
            bounds.insert(
                p.ident.to_string(),
                p.bounds.iter().cloned().collect::<Vec<_>>(),
            );
        }
    }
    if let Some(w) = &generics.where_clause {
        for p in &w.predicates {
            if let syn::WherePredicate::Type(p) = p
                && let Type::Path(t) = &p.bounded_ty
                && t.path.segments.len() == 1
            {
                bounds
                    .entry(t.path.segments[0].ident.to_string())
                    .or_default()
                    .extend(p.bounds.iter().cloned());
            }
        }
    }
    inspect(
        ty,
        context,
        index,
        config,
        &Bindings::new(),
        &bounds,
        &mut BTreeSet::new(),
        0,
    )
}
fn combines(values: Vec<Shape>) -> Shape {
    let mut result = Shape::value();
    for s in values {
        if s.clone != Proof::Yes && result.clone != Proof::No {
            result.clone = s.clone;
        }
        if s.eq != Proof::Yes && result.eq != Proof::No {
            result.eq = s.eq;
        }
        result.interior |= s.interior;
        result.shared |= s.shared;
        if s.shared || s.clone != Proof::Yes || s.eq != Proof::Yes {
            result.reason = s.reason;
        }
    }
    result
}
fn bounds_has(bounds: &[TypeParamBound], name: &str) -> bool {
    bounds.iter().any(|b|matches!(b,TypeParamBound::Trait(t) if t.path.segments.last().is_some_and(|s|s.ident==name)))
}
#[allow(clippy::too_many_arguments)]
fn inspect(
    ty: &Type,
    context: &Context,
    index: &Index,
    config: &Config,
    bindings: &Bindings,
    bounds: &BTreeMap<String, Vec<TypeParamBound>>,
    visiting: &mut BTreeSet<String>,
    depth: usize,
) -> Shape {
    if depth > 40 {
        return Shape::unknown("Recursive type analysis reached its limit.");
    }
    let recurse = |t: &Type, visiting: &mut BTreeSet<String>| {
        inspect(
            t,
            context,
            index,
            config,
            bindings,
            bounds,
            visiting,
            depth + 1,
        )
    };
    match ty {
        Type::Paren(t)=>recurse(&t.elem,visiting),Type::Group(t)=>recurse(&t.elem,visiting),
        Type::Tuple(t)=>combines(t.elems.iter().map(|t|recurse(t,visiting)).collect()),
        Type::Array(t)=>recurse(&t.elem,visiting),
        Type::Slice(t)=>{let mut s=recurse(&t.elem,visiting);s.clone=Proof::No;s.reason="An unsized slice cannot be stored by value in ParamState.".into();s},
        Type::Reference(r)=>{
            let mut s=recurse(&r.elem,visiting);
            s.clone=if r.mutability.is_some(){Proof::No}else{Proof::Yes};
            if r.mutability.is_some(){s.reason="Mutable references do not implement Clone, which a skippable parameter slot requires.".into();}
            else if s.interior || s.shared{s.shared=true;s.reason="This reference aliases interior-mutable data. The retained reference can see the new value on both sides of the comparison.".into();}
            s
        },
        Type::Ptr(_)=>Shape{reason:"Raw pointers compare addresses. Pointee mutation is not observed by this comparison.".into(),..Shape::unknown("Raw pointer semantics require review.")},
        Type::Path(p)=>{
            if p.qself.is_some(){return Shape::unknown("Associated types require compiler type resolution.");}
            if p.path.segments.len()==1 {
                let n=p.path.segments[0].ident.to_string();
                if let Some(b)=bindings.get(&n){return inspect(&b.ty,&b.context,index,config,&Bindings::new(),bounds,visiting,depth+1);}
                if let Some(b)=bounds.get(&n) {
                    return if bounds_has(b,"Clone") && bounds_has(b,"PartialEq") {Shape::value()}else{Shape::unknown(format!("Generic parameter {n} has no directly visible Clone + PartialEq contract."))};
                }
            }
            let key=index.resolve(&p.path,context);
            if config.stable_types.iter().any(|p|p==&key) {return Shape{reason:format!("User-declared stability contract for {key}; runtime behavior is not verified."),..Shape::value()};}
            let args=p.path.segments.last().and_then(|s|match &s.arguments{PathArguments::AngleBracketed(a)=>Some(a.args.iter().filter_map(|a|if let GenericArgument::Type(t)=a{Some(t.clone())}else{None}).collect::<Vec<_>>()),_=>None}).unwrap_or_default();
            let children=||args.iter().map(|a|recurse(a,&mut visiting.clone())).collect::<Vec<_>>();
            if let Some(defs)=index.definitions.get(&key) {
                if defs.len()!=1{return Shape::unknown(format!("Multiple declarations of {key}; cfg or target selection is unresolved."));}
                if !visiting.insert(key.clone()){return Shape::unknown(format!("Recursive type {key} needs compiler verification."));}
                let named=&defs[0];
                let generics=match &named.definition {Definition::Data{generics,..}|Definition::Alias{generics,..}=>generics};
                let mut bound=bindings.clone();
                let mut supplied=args.iter();
                for param in &generics.params {if let syn::GenericParam::Type(param)=param&& let Some(t)=supplied.next().or(param.default.as_ref()){bound.insert(param.ident.to_string(),Binding{ty:t.clone(),context:context.clone()});}}
                let result=match &named.definition {
                    Definition::Alias{ty,..}=>inspect(ty,&named.context,index,config,&bound,bounds,visiting,depth+1),
                    Definition::Data{fields,clone,eq,copy,..}=>{
                        let mut s=combines(fields.iter().map(|t|inspect(t,&named.context,index,config,&bound,bounds,visiting,depth+1)).collect());
                        if !clone && !copy {s.clone=Proof::Unknown;s.reason=format!("{key} has no source-visible derived Clone contract.");}
                        if !eq {s.eq=Proof::Unknown;s.reason=format!("{key} has no source-visible derived PartialEq contract.");}
                        if index.manual_traits.get(&key).is_some_and(|t|t.iter().any(|t|t=="PartialEq"||t=="Clone")) {s.eq=Proof::Unknown;s.shared=false;s.interior=false;s.reason=format!("{key} uses a manual Clone or PartialEq implementation; its behavior needs review.");}
                        if s.shared {s.reason=format!("{key} contains shared interior-mutable data; cloning does not preserve its previous value.");}
                        s
                    }
                };
                visiting.remove(&key);return result;
            }
            let canonical=key.replace("std::primitive::","core::primitive::").replace("alloc::","std::");
            if canonical.starts_with("core::primitive::") {let mut s=Shape::value();if canonical.ends_with("::str"){s.clone=Proof::No;s.reason="str is unsized; pass a string value or shared string slice.".into();}return s;}
            if ["std::string::String","std::path::PathBuf","std::ffi::OsString","std::time::Duration","core::time::Duration","std::time::Instant"].contains(&canonical.as_str()){return Shape::value();}
            if ["std::rc::Rc","std::sync::Arc"].contains(&canonical.as_str()){
                let mut s=combines(children());s.clone=Proof::Yes;
                if s.interior||s.shared{s.shared=true;s.reason=format!("{key} shares interior-mutable storage with the retained parameter. Mutation may be missed by equality.");}return s;
            }
            if ["std::cell::Cell","core::cell::Cell","std::cell::RefCell","core::cell::RefCell"].contains(&canonical.as_str()) {
                let mut s=combines(children());s.interior=true;
                if !s.shared && s.clone==Proof::Yes && s.eq==Proof::Yes{s.reason="An owned cell is cloned by value; shared references to it require separate review.".into();}
                return s;
            }
            if ["std::sync::Mutex","std::sync::RwLock","std::cell::UnsafeCell","core::cell::UnsafeCell"].contains(&canonical.as_str()) {
                return Shape{clone:Proof::No,eq:Proof::No,interior:true,shared:false,reason:format!("{key} does not provide the Clone + PartialEq contract required by a parameter slot.")};
            }
            if canonical.starts_with("std::sync::atomic::") || canonical.starts_with("core::sync::atomic::"){return Shape{clone:Proof::No,eq:Proof::No,interior:true,shared:false,reason:"Atomic values do not implement the required Clone + PartialEq parameter contract.".into()};}
            if ["core::marker::PhantomData","std::marker::PhantomData"].contains(&canonical.as_str()){return Shape::value();}
            if ["std::vec::Vec","std::boxed::Box","core::option::Option","std::option::Option","core::result::Result","std::result::Result","std::collections::HashMap","std::collections::BTreeMap","std::collections::HashSet","std::collections::BTreeSet","std::collections::VecDeque","std::borrow::Cow"].contains(&canonical.as_str()) {
                if ["std::boxed::Box","std::borrow::Cow"].contains(&canonical.as_str()) {
                    if let Some(Type::Slice(slice))=args.first() { return recurse(&slice.elem,visiting); }
                    if let Some(Type::Path(path))=args.first()
                        && index.resolve(&path.path,context)=="core::primitive::str" { return Shape::value(); }
                }
                return combines(children());
            }
            // Cranpose state handles compare identity and track value reads separately.
            if ["cranpose::MutableState","cranpose_core::MutableState","cranpose::State","cranpose_core::State"].contains(&key.as_str()){return Shape{reason:"Cranpose state handle: identity is compared and snapshot reads track changes separately.".into(),..Shape::value()};}
            Shape::unknown(format!("The Clone/PartialEq and mutation contract of {key} is not visible in this source set."))
        },
        _=>Shape::unknown("This Rust type requires compiler resolution or macro expansion."),
    }
}
