# vx-rez-adapter

Rust adapter integrating [vx](https://github.com/vx-org) with [rez-next](https://github.com/loonghao/rez-next) package environments.

`vx` needs to resolve a rez package environment and then run a tool inside it. This crate is that bridge: it turns rez package requests into a concrete environment, and launches a program with that environment applied.

## Status

**0.1.0 is a scaffolding release.** The module layout and the public API surface are in place and fully documented, but the two core verbs are not implemented yet — `RezAdapter::resolve_env` and `RezAdapter::launch` return `Error::NotImplemented`.

This is deliberate. The resolution backend depends on the `rez-next-build` public SDK, which is not yet published to crates.io; landing the crate shape first lets downstream consumers compile against a stable surface while that dependency is settled. The environment model itself (`EnvDelta`, `ResolvedEnv`) is implemented and tested, because it has no upstream dependency.

## Installation

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
use vx_rez_adapter::{EnvAction, EnvDelta, ResolvedEnv};
use std::collections::BTreeMap;

let mut delta = EnvDelta::new();
delta.set_action("PATH", EnvAction::Prepend("/opt/rez/bin".to_string()));
delta.set_action("REZ_USED", EnvAction::Set("1".to_string()));
delta.set_action("OLD_VAR", EnvAction::Unset);

let parent = BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
let resolved = ResolvedEnv::from_delta(delta, &parent);

// `Prepend` joins with the platform path separator (`:` on unix, `;` on Windows).
assert!(resolved.environment["PATH"].starts_with("/opt/rez/bin"));
```

A resolve in rez is a *difference* against a parent environment, not a bare set
of variables — paths get prepended, variables get overridden, some get unset.
`EnvDelta` models that difference with four actions (`Unset`, `Set`, `Prepend`,
`Append`), and `ResolvedEnv` keeps both the rendered environment and the delta,
so a caller can diff a resolve or re-apply it against a different parent.

## Public API

| Type | Purpose |
| --- | --- |
| `RezAdapter` | Top-level entry point: `resolve_env` and `launch` |
| `ResolveRequest` | Package requests plus optional implicit requests and package paths |
| `EnvDelta` | Ordered set of variable changes a resolve applies |
| `EnvAction` | A single change: `Unset`, `Set`, `Prepend`, `Append` |
| `ResolvedEnv` | Rendered environment, the delta behind it, and package roots |
| `LaunchRequest` | Program, arguments, environment, and working directory |
| `LaunchOutcome` | Exit code and signal state of a finished child |
| `Environment` | Alias for `BTreeMap<String, String>`; sorted for deterministic output |
| `Error` / `Result` | Adapter error and result types |

## Roadmap (stage 2)

1. Implement `resolve_env` against the rez-next SDK.
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
