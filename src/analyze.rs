use crate::{
    index::{Context, Index, Parsed},
    types, *,
};
use quote::ToTokens;
use syn::{FnArg, Item, Type, TypeParamBound, spanned::Spanned};

/// Analyze sources without compiling or executing any project code.
pub fn analyze(files: &[SourceFile], config: &Config) -> Report {
    let mut report = Report {
        schema_version: 1,
        policy: "cranpose-param-state-v1".into(),
        files_analyzed: files.len(),
        composables: Vec::new(),
        diagnostics: Vec::new(),
    };
    let mut parsed = Vec::new();
    let mut index = Index::default();
    for source in files {
        match syn::parse_file(&source.source) {
            Ok(ast) => {
                let context = Context {
                    crate_name: source.crate_name.clone(),
                    module: source.module.clone(),
                    crate_aliases: source.crate_aliases.clone(),
                };
                index.collect(&ast.items, &context);
                parsed.push(Parsed {
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
    for file in &parsed {
        index.collect_impls(&file.ast.items, &file.context);
    }
    for file in &parsed {
        collect(
            &file.ast.items,
            &file.context,
            &file.source,
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
fn collect(
    items: &[Item],
    context: &Context,
    source: &SourceFile,
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
                        source,
                        index,
                        config,
                        report,
                    );
                }
            }
            Item::Fn(f) => {
                if let Some(attr) = f.attrs.iter().find(|a| is_composable(a, context, index)) {
                    function(f, attr, context, source, index, config, report);
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
                collect(&nested, context, source, index, config, report);
            }
            _ => {}
        }
    }
}
fn is_composable(a: &syn::Attribute, context: &Context, index: &Index) -> bool {
    let path = index.resolve(a.path(), context);
    path == "composable"
        || [
            "cranpose::composable",
            "cranpose_core::composable",
            "cranpose_macros::composable",
        ]
        .contains(&path.as_str())
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
    source: &SourceFile,
    index: &Index,
    config: &Config,
    report: &mut Report,
) {
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
        qualified_name: format!("{}::{name}", context.key()),
        name: name.clone(),
        location: Location::span(&source.source, f.sig.ident.span()),
        skip_mode: mode.into(),
        parameters: Vec::new(),
    };
    for arg in &f.sig.inputs {
        let FnArg::Typed(p) = arg else { continue };
        let parameter = p.pat.to_token_stream().to_string();
        let (stability, mut effect, mut reason, advice) = if matches!(
            p.ty.as_ref(),
            Type::ImplTrait(_)
        ) && !zero_arg_impl(&p.ty)
        {
            (Stability::Unstable,Effect::SkippingDisabled,"This impl Trait parameter takes Cranpose's non-skipping expansion for the entire composable.".into(),"Use an explicit generic value type with Clone + PartialEq when skipping is appropriate. Keep callbacks intentional; do not add false equality implementations.".into())
        } else if callback(&p.ty, &f.sig.generics) {
            (Stability::Unstable,Effect::AlwaysChanged,"Cranpose updates callback holders and marks callback parameters changed on every parent invocation.".into(),"This is expected for callbacks. Keep callback-taking boundaries small or suppress this diagnostic with a documented reason.".into())
        } else {
            types::shape(&p.ty, &f.sig.generics, context, index, config).verdict()
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
            location: Location::span(&source.source, p.ty.span()),
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
