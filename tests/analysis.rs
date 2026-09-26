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
fn manual_equality_requires_review() {
    let r = report(
        "#[derive(Clone)] struct Model(i32); impl PartialEq for Model{fn eq(&self,_:&Self)->bool{false}} #[composable] fn View(a: Model) {}",
    );
    assert_eq!(r.composables[0].parameters[0].stability, Stability::Unknown);
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
