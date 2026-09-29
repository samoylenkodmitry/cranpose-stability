use cranpose_stability::project::{Overlay, ProjectRequest, analyze_project};
use std::{fs, process::Command};
fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::create_dir(dir.path().join("src")).expect("src");
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname=\"sample\"\nversion=\"0.1.0\"\n",
    )
    .expect("manifest");
    fs::write(
        dir.path().join("src/lib.rs"),
        "#[cranpose::composable] fn View(a: i32) {}",
    )
    .expect("source");
    dir
}
#[test]
fn unsaved_overlay_changes_analysis_and_leaves_disk_untouched() {
    let dir = fixture();
    let path = dir.path().join("src/lib.rs");
    let before = fs::read(&path).expect("read");
    let r = analyze_project(&ProjectRequest {
        root: dir.path().into(),
        overlays: vec![Overlay {
            path,
            source: "#[cranpose::composable] fn View(a:impl Fn()) {}".into(),
        }],
        only: vec![],
        cargo_home: Some(dir.path().join("no-cargo-home")),
    })
    .expect("report");
    assert_eq!(r.diagnostics[0].rule, "CP001");
    assert_eq!(
        fs::read(dir.path().join("src/lib.rs")).expect("read"),
        before
    );
    assert!(!dir.path().join("Cargo.lock").exists());
    assert!(!dir.path().join("target").exists());
}
#[test]
fn excludes_and_target_are_respected() {
    let dir = fixture();
    fs::create_dir(dir.path().join("target")).expect("target");
    fs::write(dir.path().join("target/broken.rs"), "fn (").expect("broken");
    let r = analyze_project(&ProjectRequest {
        root: dir.path().into(),
        overlays: vec![],
        only: vec![],
        cargo_home: Some(dir.path().join("no-cargo-home")),
    })
    .expect("report");
    assert!(r.diagnostics.is_empty());
}
#[test]
fn cli_exit_codes_and_json_are_ci_ready() {
    let dir = fixture();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_cranpose-stability"))
            .env("CARGO_HOME", dir.path().join("no-cargo-home"))
            .args([
                "--root",
                dir.path().to_str().expect("utf8"),
                "--format",
                "json",
            ])
            .output()
            .expect("run")
    };
    let clean = invoke();
    assert_eq!(clean.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&clean.stdout).expect("json")["schemaVersion"],
        1
    );
    fs::write(
        dir.path().join("src/lib.rs"),
        "#[composable] fn View(a:impl Fn()) {}",
    )
    .expect("write");
    assert_eq!(invoke().status.code(), Some(1));
    fs::write(dir.path().join("src/lib.rs"), "fn (").expect("write");
    assert_eq!(invoke().status.code(), Some(2));
}
#[test]
fn malformed_config_is_an_error() {
    let dir = fixture();
    fs::write(
        dir.path().join("cranpose-stability.toml"),
        "unknown_setting=true",
    )
    .expect("write");
    assert!(
        analyze_project(&ProjectRequest {
            root: dir.path().into(),
            overlays: vec![],
            only: vec![],
            cargo_home: Some(dir.path().join("no-cargo-home")),
        })
        .is_err()
    );
}

#[test]
fn renamed_cranpose_dependency_and_macro_import_are_supported() {
    let dir = fixture();
    fs::write(dir.path().join("Cargo.toml"),"[package]\nname=\"sample\"\nversion=\"0.1.0\"\n[dependencies]\nui={package=\"cranpose\",version=\"*\"}\n").expect("manifest");
    fs::write(
        dir.path().join("src/lib.rs"),
        "use ui::composable as component; #[component] fn View(a:impl Fn()) {}",
    )
    .expect("source");
    let r = analyze_project(&ProjectRequest {
        root: dir.path().into(),
        overlays: vec![],
        only: vec![],
        cargo_home: Some(dir.path().join("no-cargo-home")),
    })
    .expect("report");
    assert_eq!(r.composables.len(), 1);
    assert_eq!(r.diagnostics[0].rule, "CP001");
}
