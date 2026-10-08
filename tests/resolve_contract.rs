use std::fs;
use std::path::{Path, PathBuf};

use rstest::rstest;
use tempfile::TempDir;
use vx_rez_adapter::{EnvAction, Environment, Error, ResolveRequest, RezAdapter, path_separator};

fn package(repository: &Path, name: &str, version: &str, body: &str) -> PathBuf {
    let root = repository.join(name).join(version);
    fs::create_dir_all(&root).expect("create package root");
    fs::write(
        root.join("package.py"),
        format!("name = '{name}'\nversion = '{version}'\n{body}"),
    )
    .expect("write package definition");
    root
}

fn request(repository: &Path, selector: &str) -> ResolveRequest {
    ResolveRequest::new([selector])
        .package_paths([repository])
        .parent_environment(Environment::new())
}

#[rstest]
#[case("application", "2.0.0")]
#[case("application-1.0.0", "1.0.0")]
#[case("application>=1,<2", "1.0.0")]
fn test_resolve_env_selects_highest_matching_version(
    #[case] selector: &str,
    #[case] expected: &str,
) {
    let repository = TempDir::new().unwrap();
    for version in ["1.0.0", "2.0.0"] {
        package(
            repository.path(),
            "application",
            version,
            &format!("def commands():\n    env.PROBE = '{version}'\n"),
        );
    }
    let resolved = RezAdapter::new()
        .resolve_env(&request(repository.path(), selector))
        .unwrap();
    assert_eq!(resolved.environment["PROBE"], expected);
    assert!(
        resolved
            .package_roots
            .iter()
            .any(|root| root.ends_with(format!("application/{expected}")))
    );
    assert_eq!(resolved.resolved_requests, [selector]);
}

#[test]
fn test_resolve_env_includes_implicit_requirements_in_the_solve() {
    let repository = TempDir::new().unwrap();
    package(repository.path(), "application", "1.0.0", "");
    package(
        repository.path(),
        "implicit",
        "1.0.0",
        "def commands():\n    env.IMPLICIT_PROBE = 'included'\n",
    );
    let resolved = RezAdapter::new()
        .resolve_env(&request(repository.path(), "application").implicit_requests(["implicit"]))
        .unwrap();
    assert_eq!(resolved.environment["IMPLICIT_PROBE"], "included");
    assert_eq!(resolved.package_roots.len(), 2);
    assert_eq!(resolved.resolved_requests, ["application", "implicit"]);
}

#[test]
fn test_resolve_env_reports_unsatisfied_requirements() {
    let repository = TempDir::new().unwrap();
    package(repository.path(), "application", "1.0.0", "");
    for selector in ["application-9.9.9", "missing", "!!! not a request"] {
        let error = RezAdapter::new()
            .resolve_env(&request(repository.path(), selector))
            .unwrap_err();
        match error {
            Error::Resolve { request, reason } => {
                assert_eq!(request, selector);
                assert!(!reason.is_empty());
            }
            other => panic!("unexpected diagnostic: {other}"),
        }
    }
}

#[test]
fn test_resolve_env_validates_request_and_repository() {
    let repository = TempDir::new().unwrap();
    let adapter = RezAdapter::new();
    assert!(matches!(
        adapter.resolve_env(&ResolveRequest::new(Vec::<String>::new())),
        Err(Error::Resolve { .. })
    ));
    assert!(matches!(
        adapter.resolve_env(
            &ResolveRequest::new(["application"]).package_paths(Vec::<PathBuf>::new())
        ),
        Err(Error::Resolve { .. })
    ));
    assert!(matches!(
        adapter.resolve_env(&request(&repository.path().join("missing"), "application")),
        Err(Error::PackagePath { .. })
    ));
    let file = repository.path().join("file");
    fs::write(&file, "not a repository").unwrap();
    assert!(matches!(
        adapter.resolve_env(&request(&file, "application")),
        Err(Error::PackagePath { .. })
    ));
}

#[test]
fn test_resolve_env_explicit_empty_parent_does_not_inherit_ambient_variables() {
    let repository = TempDir::new().unwrap();
    package(
        repository.path(),
        "application",
        "1.0.0",
        "def commands():\n    env.PROBE = 'package'\n",
    );
    let resolved = RezAdapter::new()
        .resolve_env(&request(repository.path(), "application"))
        .unwrap();
    for (name, _) in std::env::vars() {
        let name = vx_rez_adapter::env_key(&name);
        if !name.starts_with("REZ_") && name != "PROBE" {
            assert!(
                !resolved.environment.contains_key(&name),
                "ambient variable {name} leaked into an explicit empty parent"
            );
        }
    }
    assert_eq!(resolved.environment["PROBE"], "package");
    assert_eq!(
        resolved.delta.apply(&Environment::new()),
        resolved.environment
    );
}

#[test]
fn test_resolve_env_preserves_explicit_parent_and_exact_delta() {
    let repository = TempDir::new().unwrap();
    package(
        repository.path(),
        "application",
        "1.0.0",
        "def commands():\n    env.CHANGED = 'after'\n    env.ADDED = 'new'\n",
    );
    let parent = Environment::from([
        ("CHANGED".to_owned(), "before".to_owned()),
        ("KEPT".to_owned(), "same".to_owned()),
    ]);
    let resolved = RezAdapter::new()
        .resolve_env(&request(repository.path(), "application").parent_environment(parent.clone()))
        .unwrap();
    assert_eq!(resolved.environment["KEPT"], "same");
    assert_eq!(
        resolved.delta.actions("CHANGED"),
        [EnvAction::Set("after".to_owned())]
    );
    assert_eq!(
        resolved.delta.actions("ADDED"),
        [EnvAction::Set("new".to_owned())]
    );
    assert!(resolved.delta.actions("KEPT").is_empty());
    assert_eq!(resolved.delta.apply(&parent), resolved.environment);
}

#[test]
fn test_resolve_env_delta_recognizes_only_whole_path_segments() {
    let repository = TempDir::new().unwrap();
    let separator = path_separator();
    let cases = [
        (
            "/base".to_owned(),
            format!("/front{separator}/base"),
            EnvAction::Prepend("/front".to_owned(), separator.to_owned()),
        ),
        (
            "/base".to_owned(),
            format!("/base{separator}/back"),
            EnvAction::Append("/back".to_owned(), separator.to_owned()),
        ),
        (
            "/bin".to_owned(),
            "/usr/bin".to_owned(),
            EnvAction::Set("/usr/bin".to_owned()),
        ),
        (
            format!("/a{separator}/c"),
            format!("/a{separator}/b{separator}/c"),
            EnvAction::Set(format!("/a{separator}/b{separator}/c")),
        ),
        (
            format!("/a{separator}/b"),
            "/a".to_owned(),
            EnvAction::Set("/a".to_owned()),
        ),
    ];
    for (index, (before, after, action)) in cases.into_iter().enumerate() {
        let name = format!("application{index}");
        package(
            repository.path(),
            &name,
            "1.0.0",
            &format!("def commands():\n    env.PATH = '{after}'\n"),
        );
        let parent = Environment::from([("PATH".to_owned(), before)]);
        let resolved = RezAdapter::new()
            .resolve_env(&request(repository.path(), &name).parent_environment(parent.clone()))
            .unwrap();
        assert_eq!(resolved.delta.actions("PATH"), [action]);
        assert_eq!(resolved.delta.apply(&parent), resolved.environment);
    }
}

#[test]
fn test_resolve_env_rejects_invalid_target() {
    let repository = TempDir::new().unwrap();
    package(repository.path(), "application", "1.0.0", "");
    let result = RezAdapter::new()
        .resolve_env(&request(repository.path(), "application").target("unsupported", "x86_64"));
    assert!(matches!(result, Err(Error::Resolve { .. })));
}

#[test]
fn test_resolve_env_target_selects_compatible_variant() {
    let repository = TempDir::new().unwrap();
    package(repository.path(), "platform", "windows", "");
    package(repository.path(), "platform", "linux", "");
    package(repository.path(), "arch", "AMD64", "");
    package(repository.path(), "arch", "x86_64", "");
    package(
        repository.path(),
        "application",
        "1.0.0",
        "variants = [['platform-linux', 'arch-x86_64'], ['platform-windows', 'arch-AMD64']]\ndef commands():\n    env.TARGET_PROOF = '{root}'\n",
    );
    let resolved = RezAdapter::new()
        .resolve_env(&request(repository.path(), "application").target("windows", "AMD64"))
        .unwrap();
    assert!(
        resolved
            .package_roots
            .iter()
            .any(|root| root.ends_with("platform/windows"))
    );
    assert!(
        resolved
            .package_roots
            .iter()
            .any(|root| root.ends_with("arch/AMD64"))
    );
    assert!(
        !resolved
            .package_roots
            .iter()
            .any(|root| root.ends_with("platform/linux"))
    );
    assert!(resolved.environment.contains_key("TARGET_PROOF"));
}

#[test]
fn test_resolve_env_loads_yaml_packages_through_sdk() {
    let repository = TempDir::new().unwrap();
    let root = repository.path().join("application/1.0.0");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("package.yaml"),
        "name: application\nversion: 1.0.0\ncommands: |\n  setenv('YAML_PROBE', 'resolved')\n",
    )
    .unwrap();
    let resolved = RezAdapter::new()
        .resolve_env(&request(repository.path(), "application"))
        .unwrap();
    assert_eq!(resolved.environment["YAML_PROBE"], "resolved");
}

#[tokio::test]
async fn test_resolve_env_async_runs_in_existing_runtime() {
    let repository = TempDir::new().unwrap();
    package(
        repository.path(),
        "application",
        "1.0.0",
        "def commands():\n    env.ASYNC_PROBE = 'resolved'\n",
    );
    let adapter = RezAdapter::new();
    let request = request(repository.path(), "application");
    let error = adapter.resolve_env(&request).unwrap_err();
    assert!(error.to_string().contains("resolve_env_async"));
    let resolved = adapter.resolve_env_async(&request).await.unwrap();
    assert_eq!(resolved.environment["ASYNC_PROBE"], "resolved");
}

#[cfg(windows)]
#[test]
fn test_resolve_env_collapses_windows_path_case_before_activation() {
    let repository = TempDir::new().unwrap();
    package(
        repository.path(),
        "application",
        "1.0.0",
        "def commands():\n    env.Path.prepend('/front')\n",
    );
    let resolved = RezAdapter::new()
        .resolve_env(
            &request(repository.path(), "application")
                .parent_environment([("Path", "/old"), ("PATH", "/base")]),
        )
        .unwrap();
    assert_eq!(resolved.environment["PATH"], "/front;/base");
    assert!(!resolved.environment.contains_key("Path"));
}
