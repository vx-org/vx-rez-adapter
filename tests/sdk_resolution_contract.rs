//! The adapter must delegate dependency semantics to the Rez Next SDK.

use std::fs;
use std::path::Path;

use tempfile::TempDir;
use vx_rez_adapter::{ResolveRequest, RezAdapter};

fn package(repository: &Path, name: &str, version: &str, body: &str) {
    let root = repository.join(name).join(version);
    fs::create_dir_all(&root).expect("create package root");
    fs::write(
        root.join("package.py"),
        format!("name = '{name}'\nversion = '{version}'\n{body}"),
    )
    .expect("write package definition");
}

#[test]
fn test_resolve_env_includes_transitive_package_environment() {
    let repository = TempDir::new().expect("temporary repository");
    package(
        repository.path(),
        "dependency",
        "1.0.0",
        "def commands():\n    env.VX_DEPENDENCY_PROOF = 'resolved'\n",
    );
    package(
        repository.path(),
        "application",
        "1.0.0",
        "requires = ['dependency-1.0.0']\n",
    );

    let resolved = RezAdapter::new()
        .resolve_env(&ResolveRequest::new(["application-1.0.0"]).package_paths([repository.path()]))
        .expect("resolve application and its dependency");

    assert_eq!(
        resolved.environment.get("VX_DEPENDENCY_PROOF"),
        Some(&"resolved".to_owned()),
        "dependency commands must contribute to the resolved environment"
    );
    assert!(
        resolved
            .package_roots
            .iter()
            .any(|root| root.ends_with("dependency/1.0.0")),
        "the resolved graph must include the dependency root"
    );
}

#[test]
fn test_resolve_env_rejects_conflicting_transitive_requirements() {
    let repository = TempDir::new().expect("temporary repository");
    for version in ["1.0.0", "2.0.0"] {
        package(repository.path(), "dependency", version, "");
    }
    package(
        repository.path(),
        "application",
        "1.0.0",
        "requires = ['dependency-1.0.0']\n",
    );
    package(
        repository.path(),
        "extension",
        "1.0.0",
        "requires = ['dependency-2.0.0']\n",
    );

    let result = RezAdapter::new().resolve_env(
        &ResolveRequest::new(["application-1.0.0", "extension-1.0.0"])
            .package_paths([repository.path()]),
    );

    assert!(
        result.is_err(),
        "incompatible dependency versions must fail resolution"
    );
}
