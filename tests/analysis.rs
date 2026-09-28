use cranpose_stability::{output, *};

fn report(source: &str) -> Report {
    analyze(
        &[SourceFile {
            path: "src/lib.rs".into(),
            source: source.into(),
            crate_name: "app".into(),
            module: vec![],
            crate_aliases: Default::default(),
        }],
        &Config::default(),
    )
}
fn states(source: &str) -> Vec<Stability> {
    report(source).composables[0]
        .parameters
        .iter()
        .map(|p| p.stability)
        .collect()
}

#[test]
fn ordinary_values_and_collections_compare_by_value() {
    assert_eq!(
        states(
            "#[cranpose::composable] fn View(a: i32, b: String, c: Vec<Option<String>>, d: &'static str) {}"
        ),
        vec![Stability::Stable; 4]
    );
}
#[test]
fn all_callback_forms_follow_macro_rules() {
    let r = report(
        "#[composable] fn View<F>(a: impl Fn(), b: Box<dyn FnMut()>, c: fn(), d: F) where F: FnOnce() {}",
    );
    assert_eq!(r.diagnostics.len(), 4);
    assert!(r.diagnostics.iter().all(|d| d.rule == "CP001"));
}
#[test]
fn opaque_argument_disables_skipping_for_the_function() {
    let r = report("#[composable] fn View(a: impl AsRef<str>, b: i32) {}");
    assert_eq!(r.composables[0].skip_mode, "opaqueParameter");
    assert_eq!(r.diagnostics[0].rule, "CP002");
}
#[test]
fn nonzero_callback_impl_uses_non_skipping_expansion() {
    let r = report("#[composable] fn View(a: impl Fn(u32), b: impl FnOnce()) {}");
    assert!(r.diagnostics.iter().all(|d| d.rule == "CP002"));
}
#[test]
fn intentional_no_skip_has_badges_without_warnings() {
    let r = report("#[composable(no_skip)] fn View(a: impl Fn(), b: &mut i32) {}");
    assert!(r.diagnostics.is_empty());
    assert!(
        r.composables[0]
            .parameters
            .iter()
            .all(|p| p.effect == Effect::SkippingDisabled)
    );
}
#[test]
fn derived_local_data_resolves_fields_and_generics() {
    assert_eq!(
        states(
            "#[derive(Clone, PartialEq)] struct Model<T> { value: T, list: Vec<String> } #[composable] fn View(a: Model<i32>) {}"
        ),
        vec![Stability::Stable]
    );
}
#[test]
fn local_enum_and_alias_are_resolved() {
    assert_eq!(
        states(
            "#[derive(Clone, PartialEq)] enum Model { A(String), B {n: i32} } type Alias=Model; #[composable] fn View(a: Alias) {}"
        ),
        vec![Stability::Stable]
    );
}
#[test]
fn shared_interior_mutation_is_different_from_always_changed() {
    let r =
        report("use std::{rc::Rc,cell::RefCell}; #[composable] fn View(a: Rc<RefCell<i32>>) {}");
    assert_eq!(r.diagnostics[0].rule, "CP003");
    assert_eq!(
        r.composables[0].parameters[0].effect,
        Effect::SharedMutation
    );
}
#[test]
fn owned_cell_is_not_mislabeled_as_shared() {
    assert_eq!(
        states("use std::cell::RefCell; #[composable] fn View(a: RefCell<i32>) {}"),
        vec![Stability::Stable]
    );
}
#[test]
fn shared_hazard_traverses_struct_and_alias() {
    let r = report(
        "use std::{rc::Rc,cell::RefCell}; type Shared=Rc<RefCell<i32>>; #[derive(Clone,PartialEq)] struct Model{value: Shared} #[composable] fn View(a: Model) {}",
    );
    assert_eq!(r.diagnostics[0].rule, "CP003");
}
#[test]
fn reference_to_cell_preserves_alias_hazard() {
    assert_eq!(
        report("use std::cell::Cell; #[composable] fn View(a: &'static Cell<i32>) {}").diagnostics
            [0]
        .rule,
        "CP003"
    );
}
#[test]
fn incompatible_standard_types_are_reported() {
    let r = report("use std::sync::Mutex; #[composable] fn View(a: Mutex<i32>, b: &mut i32) {}");
    assert_eq!(r.diagnostics.len(), 2);
    assert!(
        r.diagnostics
            .iter()
            .all(|d| d.rule == "CP004" && d.severity == "error")
    );
}
#[test]
fn foreign_contracts_are_unknown_not_definite_failures() {
    let r = report("use foreign::Model; #[composable] fn View(a: Model) {}");
    assert_eq!(r.diagnostics[0].rule, "CP005");
    assert!(!r.has_findings(false));
    assert!(r.has_findings(true));
}
#[test]
fn manual_equality_is_the_comparison_contract() {
    let r = report(
        "#[derive(Clone)] struct Model(i32); impl PartialEq for Model{fn eq(&self,_:&Self)->bool{false}} #[composable] fn View(a: Model) {}",
    );
    let p = &r.composables[0].parameters[0];
    assert_eq!(p.stability, Stability::Stable);
    assert!(p.reason.contains("manual PartialEq"), "{}", p.reason);
    assert_eq!(p.resolved_type.as_deref(), Some("app::Model"));
}
#[test]
fn explicit_generic_bounds_are_used() {
    assert_eq!(
        states("#[composable] fn View<T:Clone+PartialEq,U>(a:T,b:U) where U:Clone+PartialEq {}"),
        vec![Stability::Stable; 2]
    );
}
#[test]
fn unknown_generic_contract_stays_unknown() {
    assert_eq!(
        states("#[composable] fn View<T:UserTrait>(a:T) {}"),
        vec![Stability::Unknown]
    );
}
#[test]
fn local_shadowing_cannot_be_mistaken_for_a_standard_type() {
    assert_eq!(
        states("struct String; #[composable] fn View(a:String) {}"),
        vec![Stability::Unknown]
    );
}
#[test]
fn module_imports_renames_and_macro_aliases() {
    let r = report(
        "mod model {#[derive(Clone,PartialEq)] pub struct Model{pub n:i32}} mod ui {use crate::model::Model as Data; use cranpose::composable as component; #[component] fn View(a:Data) {}}",
    );
    assert_eq!(r.composables.len(), 1);
    assert_eq!(r.composables[0].parameters[0].stability, Stability::Stable);
}
#[test]
fn unicode_offsets_are_editor_utf16_and_source_bytes() {
    let source = "const _: &str = \"😀\"; #[composable] fn View(名字: String) {}";
    let r = report(source);
    let p = &r.composables[0].parameters[0];
    assert_eq!(&source[p.location.start..p.location.end], "String");
    assert_eq!(
        p.location.utf16_start,
        source[..p.location.start].encode_utf16().count()
    );
    assert_eq!(p.location.utf16_end - p.location.utf16_start, 6);
}
#[test]
fn malformed_source_produces_a_diagnostic_not_empty_success() {
    let r = report("#[composable] fn View(");
    assert_eq!(r.diagnostics[0].rule, "CP000");
}
#[test]
fn function_suppression_requires_a_reason_and_preserves_badge() {
    let files = [SourceFile {
        path: "src/lib.rs".into(),
        source: "#[composable] fn View(callback: impl Fn()) {}".into(),
        crate_name: "app".into(),
        module: vec![],
        crate_aliases: Default::default(),
    }];
    let config = Config {
        allow: vec![Allow {
            path: "src/**".into(),
            function: "View".into(),
            parameter: "callback".into(),
            rule: "CP001".into(),
            reason: "Interactive event boundary".into(),
        }],
        ..Config::default()
    };
    let r = analyze(&files, &config);
    assert!(r.diagnostics.is_empty());
    assert!(r.composables[0].parameters[0].suppressed.is_some());
}
#[test]
fn explicit_external_contract_is_identified_as_user_asserted() {
    let files = [SourceFile {
        path: "src/lib.rs".into(),
        source: "use external::Model; #[composable] fn View(a: Model) {}".into(),
        crate_name: "app".into(),
        module: vec![],
        crate_aliases: Default::default(),
    }];
    let r = analyze(
        &files,
        &Config {
            stable_types: vec!["external::Model".into()],
            ..Config::default()
        },
    );
    assert_eq!(r.composables[0].parameters[0].stability, Stability::Stable);
    assert!(
        r.composables[0].parameters[0]
            .reason
            .contains("User-declared")
    );
}
#[test]
fn sarif_has_utf16_regions_and_encoded_paths() {
    let mut r = report("#[composable] fn View(a:impl Fn()) {}");
    r.diagnostics[0].path = "src/a b.rs".into();
    let sarif = output::sarif(&r);
    assert_eq!(sarif["runs"][0]["columnKind"], "utf16CodeUnits");
    assert_eq!(
        sarif["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "src/a%20b.rs"
    );
}
#[test]
fn workflow_annotations_escape_control_sequences() {
    let mut r = report("#[composable] fn View(a:impl Fn()) {}");
    r.diagnostics[0].message = "one\n::error::injected".into();
    r.diagnostics[0].path = "a,b.rs".into();
    let text = output::github(&r);
    assert!(text.contains("a%2Cb.rs"));
    assert!(text.contains("one%0A::error::injected"));
    assert_eq!(text.lines().count(), 1);
}
#[test]
fn cross_file_type_resolution() {
    let r = analyze(
        &[
            SourceFile {
                path: "src/lib.rs".into(),
                source: "use crate::model::Model; #[composable] fn View(a:Model) {}".into(),
                crate_name: "app".into(),
                module: vec![],
                crate_aliases: Default::default(),
            },
            SourceFile {
                path: "src/model.rs".into(),
                source: "#[derive(Clone,PartialEq)] pub struct Model{pub text:String}".into(),
                crate_name: "app".into(),
                module: vec!["model".into()],
                crate_aliases: Default::default(),
            },
        ],
        &Config::default(),
    );
    assert_eq!(r.composables[0].parameters[0].stability, Stability::Stable);
}
#[test]
fn recursive_alias_does_not_crash() {
    let r = report("type A=A; #[composable] fn View(a:A) {}");
    assert_eq!(r.composables[0].parameters[0].stability, Stability::Unknown);
}

#[test]
fn an_opaque_parameter_disables_other_parameter_checks() {
    let r = report(
        "#[composable] fn View(text:impl AsRef<str>, callback:impl Fn(), value:&mut i32) {}",
    );
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.diagnostics[0].rule, "CP002");
}
#[test]
fn crate_root_relative_import_resolves() {
    assert_eq!(
        states(
            "mod data {#[derive(Clone,PartialEq)] pub struct Model;} use data::Model; #[composable] fn View(a:Model) {}"
        ),
        vec![Stability::Stable]
    );
}

#[test]
fn boxed_strings_and_slices_use_the_owned_contract() {
    assert_eq!(
        states(
            "use std::borrow::Cow; #[composable] fn View(a:Box<str>,b:Box<[String]>,c:Cow<'static,str>) {}"
        ),
        vec![Stability::Stable; 3]
    );
}
#[test]
fn boxes_do_not_hide_a_nonclone_element() {
    assert_eq!(
        report("#[composable] fn View(a:Box<[&mut i32]>) {}").diagnostics[0].rule,
        "CP004"
    );
}
#[test]
fn unrelated_attribute_import_is_not_cranpose() {
    assert!(
        report("use another::composable; #[composable] fn View(a:impl Fn()) {}")
            .composables
            .is_empty()
    );
}

#[test]
fn cell_requires_copy_but_refcell_can_clone_owned_values() {
    assert_eq!(
        states(
            "use std::cell::{Cell,RefCell}; #[composable] fn View(a:Cell<String>, b:Cell<i32>, c:RefCell<String>, d:Cell<Vec<i32>>, e:Cell<Option<i32>>) {}"
        ),
        vec![
            Stability::Incompatible,
            Stability::Stable,
            Stability::Stable,
            Stability::Incompatible,
            Stability::Stable
        ]
    );
}
#[test]
fn cell_copy_contract_resolves_generics_aliases_and_local_derives() {
    assert_eq!(
        states(
            "use std::cell::Cell; #[derive(Copy,Clone,PartialEq)] struct Id(u32); type Alias=Id; #[composable] fn View<T:Copy+Eq>(a:Cell<T>, b:Cell<Alias>, c:T) {}"
        ),
        vec![Stability::Stable; 3]
    );
    assert_eq!(
        states("use std::cell::Cell; #[composable] fn View<T:Clone+PartialEq>(a:Cell<T>) {}"),
        vec![Stability::Unknown]
    );
}

fn crates(files: &[(&str, &str, &str)]) -> Report {
    let files: Vec<SourceFile> = files
        .iter()
        .map(|(krate, module, source)| SourceFile {
            path: format!(
                "{krate}/{}.rs",
                if module.is_empty() { "lib" } else { module }
            ),
            source: source.to_string(),
            crate_name: krate.to_string(),
            module: module
                .split('/')
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .collect(),
            crate_aliases: [("ui".to_string(), "ui".to_string())].into(),
        })
        .collect();
    analyze(&files, &Config::default())
}
fn parameter<'a>(r: &'a Report, name: &str) -> &'a Parameter {
    r.composables
        .iter()
        .flat_map(|c| &c.parameters)
        .find(|p| p.name == name)
        .expect("parameter")
}

#[test]
fn primitives_and_prelude_types_resolve_under_an_unreadable_glob() {
    assert_eq!(
        states(
            "use cranpose::prelude::*; #[composable] fn View(a: bool, b: String, c: usize, d: &'static str, e: f32, f: Option<String>, g: char, h: Vec<u8>) {}"
        ),
        vec![Stability::Stable; 8]
    );
}
#[test]
fn a_glob_that_exports_a_prelude_name_shadows_it() {
    let r = crates(&[
        (
            "ui",
            "",
            "pub mod prelude { pub struct String; pub fn bool() {} }",
        ),
        (
            "app",
            "",
            "use ui::prelude::*; #[composable] fn View(a: String, b: bool) {}",
        ),
    ]);
    let a = parameter(&r, "a");
    assert_eq!(a.stability, Stability::Unknown);
    assert_eq!(a.resolved_type.as_deref(), Some("ui::prelude::String"));
    assert_eq!(parameter(&r, "b").stability, Stability::Stable);
}
#[test]
fn private_items_are_not_glob_exported_to_other_crates() {
    let r = crates(&[
        (
            "ui",
            "",
            "pub mod prelude { use crate::Hidden as String; pub use crate::Shown; } struct Hidden; #[derive(Clone, PartialEq)] pub struct Shown;",
        ),
        (
            "app",
            "",
            "use ui::prelude::*; #[composable] fn View(a: String, b: Shown) {}",
        ),
    ]);
    assert_eq!(parameter(&r, "a").stability, Stability::Stable);
    assert_eq!(
        parameter(&r, "b").resolved_type.as_deref(),
        Some("ui::Shown")
    );
}
#[test]
fn project_identity_handle_over_shared_storage_is_reported() {
    let r = report(
        "use std::{cell::RefCell, rc::Rc}; #[derive(Clone)] struct Handle(Rc<RefCell<i32>>); impl PartialEq for Handle { fn eq(&self, o: &Self) -> bool { Rc::ptr_eq(&self.0, &o.0) } } #[composable] fn View(a: Handle) {}",
    );
    let p = &r.composables[0].parameters[0];
    assert_eq!(p.effect, Effect::SharedMutation);
    assert!(p.reason.contains("manual PartialEq"), "{}", p.reason);
    assert!(p.advice.contains("CP003"), "{}", p.advice);
}
#[test]
fn derived_generic_arguments_carry_contracts_and_hazards() {
    let r = report(
        "use std::{cell::RefCell, rc::Rc}; use foreign::Opaque; #[derive(Clone, PartialEq)] struct Wrap<T> { value: T } #[composable] fn View(a: Wrap<i32>, b: Wrap<Rc<RefCell<i32>>>, c: Wrap<Opaque>) {}",
    );
    let p = &r.composables[0].parameters;
    assert_eq!(p[0].stability, Stability::Stable);
    assert_eq!(p[1].effect, Effect::SharedMutation);
    assert_eq!(p[2].stability, Stability::Unknown);
    assert_eq!(p[2].resolved_type.as_deref(), Some("foreign::Opaque"));
}
#[test]
fn manual_impl_bounds_decide_generic_arguments() {
    let r = report(
        "#[derive(Clone)] struct NoEq; #[derive(Clone)] struct Wrap<T>(T); impl<T: PartialEq> PartialEq for Wrap<T> { fn eq(&self, o: &Self) -> bool { self.0 == o.0 } } struct Id<T>(T); impl<T> Clone for Id<T> { fn clone(&self) -> Self { todo!() } } impl<T> PartialEq for Id<T> { fn eq(&self, _: &Self) -> bool { true } } #[composable] fn View(a: Wrap<i32>, b: Wrap<NoEq>, c: Id<NoEq>) {}",
    );
    let p = &r.composables[0].parameters;
    assert_eq!(p[0].stability, Stability::Stable);
    assert_eq!(p[1].stability, Stability::Unknown);
    assert!(p[1].reason.contains("app::NoEq"), "{}", p[1].reason);
    assert_eq!(p[2].stability, Stability::Stable);
}
#[test]
fn cfg_variants_are_combined_when_their_derives_agree() {
    assert_eq!(
        states(
            "#[cfg(unix)] #[derive(Clone, PartialEq)] struct A(u8); #[cfg(not(unix))] #[derive(Clone, PartialEq)] struct A(u16); \
             #[cfg(unix)] #[derive(Clone, PartialEq)] struct B(u8); #[cfg(not(unix))] #[derive(Clone)] struct B(u16); \
             #[derive(Clone, PartialEq)] struct C(u8); #[cfg(any())] struct C(std::sync::Mutex<u8>); \
             #[composable] fn View(a: A, b: B, c: C) {}"
        ),
        vec![Stability::Stable, Stability::Unknown, Stability::Stable]
    );
}
#[test]
fn supertraits_supply_generic_contracts() {
    assert_eq!(
        states(
            "trait Model: Clone + PartialEq {} trait Loose {} #[composable] fn View<T: Model, U: Loose>(a: T, b: U) {}"
        ),
        vec![Stability::Stable, Stability::Unknown]
    );
}
#[test]
fn standard_value_types_are_known() {
    let r = report(
        "use std::{cell::OnceCell, cmp::{Ordering, Reverse}, collections::{BTreeSet, HashMap}, net::IpAddr, num::{NonZeroU32, Wrapping}, ops::Range, path::{Path, PathBuf}, rc::Rc, sync::Arc, time::{Duration, SystemTime}}; \
         #[composable] fn View(a: char, b: Rc<str>, c: Arc<str>, d: Range<u32>, e: NonZeroU32, f: Ordering, g: IpAddr, h: SystemTime, i: Wrapping<u8>, j: Duration, k: PathBuf, l: [u8; 4], m: (i32, String), n: OnceCell<i32>, o: HashMap<String, i32>, p: BTreeSet<u8>, q: Box<Path>, r: Reverse<i64>, s: std::sync::atomic::Ordering, t: Option<fn(i32) -> i32>) {}",
    );
    for p in &r.composables[0].parameters {
        assert_eq!(
            p.stability,
            Stability::Stable,
            "{}: {}",
            p.type_text,
            p.reason
        );
    }
}
#[test]
fn standard_types_without_equality_are_incompatible() {
    let r = report(
        "use std::{collections::BinaryHeap, rc::{Rc, Weak}, sync::atomic::AtomicBool}; #[composable] fn View(a: Weak<i32>, b: BinaryHeap<i32>, c: Rc<dyn Fn()>, d: AtomicBool) {}",
    );
    for p in &r.composables[0].parameters {
        assert_eq!(
            p.stability,
            Stability::Incompatible,
            "{}: {}",
            p.type_text,
            p.reason
        );
    }
}
#[test]
fn path_attribute_modules_resolve_their_items() {
    let file = |path: &str, module: &[&str], source: &str| SourceFile {
        path: path.into(),
        source: source.into(),
        crate_name: "app".into(),
        module: module.iter().map(|m| m.to_string()).collect(),
        crate_aliases: Default::default(),
    };
    let r = analyze(
        &[
            file("src/lib.rs", &[], "mod model; mod feature;"),
            file(
                "src/model.rs",
                &["model"],
                "#[derive(Clone, PartialEq)] pub struct Body;",
            ),
            file(
                "src/feature.rs",
                &["feature"],
                "use std::rc::Rc; #[cfg(test)] #[path = \"tests/feature_tests.rs\"] mod tests;",
            ),
            file(
                "src/tests/feature_tests.rs",
                &["tests", "feature_tests"],
                "use super::*; #[composable] fn Helper(a: Rc<i32>) {}",
            ),
            file(
                "runners/demo.rs",
                &["runners", "demo"],
                "#[path = \"../src/model.rs\"] mod model; use model::Body; #[composable] fn View(a: Body) {}",
            ),
        ],
        &Config::default(),
    );
    assert_eq!(r.composables.len(), 2);
    for c in &r.composables {
        assert_eq!(
            c.parameters[0].stability,
            Stability::Stable,
            "{}",
            c.parameters[0].reason
        );
    }
    assert!(
        r.composables
            .iter()
            .any(|c| c.qualified_name == "app::feature::tests::Helper")
    );
}
#[test]
fn function_body_imports_scope_nested_composables() {
    let r = report(
        "#[test] fn scenario() { use std::rc::Rc; #[composable] fn Reader(a: Rc<i32>, b: Local) {} #[derive(Clone, PartialEq)] struct Local; }",
    );
    assert_eq!(r.composables[0].qualified_name, "app::Reader");
    assert_eq!(
        r.composables[0]
            .parameters
            .iter()
            .map(|p| p.stability)
            .collect::<Vec<_>>(),
        vec![Stability::Stable; 2]
    );
}
#[test]
fn composable_reexported_by_cranpose_ui_is_recognized() {
    assert_eq!(
        report("use cranpose_ui::composable; #[composable] fn View(a: i32) {}")
            .composables
            .len(),
        1
    );
}
#[test]
fn statement_errors_inside_bodies_keep_badges() {
    let r = report("#[composable] fn View(a: i32) { let x = ; }");
    assert!(r.diagnostics.is_empty());
    assert_eq!(r.composables[0].parameters[0].stability, Stability::Stable);
}
#[test]
fn unresolved_names_explain_what_was_searched() {
    let r = crates(&[
        ("ui", "", "pub mod prelude { pub struct Text; }"),
        (
            "app",
            "",
            "use ui::prelude::*; #[composable] fn View(a: Missing) {}",
        ),
    ]);
    let a = parameter(&r, "a");
    assert_eq!(a.stability, Stability::Unknown);
    assert!(
        a.reason.contains("`use ui::prelude::*` does not export it"),
        "{}",
        a.reason
    );
    assert!(a.advice.contains("Import Missing"), "{}", a.advice);
}
