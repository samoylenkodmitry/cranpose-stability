# Cranpose Stability

A Rust library, command-line linter, and GitHub Action for composable parameter stability. The Cranpose IntelliJ plugin uses the same engine for inline badges.

It reads source and Cargo manifests. It does not run Cargo, expand application macros, execute build scripts, rewrite sources, or add application dependencies. Application release builds are unaffected.

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
| Stable | A source-visible value-comparison contract. Equal values may skip this check. |
| Unstable: callback | Cranpose marks this callback changed on every parent invocation. |
| Unstable: shared mutation | A retained clone shares interior-mutable storage; mutation may be missed. |
| Unknown | The type contract requires compiler resolution, macro expansion, or manual review. |
| Incompatible | A known type lacks the required comparison-slot traits. |
| No skip | Parameter comparison is disabled for this function. |

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

Rust syntax could not be parsed. Results for that file are incomplete.

### CP001

A callback parameter forces the macro's changed flag. Includes function pointers, directly recognized boxed callbacks and generic function bounds. This matches Cranpose's macro dispatch, rather than assuming closures are stable.

### CP002

An unsupported `impl Trait` parameter disables skipping for the entire function. Includes nonzero-argument opaque callbacks and opaque value parameters. Explicit `#[composable(no_skip)]` is reported as intentional and produces no lint warning.

### CP003

Shared ownership or a reference aliases interior-mutable data. A clone such as `Rc<RefCell<T>>` may compare against the same newly mutated storage. An owned `RefCell<T>` takes a value snapshot and is treated differently. Cranpose snapshot-state handles compare identity and separately track state reads.

### CP004

A known incompatible parameter, such as `&mut T` or an owned mutex, cannot satisfy the ordinary parameter slot's `Clone + PartialEq` contract. Rust's compiler remains authoritative.

### CP005

An external, generated, recursive, ambiguous, or manually implemented contract is unknown. Local derived structs/enums, type aliases, explicit generic bounds, standard value containers and supported imports are analyzed recursively. The tool never silently labels an unresolved external type stable.

## Shared configuration

Place `cranpose-stability.toml` in the project root. The CLI and IDE read the same file.

```toml
exclude = ["generated/**", "vendor/**"]
# Explicit assertions, not verified facts. Use fully qualified type paths.
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
      - uses: samoylenkodmitry/cranpose-stability@main
        with:
          path: .
          deny-unknown: 'false'
```

Pin the action to a reviewed commit in production workflows. The action builds the tool in the runner's temporary directory and emits GitHub annotations with source locations. It does not compile your application. JSON and SARIF output are also available from the CLI.

## Editor protocol

The library exports `analyze` for supplied source files and `project::analyze_project` for a disk project. The CLI's `--stdin-json --format json` reads a `ProjectRequest`:

```json
{
  "root": "/absolute/project",
  "overlays": [{"path": "src/lib.rs", "source": "unsaved Rust source"}],
  "only": ["src/lib.rs"]
}
```

Overlays replace disk content only in memory. Source ranges include byte offsets, UTF-16 offsets and one-based UTF-16 line/column positions. Reports have `schemaVersion: 1`. `only` filters output after project context is analyzed. The plugin embeds this library in its existing native executable; no separate linter installation is required there.

## Scope and policy

Policy `cranpose-param-state-v1` follows the framework's [composable expansion](https://github.com/samoylenkodmitry/Cranpose/blob/e177b19985c303a13fcf40d61decc7253fdbe057/crates/cranpose-macros/src/lib.rs) and [parameter slots](https://github.com/samoylenkodmitry/Cranpose/blob/e177b19985c303a13fcf40d61decc7253fdbe057/crates/cranpose-core/src/callbacks.rs). Runtime contract tests use that pinned framework revision.

This is source analysis on stable Rust, not a rustc compiler plugin. It does not select Cargo features/targets, expand `cfg_attr`, follow generated includes, or evaluate custom trait implementations. It analyzes all discovered Rust source files except excluded directories. Conventional `src/` module paths, inline modules, type aliases, renamed imports and renamed Cargo dependencies are supported. Nonstandard `#[path]` layouts and duplicate cfg/target declarations can remain unknown. Dependencies outside the source set remain unknown unless explicitly configured.

Limits: 20,000 Rust files and 4 MiB per source file. Symlinked directories are not traversed. Build/cache directories are excluded by default.

## Development

```sh
cargo fmt --all --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Tests cover analysis, Unicode editor offsets, unsaved overlays, CI exit codes, annotation escaping, source isolation and the real Cranpose comparison runtime.

## Credits

Apache-2.0. Built for [Cranpose](https://github.com/samoylenkodmitry/Cranpose).
The inline stability presentation was inspired by [Compose Stability Analyzer](https://github.com/skydoves/compose-stability-analyzer); the analysis rules here are specific to Rust and Cranpose.
