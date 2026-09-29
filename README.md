# Cranpose Stability

A Rust library, command-line linter, and GitHub Action for composable parameter stability. The Cranpose IntelliJ plugin uses the same engine for inline badges.

It reads source, Cargo manifests and `Cargo.lock`, and it reads dependency sources that Cargo has already unpacked under `CARGO_HOME`. It does not run Cargo, fetch anything, expand procedural macros, execute build scripts, rewrite sources, or add application dependencies. Application release builds are unaffected.

## Install and run

```sh
cargo install --git https://github.com/samoylenkodmitry/cranpose-stability --locked
cranpose-stability --root .
cranpose-stability --root . --format json
cranpose-stability --root . --format sarif --output stability.sarif
```

Exit codes: **0** means no failing findings; **1** means a stability warning/error; **2** means invalid input, syntax, configuration, or a tool failure. Unknown type contracts are informational by default. Use `--deny-unknown` to fail on them too.

## What the badges mean

Cranpose does not use Jetpack Compose's stability rules. Its ordinary parameter slots clone values and compare them with `PartialEq`. Recognized callback parameters always mark the invocation changed. Certain `impl Trait` parameters select the non-skipping expansion for the whole function.

| Result | Meaning |
| --- | --- |
| Stable | A source-visible `Clone + PartialEq` contract: derived, manually implemented, a known standard type, a Cranpose state handle, or a `stable_types` entry. Equal values may skip this check. |
| Unstable: callback | Cranpose marks this callback changed on every parent invocation. |
| Unstable: shared mutation | A retained clone shares interior-mutable storage; mutation may be missed. |
| Unknown | The contract cannot be established from source. The reason names the type, the crate and what was missing. |
| Incompatible | A known type lacks the required comparison-slot traits. |
| No skip | Parameter comparison is disabled for this function. |

Every parameter carries a reason, advice and, when one type decided the result, `resolvedType`: the canonical path of that type, such as `cranpose_ui::modifier::Modifier`. Reasons for types from dependencies name the package and version, for example `cranpose_ui_graphics::color::Color (cranpose-ui-graphics 0.1.174) derives Clone + PartialEq and is compared by value.`

For example:

```rust
use std::{cell::RefCell, rc::Rc};
use cranpose::composable;

#[composable]
fn Card(
    title: String,                 // stable: compared by value
    labels: Vec<String>,           // stable: element equality, not Kotlin List rules
    on_click: impl Fn(),            // CP001: always changed
    shared: Rc<RefCell<i32>>,       // CP003: shared mutation may be missed
) {
    // ...
}
```

Stability is a static contract, not a performance measurement. A stable parameter does not guarantee that the composable skips. An intentional callback is often appropriate.

### CP000

Item-level Rust syntax could not be parsed. Results for that file are incomplete. Function bodies are not parsed (except for nested function items), so an unfinished statement inside a body does not hide the file's badges.

### CP001

A callback parameter forces the macro's changed flag. Includes function pointers, directly recognized boxed callbacks and generic function bounds. This matches Cranpose's macro dispatch, rather than assuming closures are stable.

### CP002

An unsupported `impl Trait` parameter disables skipping for the entire function. Includes nonzero-argument opaque callbacks and opaque value parameters. Explicit `#[composable(no_skip)]` is reported as intentional and produces no lint warning.

### CP003

Shared ownership or a reference aliases interior-mutable data. A clone such as `Rc<RefCell<T>>` may compare against the same newly mutated storage. An owned `RefCell<T>` takes a value snapshot and is treated differently. Write-once cells (`OnceCell`, `OnceLock`) are not reported. Cranpose snapshot-state handles compare identity and separately track state reads.

A project type with a manual `PartialEq` over shared interior-mutable storage, such as an identity handle comparing `Rc::ptr_eq`, is still reported: a mutation through that storage is not a parameter change. Allow CP003 for the parameter with a reason when that is intended. A dependency type with a manual `PartialEq` is stable: its crate defines what equality means for the state it shares.

### CP004

A known incompatible parameter, such as `&mut T` or an owned mutex, cannot satisfy the ordinary parameter slot's `Clone + PartialEq` contract. Rust's compiler remains authoritative.

### CP005

The contract could not be established from source. Each reason says why, and the advice says what to do:

- a dependency whose source is not unpacked in `CARGO_HOME` (reason names the package, version and location; run `cargo fetch`);
- a name that is not declared or imported (reason lists the glob imports that were searched);
- a type without a derived or manual `Clone` or `PartialEq` in source, for example one implemented by a procedural macro;
- a generic parameter without `Clone + PartialEq` bounds (supertraits of traits declared in source count);
- associated types, trait objects, type macros, and cfg variants whose derives disagree.

Derived and manual implementations are the contract: a type that derives `Clone + PartialEq` compiles only if its fields do, so fields are inspected for shared interior mutability, not re-proved. Generic arguments are checked against derive bounds and against the bounds of manual impls. The tool never silently labels an unresolved type stable; declare such types in `stable_types`.

## Shared configuration

Place `cranpose-stability.toml` in the project root. The CLI and IDE read the same file.

```toml
exclude = ["generated/**", "vendor/**"]
# Explicit assertions, not verified facts. Use a fully qualified type path: the
# canonical path or any public re-export of it, such as `resolvedType` shows.
stable_types = ["external_crate::ImmutableId"]

[[allow]]
path = "src/widgets.rs"
function = "Button"
parameter = "on_click"
rule = "CP001"
reason = "Intentional interaction boundary"
```

Suppressions require a reason. They remove diagnostics but preserve the IDE badge and explanation. `function` accepts a simple or fully qualified name; `parameter = "*"` matches all parameters of that function. Unknown configuration keys are errors.

## GitHub Actions

```yaml
name: Cranpose stability
on: [push, pull_request]
jobs:
  stability:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: samoylenkodmitry/cranpose-stability@v0.2.0
        with:
          path: .
          deny-unknown: 'false'
```

Pin the action to a release tag or a reviewed commit in production workflows. The action builds the tool in the runner's temporary directory and emits GitHub annotations with source locations. It does not compile your application. JSON and SARIF output are also available from the CLI.

## Editor protocol

The library exports `analyze` for supplied source files and `project::analyze_project` for a disk project. The CLI's `--stdin-json --format json` reads a `ProjectRequest`:

```json
{
  "root": "/absolute/project",
  "overlays": [{"path": "src/lib.rs", "source": "unsaved Rust source"}],
  "only": ["src/lib.rs"],
  "cargoHome": "/optional/cargo/home"
}
```

Overlays replace disk content only in memory. Source ranges include byte offsets, UTF-16 offsets and one-based UTF-16 line/column positions. Reports have `schemaVersion: 1`; `resolvedType` is an optional parameter field. `only` filters output after project context is analyzed. `cargoHome` defaults to `CARGO_HOME`, then `~/.cargo`. The plugin embeds this library in its existing native executable; no separate linter installation is required there.

## Scope and policy

Policy `cranpose-param-state-v1` follows the framework's [composable expansion](https://github.com/samoylenkodmitry/Cranpose/blob/e177b19985c303a13fcf40d61decc7253fdbe057/crates/cranpose-macros/src/lib.rs) and [parameter slots](https://github.com/samoylenkodmitry/Cranpose/blob/e177b19985c303a13fcf40d61decc7253fdbe057/crates/cranpose-core/src/callbacks.rs). Runtime contract tests use that pinned framework revision.

This is source analysis on stable Rust, not a rustc compiler plugin. It analyzes all discovered Rust source files except excluded directories. Project modules follow file layout; `mod` items that load other project files (`#[path]`, binaries and examples outside `src/`) are followed too. Imports, renames, glob imports with Rust's visibility and shadowing rules, the standard prelude, type aliases, function-body `use` items and renamed or `workspace = true` dependencies are resolved.

Dependencies are located offline. `Cargo.lock` and the package manifests map each dependency name to a package: path dependencies and `[patch]` overrides from the workspace manifest or `.cargo/config.toml`, registry packages under `CARGO_HOME/registry/src`, and git packages found by name inside `CARGO_HOME/git/checkouts/<repo>-<hash>/<short rev>`, including workspace members. A dependency's files are parsed only when a query needs them, starting at its library root and following `mod`, `pub use` and glob re-exports; `#[cfg(test)]` modules are skipped. Crate-local `macro_rules!` item macros, such as newtype generators, are expanded when their patterns match exactly.

It does not select Cargo features or targets: cfg-gated declarations of one name are combined when their derives agree, and a single declaration without `cfg` is preferred. It does not expand procedural macros other than the built-in `Clone`, `Copy` and `PartialEq` derives (including under `cfg_attr`), `cfg_if!`-style macros, generated includes, or vendored sources.

Limits: 20,000 Rust files and 4 MiB per source file. Symlinked directories are not traversed. Build/cache directories are excluded by default.

## Development

```sh
cargo fmt --all --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Tests cover analysis, dependency resolution against a fake `CARGO_HOME` (registry, git checkouts, path dependencies and patches), Unicode editor offsets, unsaved overlays, CI exit codes, annotation escaping, source isolation and the real Cranpose comparison runtime.

## Credits

Apache-2.0. Built for [Cranpose](https://github.com/samoylenkodmitry/Cranpose).
The inline stability presentation was inspired by [Compose Stability Analyzer](https://github.com/skydoves/compose-stability-analyzer); the analysis rules here are specific to Rust and Cranpose.
