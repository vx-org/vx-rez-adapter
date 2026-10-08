# vx-rez-adapter

Rust bridge from [vx](https://github.com/vx-org) to [Rez Next](https://github.com/loonghao/rez-next) package environments.

The adapter delegates repository lookup, dependency resolution, variant selection, package materialization, and Rex activation to `rez-next-runtime`. It converts the SDK result into a deterministic environment and delta, then launches the requested program directly.

## Installation

```toml
[dependencies]
vx-rez-adapter = "0.1.0"
```

Requires Rust 1.95.0 or newer. The runtime SDK dependency is pinned to `rez-next-runtime = "=0.3.9"` from crates.io; no Git or local path dependency is needed by consumers.

## Resolve an environment

```rust
use vx_rez_adapter::{ResolveRequest, RezAdapter};

let request = ResolveRequest::new(["application-1.0.0"])
    .package_paths(["/srv/packages"]);
let resolved = RezAdapter::new().resolve_env(&request)?;

// Includes roots of transitive dependencies and selected variants.
println!("resolved {} package roots", resolved.package_roots.len());
# Ok::<(), vx_rez_adapter::Error>(())
```

Package repositories use Rez's `<repository>/<name>/<version>/package.py` layout. YAML package definitions are also supported by the SDK. If `package_paths` is omitted, the adapter reads `REZ_PACKAGES_PATH` using the host path separator.

Explicit and implicit requests participate in the same dependency solve. Dependencies contribute their commands to the environment. Incompatible versions, missing requirements, and invalid selectors return a diagnostic instead of a partial environment.

```rust
use vx_rez_adapter::{Environment, ResolveRequest};

let request = ResolveRequest::new(["application"])
    .package_paths(["/srv/packages"])
    .implicit_requests(["shared-tools-2.0.0"])
    .target("windows", "AMD64")
    .parent_environment(Environment::new());
```

`target` adds platform and architecture constraints to variant selection. The repository must provide the corresponding `platform` and `arch` packages. Without an explicit target, the adapter adds no target constraints. The SDK accepts `windows`, `linux`, and `osx`, with `macos` and `darwin` normalized to `osx`.

`parent_environment` supplies the exact base used for activation. An explicit empty map excludes ambient variables; omitting it starts from the current process environment. On Windows, variable names are normalized to uppercase before activation so `Path` and `PATH` cannot create competing definitions.

### Async callers

```rust
use vx_rez_adapter::{ResolveRequest, RezAdapter};

# async fn example() -> Result<(), vx_rez_adapter::Error> {
let request = ResolveRequest::new(["application"])
    .package_paths(["/srv/packages"]);
let resolved = RezAdapter::new().resolve_env_async(&request).await?;
# Ok(())
# }
```

Use `resolve_env_async` within Tokio. The synchronous `resolve_env` creates a runtime for synchronous callers and returns a diagnostic when an existing runtime is detected, avoiding a nested-runtime panic.

## Launch a command

```rust
use vx_rez_adapter::{LaunchRequest, ResolveRequest, RezAdapter};

let adapter = RezAdapter::new();
let resolved = adapter.resolve_env(
    &ResolveRequest::new(["python-3.11"]).package_paths(["/srv/packages"]),
)?;
let outcome = adapter.launch(
    &LaunchRequest::new("python")
        .args(["-c", "print('a value with spaces')"])
        .environment(resolved.environment),
)?;
assert!(outcome.success());
# Ok::<(), vx_rez_adapter::Error>(())
```

Arguments are forwarded directly to the executable without a shell. The child inherits stdin, stdout, and stderr. Calling `.environment(map)` injects exactly that map, including an empty map. A launch with no environment supplied inherits the current process environment. On Windows, the adapter prevents an additional console window.

With an explicit environment, a bare program is located only through its supplied PATH and Windows PATHEXT, then launched by absolute path. Missing programs fail instead of falling through to an ambient runtime. Explicit executable paths also work with an empty environment.

A child that starts and exits nonzero returns `Ok(outcome)` with `success() == false`. Only failure to start returns `Error::Spawn`. Unix signal termination is exposed through `outcome.signal`.

The runnable consumer example accepts package requests before `--` and forwards the program and all arguments after it:

```sh
vx cargo run --example resolve_and_launch -- --packages-path /srv/packages application-1.0.0 -- application --verbose "a value"
```

Repeat `--packages-path` to add repositories, or omit it to use `REZ_PACKAGES_PATH`. Resolution and launch failures print diagnostics and exit nonzero; the program's exit status is propagated.

## Environment delta

`ResolvedEnv` includes the full `Environment` (`BTreeMap<String, String>`), the requested selectors, roots of every resolved package, and an `EnvDelta` against the activation parent. The delta replays to the exact generated environment against that same parent.

The standalone delta model supports ordered `Set`, `Unset`, `Prepend`, `Append`, and `SetIfEmpty` actions, including multiple actions per variable. Generated deltas preserve simple prepends and appends for path variables when whole segments match. A shared string suffix, insertion into the middle, or replacement becomes a `Set`.

```rust
use vx_rez_adapter::{EnvAction, EnvDelta, Environment, path_separator};

let mut delta = EnvDelta::new();
delta.push_action("PATH", EnvAction::Prepend("/pkg/bin".into(), path_separator().into()));
let parent = Environment::from([("PATH".into(), "/usr/bin".into())]);
let environment = delta.apply(&parent);
```

## Diagnostics

`Error::Resolve` carries the requested selectors and the SDK diagnostic. `Error::PackagePath` identifies an invalid repository path. `Error::Spawn` identifies the executable that could not start; repository and spawn errors expose the underlying I/O error through `Error::source()`.

The SDK owns package parsing and Rex semantics. The adapter does not substitute a simplified resolver or interpreter when resolution fails.

## Development

```sh
vx cargo fmt --all -- --check
vx cargo clippy --workspace --all-targets --all-features -- -D warnings
vx cargo test --workspace --all-targets
vx cargo test --workspace --all-targets --all-features
vx cargo test --workspace --doc --all-features
vx cargo package --locked
```

CI tests Linux, macOS, and Windows and verifies the packaged crate using registry dependencies. Local SDK patches may be used during development, but they do not establish registry publication or consumer acceptance.

## License

[Apache License, Version 2.0](./LICENSE).
