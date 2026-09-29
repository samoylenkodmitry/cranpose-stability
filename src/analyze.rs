use crate::{
    deps::Deps,
    index::{Context, Index},
    types, *,
};
use quote::ToTokens;
use std::collections::HashMap;
use syn::{FnArg, Item, Type, TypeParamBound, spanned::Spanned};

struct Parsed {
    source: SourceFile,
    ast: syn::File,
    context: Context,
    lines: Lines,
}

/// Analyze sources without compiling or executing any project code.
/// Only the supplied files are read; `crate_aliases` naming other crates stay unresolved.
pub fn analyze(files: &[SourceFile], config: &Config) -> Report {
    analyze_with(files, config, Deps::default())
}
/// Analyze sources, reading dependency sources that `deps` can locate on demand.
pub(crate) fn analyze_with(files: &[SourceFile], config: &Config, deps: Deps) -> Report {
    let mut report = Report {
        schema_version: 1,
        policy: "cranpose-param-state-v1".into(),
        files_analyzed: files.len(),
        composables: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut parsed = Vec::new();
    let index = Index::new(deps);
    for source in files {
        let mut deps = index.deps.borrow_mut();
        deps.add_local(&source.crate_name, &source.crate_name, None);
        deps.extend_aliases(&source.crate_name, &source.crate_aliases);
    }
    for source in files {
        match crate::parse::file(&source.source) {
            Ok(ast) => {
                let context = Context {
                    crate_name: source.crate_name.clone(),
                    module: source.module.clone(),
                };
                parsed.push(Parsed {
                    lines: Lines::new(&source.source),
                    source: source.clone(),
                    ast,
                    context,
                });
            }
            Err(error) => report.diagnostics.push(Diagnostic {
                path: source.path.clone(),
                rule: "CP000".into(),
                severity: "error".into(),
                message: format!("Cannot parse Rust source: {error}"),
                help: "Fix syntax before relying on stability results.".into(),
                location: Location::span(&source.source, error.span()),
                function: None,
                parameter: None,
            }),
        }
    }
    let links = plan_modules(&mut parsed);
    for file in &parsed {
        index.add_local(&file.ast.items, &file.context);
    }
    for (parent, name, target) in links {
        index.link_module(&parent, &name, &target);
    }
    for file in &parsed {
        collect(
            &file.ast.items,
            &file.context,
            file,
            &index,
            config,
            &mut report,
        );
    }
    report
        .composables
        .sort_by(|a, b| (&a.path, a.location.start).cmp(&(&b.path, b.location.start)));
    report.diagnostics.sort_by(|a, b| {
        (&a.path, a.location.start, &a.rule).cmp(&(&b.path, b.location.start, &b.rule))
    });
    report
}
/// Project modules come from file layout. A file that only a nonstandard `mod` item loads
/// (`#[path]`, test helpers) takes that item's module; other declarations become aliases.
fn plan_modules(parsed: &mut [Parsed]) -> Vec<(Context, String, Context)> {
    let by_path: HashMap<String, usize> = parsed
        .iter()
        .enumerate()
        .map(|(i, p)| (normalize(&p.source.path), i))
        .collect();
    let declarations = |parsed: &[Parsed]| {
        let mut out = Vec::new();
        for p in parsed {
            let (dir, file) = p
                .source
                .path
                .rsplit_once('/')
                .unwrap_or(("", &p.source.path));
            let stem = file.trim_end_matches(".rs");
            let children = if ["lib", "main", "mod"].contains(&stem) {
                dir.to_string()
            } else {
                join(dir, stem)
            };
            declared(&p.ast.items, &p.context, dir, &children, &by_path, &mut out);
        }
        out
    };
    // Repeat so that files loaded from moved files (nested `#[path]` chains) settle too.
    for _ in 0..8 {
        let mut declared: HashMap<usize, Vec<Context>> = HashMap::new();
        for (parent, name, t) in declarations(parsed) {
            declared.entry(t).or_default().push(parent.child(name));
        }
        let mut changed = false;
        for (t, contexts) in declared {
            if !contexts.contains(&parsed[t].context) {
                parsed[t].context = contexts[0].clone();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    declarations(parsed)
        .into_iter()
        .filter(|(parent, name, t)| parent.child(name.clone()) != parsed[*t].context)
        .map(|(parent, name, t)| (parent, name, parsed[t].context.clone()))
        .collect()
}
/// `mod name;` items and the project file each one loads.
fn declared(
    items: &[Item],
    context: &Context,
    base: &str,
    children: &str,
    files: &HashMap<String, usize>,
    out: &mut Vec<(Context, String, usize)>,
) {
    for item in items {
        let Item::Mod(m) = item else { continue };
        let name = m.ident.to_string();
        match &m.content {
            Some((_, items)) => {
                let dir = join(children, &name);
                declared(items, &context.child(name), &dir, &dir, files, out);
            }
            None => {
                let candidates = match crate::index::path_attr(&m.attrs) {
                    Some(p) => vec![join(base, &p)],
                    None => [children, base]
                        .iter()
                        .flat_map(|d| {
                            [
                                join(d, &format!("{name}.rs")),
                                join(d, &format!("{name}/mod.rs")),
                            ]
                        })
                        .collect(),
                };
                if let Some(&t) = candidates.iter().find_map(|c| files.get(&normalize(c))) {
                    out.push((context.clone(), name, t));
                }
            }
        }
    }
}
fn join(dir: &str, path: &str) -> String {
    if dir.is_empty() {
        path.into()
    } else {
        format!("{dir}/{path}")
    }
}
/// Resolve `.` and `..` lexically in a project-relative path.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}
fn collect(
    items: &[Item],
    context: &Context,
    file: &Parsed,
    index: &Index,
    config: &Config,
    report: &mut Report,
) {
    for item in items {
        match item {
            Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    collect(
                        items,
                        &context.child(m.ident.to_string()),
                        file,
                        index,
                        config,
                        report,
                    );
                }
            }
            Item::Fn(f) => {
                if let Some(attr) = f.attrs.iter().find(|a| is_composable(a, context, index)) {
                    function(f, attr, context, file, index, config, report);
                }
                let nested: Vec<_> = f
                    .block
                    .stmts
                    .iter()
                    .filter_map(|s| {
                        if let syn::Stmt::Item(i) = s {
                            Some(i.clone())
                        } else {
                            None
                        }
                    })
                    .collect();
                if !nested.is_empty() {
                    let block = context.block(&f.sig.ident);
                    index.add_local(&nested, &block);
                    collect(&nested, &block, file, index, config, report);
                }
            }
            _ => {}
        }
    }
}
fn is_composable(a: &syn::Attribute, context: &Context, index: &Index) -> bool {
    let path = index.macro_path(a.path(), context);
    let krate = path.split("::").next().unwrap_or_default();
    let krate = krate.split('@').next().unwrap_or_default();
    path == "composable"
        || path.ends_with("::composable")
            && [
                "cranpose",
                "cranpose_core",
                "cranpose_macros",
                "cranpose_ui",
            ]
            .contains(&krate)
}
fn fn_bound(bound: &TypeParamBound) -> bool {
    matches!(bound,TypeParamBound::Trait(t) if t.path.segments.last().is_some_and(|s|["Fn","FnMut","FnOnce"].iter().any(|n|s.ident==n)))
}
fn zero_arg_impl(ty: &Type) -> bool {
    if let Type::ImplTrait(t) = ty {
        return t.bounds.iter().any(|b|if let TypeParamBound::Trait(t)=b {t.path.segments.last().is_some_and(|s|{
            ["Fn","FnMut"].iter().any(|n|s.ident==n)&&matches!(&s.arguments,syn::PathArguments::Parenthesized(p) if p.inputs.is_empty() && (matches!(p.output,syn::ReturnType::Default)||matches!(&p.output,syn::ReturnType::Type(_,t) if matches!(t.as_ref(),Type::Tuple(t) if t.elems.is_empty()))))
        })}else{false});
    }
    false
}
fn callback(ty: &Type, generics: &syn::Generics) -> bool {
    match ty {
        Type::BareFn(_) => true,
        Type::ImplTrait(t) => t.bounds.iter().any(fn_bound),
        Type::Path(p) => {
            if let Some(last) = p.path.segments.last() {
                if last.ident == "Box"
                    && let syn::PathArguments::AngleBracketed(args) = &last.arguments
                    && let Some(syn::GenericArgument::Type(Type::TraitObject(t))) =
                        args.args.first()
                {
                    return t.bounds.iter().any(fn_bound);
                }
                if p.path.segments.len() == 1 {
                    let inline = generics
                        .type_params()
                        .any(|t| t.ident == last.ident && t.bounds.iter().any(fn_bound));
                    let where_fn=generics.where_clause.as_ref().is_some_and(|w|w.predicates.iter().any(|p|matches!(p,syn::WherePredicate::Type(t) if matches!(&t.bounded_ty,Type::Path(p) if p.path.is_ident(&last.ident))&&t.bounds.iter().any(fn_bound))));
                    return inline || where_fn;
                }
            }
            false
        }
        _ => false,
    }
}
#[allow(clippy::too_many_arguments)]
fn function(
    f: &syn::ItemFn,
    attr: &syn::Attribute,
    context: &Context,
    file: &Parsed,
    index: &Index,
    config: &Config,
    report: &mut Report,
) {
    let source = &file.source;
    let no_skip = attr
        .parse_args::<syn::Ident>()
        .is_ok_and(|i| i == "no_skip");
    let unhandled=f.sig.inputs.iter().any(|p|matches!(p,FnArg::Typed(p) if matches!(p.ty.as_ref(),Type::ImplTrait(_))&&!zero_arg_impl(&p.ty)));
    let mode = if no_skip {
        "explicitNoSkip"
    } else if unhandled {
        "opaqueParameter"
    } else {
        "parameterComparison"
    };
    let name = f.sig.ident.to_string();
    let mut c = Composable {
        path: source.path.clone(),
        qualified_name: format!("{}::{name}", context.scope().key()),
        name: name.clone(),
        location: Location::within(&source.source, &file.lines, f.sig.ident.span()),
        skip_mode: mode.into(),
        parameters: Vec::new(),
    };
    for arg in &f.sig.inputs {
        let FnArg::Typed(p) = arg else { continue };
        let parameter = p.pat.to_token_stream().to_string();
        let (stability, mut effect, mut reason, advice, resolved) = if matches!(
            p.ty.as_ref(),
            Type::ImplTrait(_)
        ) && !zero_arg_impl(&p.ty)
        {
            (Stability::Unstable,Effect::SkippingDisabled,"This impl Trait parameter takes Cranpose's non-skipping expansion for the entire composable.".into(),"Use an explicit generic value type with Clone + PartialEq when skipping is appropriate. Keep callbacks intentional; do not add false equality implementations.".into(),None)
        } else if callback(&p.ty, &f.sig.generics) {
            (Stability::Unstable,Effect::AlwaysChanged,"Cranpose updates callback holders and marks callback parameters changed on every parent invocation.".into(),"This is expected for callbacks. Keep callback-taking boundaries small or suppress this diagnostic with a documented reason.".into(),None)
        } else {
            let v = types::shape(&p.ty, &f.sig.generics, context, index, config).verdict();
            (v.stability, v.effect, v.reason, v.advice, v.resolved)
        };
        let inactive = no_skip || (unhandled && effect != Effect::SkippingDisabled);
        let rule = match effect {
            Effect::AlwaysChanged => Some("CP001"),
            Effect::SkippingDisabled => Some("CP002"),
            Effect::SharedMutation => Some("CP003"),
            Effect::InvalidParameter => Some("CP004"),
            Effect::Unproven => Some("CP005"),
            _ => None,
        };
        let suppressed = rule.and_then(|rule| {
            config
                .allow
                .iter()
                .find(|a| {
                    globset::Glob::new(&a.path)
                        .is_ok_and(|g| g.compile_matcher().is_match(&source.path))
                        && (a.function == name || a.function == c.qualified_name)
                        && (a.parameter == parameter || a.parameter == "*")
                        && a.rule == rule
                        && !a.reason.trim().is_empty()
                })
                .map(|a| a.reason.clone())
        });
        if no_skip {
            effect = Effect::SkippingDisabled;
            reason="Skipping is deliberately disabled by #[composable(no_skip)]; no parameter comparison is generated.".into();
        } else if inactive {
            effect = Effect::SkippingDisabled;
            reason = "Another impl Trait parameter disables skipping for this function; this parameter is not compared.".into();
        }
        let param = Parameter {
            name: parameter.clone(),
            type_text: p.ty.to_token_stream().to_string(),
            location: Location::within(&source.source, &file.lines, p.ty.span()),
            stability,
            effect,
            reason,
            advice,
            rule: if inactive {
                None
            } else {
                rule.map(str::to_string)
            },
            suppressed,
            resolved_type: resolved,
        };
        if !no_skip
            && param.suppressed.is_none()
            && let Some(rule) = &param.rule
        {
            let severity = if param.stability == Stability::Incompatible {
                "error"
            } else if param.stability == Stability::Unknown {
                "note"
            } else {
                "warning"
            };
            report.diagnostics.push(Diagnostic {
                path: source.path.clone(),
                rule: rule.clone(),
                severity: severity.into(),
                message: format!("{name}({parameter}): {}", param.reason),
                help: param.advice.clone(),
                location: param.location.clone(),
                function: Some(c.qualified_name.clone()),
                parameter: Some(parameter),
            });
        }
        c.parameters.push(param);
    }
    report.composables.push(c);
}
