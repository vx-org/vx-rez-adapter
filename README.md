# vx-rez-adapter

Rust adapter integrating [vx](https://github.com/vx-org) with [rez-next](https://github.com/loonghao/rez-next) package environments.

`vx` needs to resolve a rez package environment and then run a tool inside it. This crate is that bridge: it turns rez package requests into a concrete environment, and launches a program with that environment applied.

## Status

Both core verbs are implemented on top of the `rez-next` SDK:

- **`resolve_env`** looks packages up on disk and runs their Rex commands through `rez-next-context`, returning a `ResolvedEnv`.
- **`launch`** spawns a program with that environment applied and reports its exit status.

The environment model (`EnvDelta`, `ResolvedEnv`) is implemented and tested independently of the resolve, so it can be used on its own.

## Installation

```toml
[dependencies]
vx-rez-adapter = "0.1.0"
```

Requires Rust 1.95.0 or newer (edition 2024).

To depend on unreleased changes, use the git form. Prefer a pinned revision
over a branch so the build stays reproducible:

```toml
[dependencies]
vx-rez-adapter = { git = "https://github.com/vx-org/vx-rez-adapter", rev = "<sha>" }
```

The git form exists for local development against unreleased work; releases
are consumed from crates.io.

## Usage

### Resolving an environment

Packages are found under `REZ_PACKAGES_PATH`, or under an explicit override:

```rust
use vx_rez_adapter::{RezAdapter, ResolveRequest};

let adapter = RezAdapter::new();

// Uses REZ_PACKAGES_PATH, or an explicit override:
let request = ResolveRequest::new(["python-3.11"])
    .package_paths(["/srv/packages"]);

let resolved = adapter.resolve_env(&request)?;
println!("PATH={}", resolved.environment["PATH"]);
println!("resolved {} package(s)", resolved.package_roots.len());
```

Packages are laid out the way rez expects them:

```
<package_path>/<name>/<version>/package.py
```

A bare request like `"python"` selects the highest version found. A request that
carries a version constraint — `"python-3.11"`, `"python-3.11+"`,
`"python<4"` — selects the highest version **matching that constraint**. A
constraint that matches nothing is an error; the resolve never quietly hands
back a version you did not ask for.

### Launching a tool inside a resolved environment

```rust
use vx_rez_adapter::{LaunchRequest, RezAdapter, ResolveRequest};

let adapter = RezAdapter::new();
let resolved = adapter.resolve_env(&ResolveRequest::new(["python-3.11"]))?;

let outcome = adapter.launch(
    &LaunchRequest::new("python")
        .arg("--version")
        .environment(resolved.environment),
)?;

assert!(outcome.success());
println!("exit code: {:?}", outcome.code);
```

The child inherits this process's stdin, stdout, and stderr, so an interactive tool behaves the way a user expects. On Windows the child is created with `CREATE_NO_WINDOW`, so a GUI-launched tool does not open an extra console window.

**A program that runs and exits non-zero is a successful launch** — you get `Ok(outcome)` with `outcome.success() == false`. Only a failure to *start* the program is an `Err`.

This matches `std::process::Command::status()`: `Ok` means "the process was started", `success()` means "it did its job". **Always check `success()`** — inspecting only the `Result` makes a tool that ran and failed look identical to one that ran and succeeded. The two are not the same, and they are usually handled differently.

```rust
use vx_rez_adapter::LaunchRequest;

// Runs, exits 1 -> Ok(outcome) with success() == false
let outcome = RezAdapter::new().launch(
    &LaunchRequest::new("python").arg("-c").arg("raise SystemExit(1)"),
)?;
assert!(!outcome.success());

// Cannot be started at all -> Err(Error::Spawn { .. })
let err = RezAdapter::new().launch(&LaunchRequest::new("no-such-tool"));
assert!(err.is_err());
```

On unix, a child killed by a signal reports `code: None` and `signal: Some(n)`, so a caller can tell *how* the child died rather than only that it failed.

### Working with the environment model

The environment model is available without a resolve:

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

When a resolve produces a delta, a changed variable is recorded as a `Prepend`
or `Append` only when the change really is one — and only for path-like
variables (`PATH`, `LD_LIBRARY_PATH`, `PYTHONPATH`, and similar). The comparison
splits on the separator and compares whole segments, so `/bin` becoming
`/usr/bin` is a `Set`, not a prepend of `/usr`. Anything else is a `Set`, which
always replays correctly.

## Error handling

Every failure is a value, never a panic. The resolve path reports which request
failed and why:

```rust
use vx_rez_adapter::{Error, RezAdapter, ResolveRequest};

let err = RezAdapter::new()
    .resolve_env(&ResolveRequest::new(["no-such-package"]))
    .unwrap_err();

match err {
    Error::Resolve { request, reason } => {
        eprintln!("could not resolve `{request}`: {reason}");
    }
    Error::PackagePath { path, .. } => {
        eprintln!("unusable package path: {}", path.display());
    }
    Error::Spawn { program, .. } => {
        eprintln!("could not start `{program}`");
    }
    other => eprintln!("{other}"),
}
```

One case is worth knowing about: a package whose `def commands()` body the
upstream loader cannot parse contributes **nothing** to the environment rather
than failing the resolve. That is `rez-next`'s behaviour, surfaced here so it is
not mistaken for a silent success of your own code.

`Error::source()` carries the underlying I/O error for `PackagePath` and
`Spawn`, so a caller can inspect the OS-level cause.

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
| `which` | Locates a program on `PATH` without spawning it |
| `Error` / `Result` | Adapter error and result types |

The data-oriented public types are `#[non_exhaustive]`, so new fields and
variants can be added without breaking downstream code.

## Examples

```bash
REZ_PACKAGES_PATH=/srv/packages cargo run --example resolve_and_launch
```

The example resolves a package, launches a tool inside it, and demonstrates the
failure path. It degrades to a readable message when no repository is
configured, so it stays runnable in a bare checkout.

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
