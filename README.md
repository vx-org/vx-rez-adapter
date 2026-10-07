# vx-rez-adapter

Rust adapter integrating [vx](https://github.com/vx-org) with [rez-next](https://github.com/loonghao/rez-next) package environments.

`vx` needs to resolve a rez package environment and then run a tool inside it. This crate is that bridge: it turns rez package requests into a concrete environment, and launches a program with that environment applied.

## Status

**0.1.0 is a scaffolding release.** The module layout and the public API surface are in place and fully documented, but the two core verbs are not implemented yet — `RezAdapter::resolve_env` and `RezAdapter::launch` return `Error::NotImplemented`.

This is a staging choice, not a dependency blocker: the resolution backend will depend on the `rez-next-build` SDK, and `rez-next-build` **0.3.9 is already published on crates.io** (released 2026-10-07, not yanked). The crate ships its shape first so downstream consumers can compile against a stable surface, and stage 2 wires in the resolve and launch paths on top of it. The environment model itself (`EnvDelta`, `ResolvedEnv`) is implemented and tested, because it has no upstream dependency.

## Installation

This crate has **not been published to crates.io yet**, so a version requirement will not resolve. Until the first release, depend on it from git:

```toml
[dependencies]
vx-rez-adapter = { git = "https://github.com/vx-org/vx-rez-adapter" }
```

Once published, the registry form will be:

```toml
[dependencies]
vx-rez-adapter = "0.1.0"
```

Requires Rust 1.95.0 or newer (edition 2024).

## Usage

### Resolving an environment

```rust
use vx_rez_adapter::{RezAdapter, ResolveRequest};

let adapter = RezAdapter::new();
let request = ResolveRequest::new(["python-3.11", "maya-2024"]);

// Stage 2: returns the resolved environment.
// Until then: returns `Error::NotImplemented`.
let env = adapter.resolve_env(&request)?;
println!("PATH={}", env.environment["PATH"]);
```

### Launching a tool inside a resolved environment

```rust
use vx_rez_adapter::{LaunchRequest, RezAdapter, ResolveRequest};

let adapter = RezAdapter::new();
let env = adapter.resolve_env(&ResolveRequest::new(["maya-2024"]))?;

// Stage 2: spawns the program with `env.environment` applied.
// Until then: returns `Error::NotImplemented`.
let outcome = adapter.launch(
    &LaunchRequest::new("maya")
        .args(["-batch", "-file", "scene.ma"])
        .environment(env.environment),
)?;

assert!(outcome.success());
```

### Working with the environment model

The environment model is available today and needs no resolve:

```rust
use std::collections::BTreeMap;
use vx_rez_adapter::{EnvAction, EnvDelta, ResolvedEnv, env_key, path_separator};

let sep = path_separator();
let mut delta = EnvDelta::new();

// Every package in a resolve emits its own commands, so a delta holds a *list*
// of actions per variable. These two both survive:
delta.push_action("PATH", EnvAction::Prepend("/pkg/a/bin".to_string(), sep.to_string()));
delta.push_action("PATH", EnvAction::Prepend("/pkg/b/bin".to_string(), sep.to_string()));
delta.push_action("REZ_USED", EnvAction::Set("1".to_string()));
delta.push_action("OLD_VAR", EnvAction::Unset);

let parent = BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
let resolved = ResolvedEnv::from_delta(delta, &parent);

// Later prepends land in front, matching rez activation order.
let path = &resolved.environment["PATH"];
assert!(path.starts_with(&format!("/pkg/b/bin{sep}/pkg/a/bin{sep}")));
```

A resolve in rez is a *difference* against a parent environment, not a bare set
of variables — paths get prepended, variables get overridden, some get unset.
`EnvDelta` models that difference with five actions (`Unset`, `Set`, `Prepend`,
`Append`, `SetIfEmpty`), and `ResolvedEnv` keeps both the rendered environment
and the delta, so a caller can diff a resolve or re-apply it against a different
parent.

Three properties matter for correctness, and each is covered by tests:

- **Multiple actions per variable.** Nearly every rez package prepends to
  `PATH`. A delta that kept only the last action for a variable would silently
  drop the others. Actions accumulate in insertion order, with later prepends
  landing in front of earlier ones.
- **Per-action separator.** `Prepend` and `Append` carry their own separator
  (`(value, separator)`), matching the upstream `rez-next-context` contract,
  rather than inferring one from the host platform.
- **Case-folded keys on Windows.** Windows environment variables are
  case-insensitive, so `Path` and `PATH` name one variable. Build keys with
  `env_key` to avoid handing a child two competing definitions.

## Public API

| Type | Purpose |
| --- | --- |
| `RezAdapter` | Top-level entry point: `resolve_env` and `launch` |
| `ResolveRequest` | Package requests plus optional implicit requests and package paths |
| `EnvDelta` | Ordered set of variable changes a resolve applies |
| `EnvAction` | One change: `Unset`, `Set`, `Prepend`, `Append`, `SetIfEmpty` |
| `ResolvedEnv` | Rendered environment, the delta behind it, and package roots |
| `LaunchRequest` | Program, arguments, environment, and working directory |
| `LaunchOutcome` | Exit code and terminating signal of a finished child |
| `Environment` | Alias for `BTreeMap<String, String>`; sorted for deterministic output |
| `env_key` | Normalizes a variable name for use as a map key |
| `path_separator` | Host path list separator (`:` on unix, `;` on Windows) |
| `Error` / `Result` | Adapter error and result types |

The data-oriented public types are `#[non_exhaustive]`, so new fields and
variants can be added as stage 2 lands without breaking downstream code.

## Roadmap (stage 2)

1. Implement `resolve_env` against the `rez-next-build` SDK.
2. Implement `launch` as a real process spawn with the resolved environment.
3. Add integration tests using constructed rez packages.

## Development

```bash
cargo build
cargo test
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs the same checks on Linux, macOS, and Windows.

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
