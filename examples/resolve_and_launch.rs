//! End-to-end walkthrough: resolve a package's environment, then launch a
//! tool inside it.
//!
//! Run with a package repository laid out the way rez expects
//! (`<path>/<name>/<version>/package.py`):
//!
//! ```sh
//! REZ_PACKAGES_PATH=/srv/packages cargo run --example resolve_and_launch
//! ```
//!
//! The example degrades to a readable message when no repository is configured,
//! so it stays runnable in a checkout without one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use vx_rez_adapter::{LaunchRequest, ResolveRequest, RezAdapter};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let Some(packages_path) = std::env::var_os("REZ_PACKAGES_PATH") else {
        println!("REZ_PACKAGES_PATH is not set; nothing to resolve.");
        println!("Set it to a rez package repository and re-run to see a real resolve.");
        return Ok(());
    };

    let paths: Vec<PathBuf> = std::env::split_paths(&packages_path).collect();
    println!("searching for packages under {paths:?}");

    let adapter = RezAdapter::new();
    let request = ResolveRequest::new(["python"]).package_paths(paths);

    let resolved = match adapter.resolve_env(&request) {
        Ok(resolved) => resolved,
        Err(err) => {
            println!("resolve failed: {err}");
            println!("This is the expected outcome when no `python` package is present.");
            return Ok(());
        }
    };

    println!("resolved requests: {:?}", resolved.resolved_requests);
    println!("package roots: {:?}", resolved.package_roots);
    println!(
        "delta recorded {} action(s) across the resolve",
        resolved.delta.len()
    );

    // Launch a tool inside the resolved environment. `python --version` is the
    // smallest program that proves the environment reached a real child.
    let outcome = adapter.launch(
        &LaunchRequest::new("python")
            .arg("--version")
            .environment(resolved.environment.clone()),
    )?;
    println!("python --version exited with {:?}", outcome.code);

    // A tool that is not on the resolved PATH fails to *start*, which the
    // adapter reports as an error rather than a non-zero exit.
    match adapter.launch(&LaunchRequest::new("definitely-not-a-real-tool")) {
        Ok(_) => println!("unexpectedly launched a missing tool"),
        Err(err) => println!("missing tool reported as expected: {err}"),
    }

    // The delta can be re-applied to a different parent, which is how a caller
    // diffs a resolve against another shell.
    let other_parent = BTreeMap::new();
    println!(
        "re-applying the delta to an empty parent yields {} variable(s)",
        resolved.delta.apply(&other_parent).len()
    );

    Ok(())
}
