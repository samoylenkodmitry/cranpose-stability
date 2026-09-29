//! Dependency sources are read from a fake CARGO_HOME, never from the machine's own cache.
use cranpose_stability::{
    Effect, Parameter, Report, Stability,
    project::{Overlay, ProjectRequest, analyze_project},
};
use std::{fs, path::PathBuf, process::Command};

const REGISTRY: &str = "home/registry/src/index.crates.io-0000000000000000";
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

struct Fixture(tempfile::TempDir);
impl Fixture {
    fn new(manifest: &str, lib: &str) -> Self {
        let f = Self(tempfile::tempdir().expect("tempdir"));
        f.write("app/Cargo.toml", manifest)
            .write("app/src/lib.rs", lib);
        f
    }
    fn path(&self, relative: &str) -> PathBuf {
        self.0.path().join(relative)
    }
    fn write(&self, relative: &str, text: &str) -> &Self {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("directories");
        fs::write(path, text).expect("write");
        self
    }
    /// An unpacked crate in `dir` with the given library sources.
    fn package(&self, dir: &str, name: &str, version: &str, files: &[(&str, &str)]) -> &Self {
        self.write(
            &format!("{dir}/Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        );
        for (file, text) in files {
            self.write(&format!("{dir}/src/{file}"), text);
        }
        self
    }
    /// A crates.io package unpacked in the fake CARGO_HOME, with its registry dependencies.
    fn registry(&self, name: &str, version: &str, deps: &[&str], files: &[(&str, &str)]) -> &Self {
        let dir = format!("{REGISTRY}/{name}-{version}");
        self.package(&dir, name, version, files);
        let deps: String = deps
            .iter()
            .map(|d| format!("[dependencies.{d}]\nversion = \"*\"\n"))
            .collect();
        self.write(
            &format!("{dir}/Cargo.toml"),
            &format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n\n{deps}"),
        )
    }
    /// Cargo.lock entries: (name, version, source, dependencies).
    fn lock(&self, packages: &[(&str, &str, &str, &[&str])]) -> &Self {
        let mut text = String::from("version = 4\n");
        for (name, version, source, deps) in packages {
            text.push_str(&format!(
                "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n"
            ));
            if !source.is_empty() {
                text.push_str(&format!("source = \"{source}\"\n"));
            }
            if !deps.is_empty() {
                text.push_str("dependencies = [\n");
                for d in *deps {
                    text.push_str(&format!(" \"{d}\",\n"));
                }
                text.push_str("]\n");
            }
        }
        self.write("app/Cargo.lock", &text)
    }
    fn request(&self) -> ProjectRequest {
        ProjectRequest {
            root: self.path("app"),
            overlays: vec![],
            only: vec![],
            cargo_home: Some(self.path("home")),
        }
    }
    fn analyze(&self) -> Report {
        analyze_project(&self.request()).expect("report")
    }
}
fn parameter<'a>(r: &'a Report, name: &str) -> &'a Parameter {
    r.composables
        .iter()
        .flat_map(|c| &c.parameters)
        .find(|p| p.name == name)
        .expect("parameter")
}
fn manifest(deps: &str) -> String {
    format!("[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n{deps}")
}

#[test]
fn registry_types_resolve_through_reexports_and_globs() {
    let f = Fixture::new(
        &manifest("kit = \"1\"\n"),
        "use kit::prelude::*;\n#[composable] fn View(a: Style, b: bool, c: String, d: Padding, e: kit::Color) {}",
    );
    f.registry(
        "kit",
        "1.2.0",
        &["paint"],
        &[(
            "lib.rs",
            "pub use paint::Color;\npub mod prelude { pub use crate::layout::*; pub use paint::*; }\nmod layout;",
        ), ("layout.rs", "#[derive(Clone, Copy, PartialEq)] pub struct Padding(pub f32);")],
    )
    .registry(
        "paint",
        "0.3.1",
        &[],
        &[
            ("lib.rs", "mod style;\npub use style::{Color, Style};"),
            (
                "style.rs",
                "#[derive(Clone, Debug, PartialEq)] pub struct Style { pub color: Color, pub name: String }\n#[derive(Clone, Copy, PartialEq)] pub struct Color(pub u32);",
            ),
        ],
    )
    .lock(&[
        ("app", "0.1.0", "", &["kit"]),
        ("kit", "1.2.0", CRATES_IO, &["paint"]),
        ("paint", "0.3.1", CRATES_IO, &[]),
    ]);
    let r = f.analyze();
    for name in ["a", "b", "c", "d", "e"] {
        let p = parameter(&r, name);
        assert_eq!(p.stability, Stability::Stable, "{name}: {}", p.reason);
    }
    let a = parameter(&r, "a");
    assert_eq!(a.resolved_type.as_deref(), Some("paint::style::Style"));
    assert!(a.reason.contains("paint 0.3.1"), "{}", a.reason);
    assert_eq!(
        parameter(&r, "d").resolved_type.as_deref(),
        Some("kit::layout::Padding")
    );
}
#[test]
fn stable_types_match_public_reexport_paths() {
    let f = Fixture::new(
        &manifest("kit = \"1\"\n"),
        "use kit::Opaque;\n#[composable] fn View(a: Opaque) {}",
    );
    f.registry(
        "kit",
        "1.0.0",
        &[],
        &[(
            "lib.rs",
            "mod inner { pub struct Opaque; }\npub use inner::Opaque;",
        )],
    )
    .lock(&[
        ("app", "0.1.0", "", &["kit"]),
        ("kit", "1.0.0", CRATES_IO, &[]),
    ])
    .write(
        "app/cranpose-stability.toml",
        "stable_types = [\"kit::Opaque\"]\n",
    );
    let a = parameter(&f.analyze(), "a").clone();
    assert_eq!(a.stability, Stability::Stable, "{}", a.reason);
    assert!(a.reason.contains("User-declared"));
}
#[test]
fn git_checkout_workspace_members_are_found_by_name() {
    let rev = "abc1234def5678901234567890abcdef12345678";
    let f = Fixture::new(
        &manifest(
            "widgets = { git = \"https://github.com/example/Widgets\", rev = \"abc1234\" }\n",
        ),
        "use widgets::Theme;\n#[composable] fn View(a: Theme) {}",
    );
    let checkout = "home/git/checkouts/widgets-0123456789abcdef/abc1234";
    f.write(
        &format!("{checkout}/Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    )
    .package(
        &format!("{checkout}/crates/widgets"),
        "widgets",
        "0.4.0",
        &[("lib.rs", "pub use widgets_core::theme::Theme;")],
    )
    .package(
        &format!("{checkout}/crates/widgets-core"),
        "widgets-core",
        "0.4.0",
        &[
            ("lib.rs", "pub mod theme;"),
            ("theme.rs", "#[derive(Clone, PartialEq)] pub struct Theme { pub dark: bool }"),
        ],
    )
    .write(
        &format!("{checkout}/crates/widgets/Cargo.toml"),
        "[package]\nname = \"widgets\"\nversion = \"0.4.0\"\n[dependencies]\nwidgets-core = { path = \"../widgets-core\" }\n",
    );
    let source = format!("git+https://github.com/example/Widgets?rev=abc1234#{rev}");
    f.lock(&[
        ("app", "0.1.0", "", &["widgets"]),
        ("widgets", "0.4.0", &source, &["widgets-core"]),
        ("widgets-core", "0.4.0", &source, &[]),
    ]);
    let a = parameter(&f.analyze(), "a").clone();
    assert_eq!(a.stability, Stability::Stable, "{}", a.reason);
    assert_eq!(
        a.resolved_type.as_deref(),
        Some("widgets_core::theme::Theme")
    );
    assert!(a.reason.contains("widgets-core 0.4.0"), "{}", a.reason);
}
#[test]
fn missing_dependency_source_is_explained() {
    let f = Fixture::new(
        &manifest("kit = \"1\"\n"),
        "use kit::Theme;\n#[composable] fn View(a: Theme) {}",
    );
    f.lock(&[
        ("app", "0.1.0", "", &["kit"]),
        ("kit", "1.4.2", CRATES_IO, &[]),
    ]);
    let a = parameter(&f.analyze(), "a").clone();
    assert_eq!(a.stability, Stability::Unknown);
    assert!(a.reason.contains("kit 1.4.2"), "{}", a.reason);
    assert!(a.reason.contains("cargo fetch"), "{}", a.reason);
    assert!(a.advice.contains("stable_types"), "{}", a.advice);
    assert_eq!(a.resolved_type.as_deref(), Some("kit::Theme"));
}
#[test]
fn prelude_names_survive_a_glob_from_a_missing_dependency() {
    let f = Fixture::new(
        &manifest("kit = \"1\"\n"),
        "use kit::prelude::*;\n#[composable] fn View(a: bool, b: String, c: Theme) {}",
    );
    f.lock(&[
        ("app", "0.1.0", "", &["kit"]),
        ("kit", "1.4.2", CRATES_IO, &[]),
    ]);
    let r = f.analyze();
    assert_eq!(parameter(&r, "a").stability, Stability::Stable);
    assert_eq!(parameter(&r, "b").stability, Stability::Stable);
    let c = parameter(&r, "c");
    assert_eq!(c.stability, Stability::Unknown);
    assert!(c.reason.contains("use kit::prelude::*"), "{}", c.reason);
    assert!(c.reason.contains("cargo fetch"), "{}", c.reason);
}
#[test]
fn shared_mutation_is_found_inside_dependency_types() {
    let f = Fixture::new(
        &manifest("kit = \"1\"\n"),
        "use kit::*;\n#[composable] fn View(a: Store, b: Handle, c: Wrapper<Store>) {}",
    );
    f.registry(
        "kit",
        "1.0.0",
        &[],
        &[
            (
                "lib.rs",
                "use std::{cell::RefCell, rc::Rc};\n\
                 mod store;\nmod handle_eq;\npub use store::Store;\n\
                 #[derive(Clone)] pub struct Handle { inner: Rc<RefCell<Vec<u8>>> }\n\
                 #[derive(Clone, PartialEq)] pub struct Wrapper<T> { pub value: T }\n",
            ),
            (
                "store.rs",
                "use std::{cell::RefCell, rc::Rc};\n#[derive(Clone, PartialEq)] pub struct Store { items: Rc<RefCell<Vec<u8>>> }",
            ),
            (
                // The manual impl lives in a module nothing else needs, so it is found by scanning.
                "handle_eq.rs",
                "use super::Handle;\nimpl PartialEq for Handle { fn eq(&self, o: &Self) -> bool { std::rc::Rc::ptr_eq(&self.inner, &o.inner) } }",
            ),
        ],
    )
    .lock(&[("app", "0.1.0", "", &["kit"]), ("kit", "1.0.0", CRATES_IO, &[])]);
    let r = f.analyze();
    let a = parameter(&r, "a");
    assert_eq!(a.effect, Effect::SharedMutation, "{}", a.reason);
    assert_eq!(a.rule.as_deref(), Some("CP003"));
    // A dependency's manual PartialEq defines its identity semantics.
    let b = parameter(&r, "b");
    assert_eq!(b.stability, Stability::Stable, "{}", b.reason);
    assert!(b.reason.contains("manual PartialEq"), "{}", b.reason);
    assert_eq!(parameter(&r, "c").effect, Effect::SharedMutation);
}
#[test]
fn cranpose_state_handles_resolve_through_reexports() {
    let f = Fixture::new(
        &manifest("cranpose = \"0.1\"\nrt = { package = \"cranpose-core\", version = \"0.1\" }\n"),
        "use cranpose::prelude::*;\n#[composable] fn View(a: MutableState<Vec<u8>>, b: rt::State<String>, c: Dp) {}",
    );
    f.registry(
        "cranpose",
        "0.1.9",
        &["cranpose-core", "cranpose-ui"],
        &[("lib.rs", "pub mod prelude { pub use cranpose_core::{MutableState, State}; pub use cranpose_ui::*; }")],
    )
    .registry(
        "cranpose-core",
        "0.1.9",
        &[],
        &[
            ("lib.rs", "mod state;\npub use state::{MutableState, State};"),
            (
                "state.rs",
                "use std::{marker::PhantomData, rc::Rc, cell::RefCell};\n\
                 pub struct MutableState<T> { id: u32, cache: Rc<RefCell<Option<T>>> }\n\
                 pub struct State<T> { id: u32, _m: PhantomData<T> }\n",
            ),
        ],
    )
    .registry(
        "cranpose-ui",
        "0.1.9",
        &[],
        &[
            ("lib.rs", "#[macro_use] mod units;\npub use units::*;"),
            (
                "units.rs",
                "macro_rules! unit { ($name:ident) => { #[derive(Clone, Copy, PartialEq)] pub struct $name(pub f32); }; }\nunit!(Dp);",
            ),
        ],
    )
    .lock(&[
        ("app", "0.1.0", "", &["cranpose", "cranpose-core"]),
        ("cranpose", "0.1.9", CRATES_IO, &["cranpose-core", "cranpose-ui"]),
        ("cranpose-core", "0.1.9", CRATES_IO, &[]),
        ("cranpose-ui", "0.1.9", CRATES_IO, &[]),
    ]);
    let r = f.analyze();
    for name in ["a", "b"] {
        let p = parameter(&r, name);
        assert_eq!(p.stability, Stability::Stable, "{name}: {}", p.reason);
        assert!(p.reason.contains("Cranpose state handle"), "{}", p.reason);
    }
    assert_eq!(
        parameter(&r, "a").resolved_type.as_deref(),
        Some("cranpose_core::state::MutableState")
    );
    let c = parameter(&r, "c");
    assert_eq!(c.stability, Stability::Stable, "{}", c.reason);
    assert_eq!(c.resolved_type.as_deref(), Some("cranpose_ui::units::Dp"));
}
#[test]
fn cfg_gated_dependency_declarations_and_test_modules() {
    let f = Fixture::new(
        &manifest("kit = \"1\"\n"),
        "#[composable] fn View(a: kit::Surface, b: kit::Mixed) {}",
    );
    f.registry(
        "kit",
        "1.0.0",
        &[],
        &[(
            "lib.rs",
            "#[cfg(unix)] mod imp { #[derive(Clone, PartialEq)] pub struct Surface(pub u32); }\n\
             #[cfg(windows)] #[path = \"win.rs\"] mod imp;\n\
             pub use imp::Surface;\n\
             #[cfg(unix)] #[derive(Clone, PartialEq)] pub struct Mixed(u8);\n\
             #[cfg(not(unix))] #[derive(Clone)] pub struct Mixed(u16);\n\
             #[cfg(test)] mod tests { impl PartialEq for super::Mixed { fn eq(&self, _: &Self) -> bool { true } } }\n",
        ), (
            "win.rs",
            "#[derive(Clone, PartialEq)] pub struct Surface(pub u64);",
        )],
    )
    .lock(&[("app", "0.1.0", "", &["kit"]), ("kit", "1.0.0", CRATES_IO, &[])]);
    let r = f.analyze();
    let a = parameter(&r, "a");
    assert_eq!(a.stability, Stability::Stable, "{}", a.reason);
    let b = parameter(&r, "b");
    assert_eq!(b.stability, Stability::Unknown);
    assert!(b.reason.contains("cfg-dependent"), "{}", b.reason);
}
#[test]
fn path_dependencies_and_cargo_config_patches_are_followed() {
    let f = Fixture::new(
        &manifest("shapes = { path = \"../shapes\" }\nicons = \"2\"\n"),
        "#[composable] fn View(a: shapes::Circle, b: icons::Icon) {}",
    );
    f.package(
        "shapes",
        "shapes",
        "0.1.0",
        &[(
            "lib.rs",
            "#[derive(Clone, PartialEq)] pub struct Circle { pub r: f32 }",
        )],
    )
    .package(
        "local-icons",
        "icons",
        "2.0.0",
        &[(
            "lib.rs",
            "#[derive(Clone, Copy, PartialEq)] pub enum Icon { Add, Remove }",
        )],
    )
    .write(
        "app/.cargo/config.toml",
        "[patch.crates-io]\nicons = { path = \"../local-icons\" }\n",
    )
    .lock(&[
        ("app", "0.1.0", "", &["icons", "shapes"]),
        ("icons", "2.0.0", "", &[]),
        ("shapes", "0.1.0", "", &[]),
    ]);
    let r = f.analyze();
    for name in ["a", "b"] {
        let p = parameter(&r, name);
        assert_eq!(p.stability, Stability::Stable, "{name}: {}", p.reason);
    }
}
#[test]
fn editor_requests_match_the_command_line() {
    let f = Fixture::new(
        &manifest("kit = \"1\"\n"),
        "use kit::Theme;\n#[composable] fn View(a: Theme, b: i32) {}",
    );
    f.registry(
        "kit",
        "1.0.0",
        &[],
        &[("lib.rs", "#[derive(Clone, PartialEq)] pub struct Theme;")],
    )
    .lock(&[
        ("app", "0.1.0", "", &["kit"]),
        ("kit", "1.0.0", CRATES_IO, &[]),
    ])
    .write("app/src/other.rs", "#[composable] fn Other(a: u8) {}");
    let cli = Command::new(env!("CARGO_BIN_EXE_cranpose-stability"))
        .env("CARGO_HOME", f.path("home"))
        .args([
            "--root",
            f.path("app").to_str().expect("utf8"),
            "--format",
            "json",
        ])
        .output()
        .expect("run");
    let cli: Report = serde_json::from_slice(&cli.stdout).expect("json");
    let editor = analyze_project(&ProjectRequest {
        overlays: vec![Overlay {
            path: "src/lib.rs".into(),
            source: fs::read_to_string(f.path("app/src/lib.rs")).expect("read"),
        }],
        only: vec!["src/lib.rs".into()],
        ..f.request()
    })
    .expect("report");
    let lib = |r: &Report| {
        serde_json::to_value(
            r.composables
                .iter()
                .filter(|c| c.path == "src/lib.rs")
                .collect::<Vec<_>>(),
        )
        .expect("json")
    };
    assert_eq!(lib(&cli), lib(&editor));
    assert_eq!(editor.composables.len(), 1);
    assert_eq!(
        editor.composables[0].parameters[0].stability,
        Stability::Stable
    );
}
