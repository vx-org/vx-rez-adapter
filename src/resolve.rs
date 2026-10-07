//! Turning rez package requests into a concrete environment.
//!
//! The resolve runs in two steps:
//!
//! 1. **Package lookup.** Each request is parsed with the `rez-next` requirement
//!    parser and matched against the packages found on [`ResolveRequest::package_paths`].
//! 2. **Environment generation.** The matched packages are handed to
//!    `rez-next-context`'s [`EnvironmentManager`], which interprets their Rex
//!    commands against a base environment and returns the result.
//!
//! The second step is what makes a real adapter: `rez-next-context` owns Rex
//! semantics, and re-implementing them here would silently drift from rez.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rez_next_context::{ContextConfig, EnvironmentManager, PathStrategy, ShellType};
use rez_next_package::Package;
use rez_next_package::requirement::RequirementParser;

use crate::ResolveRequest;
use crate::env::{EnvAction, EnvDelta, ResolvedEnv, env_key};
use crate::error::{Error, Result};

/// A package request that has been parsed and matched to a package on disk.
#[derive(Debug, Clone)]
struct MatchedPackage {
    /// The request string this package was matched for, e.g. `"python-3.11"`.
    request: String,
    /// The package definition, ready for environment generation.
    package: Package,
    /// Root directory the package was loaded from.
    root: PathBuf,
}

/// Builds the component separator used when a package appends to a path-like
/// variable. Rez itself uses the platform separator, so the adapter does too.
fn separator() -> String {
    if cfg!(windows) {
        ";".to_string()
    } else {
        ":".to_string()
    }
}

impl ResolveRequest {
    /// Returns the paths searched for packages.
    ///
    /// Falls back to the ambient rez package paths when the request does not
    /// override them.
    fn resolved_package_paths(&self) -> Result<Vec<PathBuf>> {
        match &self.package_paths {
            Some(paths) => {
                if paths.is_empty() {
                    return Err(Error::Resolve {
                        request: self.requests.join(", "),
                        reason: "package_paths was set but empty".to_string(),
                    });
                }
                Ok(paths.clone())
            }
            None => Ok(ambient_package_paths()),
        }
    }
}

/// Reads the ambient rez package search path.
///
/// `REZ_PACKAGES_PATH` is the conventional override; without it there is no
/// sensible default to scan, and the resolve reports that rather than guessing.
fn ambient_package_paths() -> Vec<PathBuf> {
    std::env::var_os("REZ_PACKAGES_PATH")
        .map(|value| {
            std::env::split_paths(&value)
                .filter(|p| !p.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Resolves the environment described by `request`.
///
/// # Errors
///
/// Returns [`Error::Resolve`] when a request cannot be parsed, when no package
/// satisfies it, or when no package paths are configured. Returns
/// [`Error::PackagePath`] when a configured path cannot be read.
pub fn resolve_env(request: &ResolveRequest) -> Result<ResolvedEnv> {
    if request.requests.is_empty() {
        return Err(Error::Resolve {
            request: String::new(),
            reason: "no package requests were given".to_string(),
        });
    }

    let paths = request.resolved_package_paths()?;
    if paths.is_empty() {
        return Err(Error::Resolve {
            request: request.requests.join(", "),
            reason: "no package paths configured; set package_paths or REZ_PACKAGES_PATH"
                .to_string(),
        });
    }
    for path in &paths {
        validate_package_path(path)?;
    }

    let parser = RequirementParser::new();
    let mut matched = Vec::with_capacity(request.requests.len());
    for raw in &request.requests {
        let requirement = parser.parse(raw).map_err(|reason| Error::Resolve {
            request: raw.clone(),
            reason,
        })?;
        let found = find_package(&paths, &requirement.name, raw)?;
        matched.push(found);
    }

    let parent = parent_environment();
    let environment = generate_environment(&matched, &parent)?;
    let delta = delta_against(&parent, &environment);

    Ok(ResolvedEnv {
        environment: environment.into_iter().collect(),
        delta,
        resolved_requests: matched.iter().map(|m| m.request.clone()).collect(),
        package_roots: matched.into_iter().map(|m| m.root).collect(),
    })
}

/// Checks that `path` can be used as a package search path.
fn validate_package_path(path: &Path) -> Result<()> {
    std::fs::metadata(path).map_err(|source| Error::PackagePath {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Collects the environment a resolve starts from.
fn parent_environment() -> HashMap<String, String> {
    std::env::vars()
        .map(|(name, value)| (env_key(&name), value))
        .collect()
}

/// Finds the package satisfying `name` under one of `paths`.
///
/// Rez lays packages out as `<path>/<name>/<version>/package.py`. The adapter
/// scans for the highest version directory that carries a package definition,
/// which is what a plain `rez-env <name>` request means.
fn find_package(paths: &[PathBuf], name: &str, raw: &str) -> Result<MatchedPackage> {
    let mut best: Option<(rez_next_version::Version, MatchedPackage)> = None;

    for root in paths {
        let family = root.join(name);
        let Ok(entries) = std::fs::read_dir(&family) else {
            continue;
        };

        for entry in entries.flatten() {
            let version_dir = entry.path();
            if !version_dir.is_dir() {
                continue;
            }
            let Some(version) = version_of(&version_dir) else {
                continue;
            };
            let Some(package) = load_package(&version_dir, name, &version) else {
                continue;
            };

            let is_better = best.as_ref().is_none_or(|(seen, _)| version > *seen);
            if is_better {
                best = Some((
                    version,
                    MatchedPackage {
                        request: raw.to_string(),
                        package,
                        root: version_dir,
                    },
                ));
            }
        }
    }

    best.map(|(_, matched)| matched)
        .ok_or_else(|| Error::Resolve {
            request: raw.to_string(),
            reason: format!("no package `{name}` found under {}", render_paths(paths)),
        })
}

/// Parses the version from a package directory name.
fn version_of(dir: &Path) -> Option<rez_next_version::Version> {
    let name = dir.file_name()?.to_str()?;
    rez_next_version::Version::parse(name).ok()
}

/// Loads a package definition from `dir`, if it has one.
///
/// A rez package is defined by `package.py`, with `package.yaml` as the
/// alternative. Neither being present means the directory is not a package.
fn load_package(dir: &Path, name: &str, version: &rez_next_version::Version) -> Option<Package> {
    let mut package = Package {
        name: name.to_string(),
        version: Some(version.clone()),
        ..Package::default()
    };

    let py = dir.join("package.py");
    let yaml = dir.join("package.yaml");

    if py.is_file() {
        let body = std::fs::read_to_string(&py).ok()?;
        package.commands = extract_commands(&body);
        package.filepath = Some(py.to_string_lossy().into_owned());
    } else if yaml.is_file() {
        let body = std::fs::read_to_string(&yaml).ok()?;
        let parsed: Package = serde_yaml::from_str(&body).ok()?;
        package = parsed;
        package.name = name.to_string();
        package.version = Some(version.clone());
        package.filepath = Some(yaml.to_string_lossy().into_owned());
    } else {
        return None;
    }

    Some(package)
}

/// Pulls the body of `def commands()` out of a `package.py` source.
///
/// The adapter keeps this deliberately small: it covers the common single-block
/// form so a constructed package can be resolved without pulling in a Python
/// runtime. Packages outside that shape still resolve, just without their Rex
/// commands, which yields an environment with the package root recorded but no
/// variable changes.
fn extract_commands(source: &str) -> Option<String> {
    let header = source.find("def commands(")?;
    let rest = &source[header..];
    let body_start = rest.find('\n')? + 1;
    let body = &rest[body_start..];

    let mut indent: Option<usize> = None;
    let mut out = String::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        let width = line.len() - line.trim_start().len();
        match indent {
            None => indent = Some(width),
            Some(expected) => {
                if width < expected {
                    break;
                }
            }
        }
        out.push_str(line.trim_start());
        out.push('\n');
    }
    Some(out)
}

/// Renders `paths` for an error message.
fn render_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| format!("`{}`", p.display()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Runs the matched packages' Rex commands against `parent`.
fn generate_environment(
    matched: &[MatchedPackage],
    parent: &HashMap<String, String>,
) -> Result<HashMap<String, String>> {
    let config = ContextConfig {
        inherit_parent_env: false,
        shell_type: default_shell(),
        working_directory: None,
        additional_env_vars: HashMap::new(),
        unset_vars: Vec::new(),
        path_strategy: PathStrategy::Prepend,
    };

    let manager = EnvironmentManager::with_base_environment(config, parent.clone());
    let packages: Vec<Package> = matched.iter().map(|m| m.package.clone()).collect();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| Error::Resolve {
            request: matched
                .first()
                .map(|m| m.request.clone())
                .unwrap_or_default(),
            reason: format!("could not start the resolve runtime: {source}"),
        })?;

    runtime
        .block_on(manager.generate_environment(&packages))
        .map_err(|source| Error::Resolve {
            request: matched
                .first()
                .map(|m| m.request.clone())
                .unwrap_or_default(),
            reason: source.to_string(),
        })
}

/// Picks the shell whose syntax the manager should assume.
///
/// Only Windows differs: a resolve there targets `cmd.exe`, whose quoting and
/// path rules are the ones a launched Windows tool will see.
fn default_shell() -> ShellType {
    if cfg!(windows) {
        ShellType::Cmd
    } else {
        ShellType::Bash
    }
}

/// Derives the delta that turns `parent` into `environment`.
///
/// [`EnvironmentManager::get_env_diff`] already classifies each variable as
/// added, modified, or removed, which maps onto the adapter's delta without
/// re-deriving the comparison.
fn delta_against(
    parent: &HashMap<String, String>,
    environment: &HashMap<String, String>,
) -> EnvDelta {
    let mut delta = EnvDelta::new();
    let mut keys: Vec<&String> = parent.keys().chain(environment.keys()).collect();
    keys.sort_unstable();
    keys.dedup();

    for key in keys {
        match (parent.get(key), environment.get(key)) {
            (None, Some(value)) => {
                delta.push_action(key.clone(), EnvAction::Set(value.clone()));
            }
            (Some(_), None) => {
                delta.push_action(key.clone(), EnvAction::Unset);
            }
            (Some(before), Some(after)) if before != after => {
                push_value_change(&mut delta, key, before, after);
            }
            _ => {}
        }
    }
    delta
}

/// Records how a variable moved from `before` to `after`.
///
/// A change that only grew one end is recorded as the matching prepend or
/// append, so re-applying the delta to a different parent keeps the same
/// intent. Anything else is a plain set.
fn push_value_change(delta: &mut EnvDelta, key: &str, before: &str, after: &str) {
    let sep = separator();

    if let Some(added) = after.strip_suffix(before) {
        let added = added.strip_suffix(&sep).unwrap_or(added);
        if !added.is_empty() {
            delta.push_action(
                env_key(key),
                EnvAction::Prepend(added.to_string(), sep.clone()),
            );
            return;
        }
    }

    if let Some(added) = after.strip_prefix(before) {
        let added = added.strip_prefix(&sep).unwrap_or(added);
        if !added.is_empty() {
            delta.push_action(env_key(key), EnvAction::Append(added.to_string(), sep));
            return;
        }
    }

    delta.push_action(env_key(key), EnvAction::Set(after.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_package(root: &Path, name: &str, version: &str, commands: &str) -> PathBuf {
        let dir = root.join(name).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.py"),
            format!(
                "name = \"{name}\"\nversion = \"{version}\"\n\ndef commands():\n    {commands}\n"
            ),
        )
        .unwrap();
        dir
    }

    #[test]
    fn extract_commands_reads_an_indented_block() {
        let source = "name = \"x\"\n\ndef commands():\n    env.PATH.append('/bin')\n    setenv('FOO', '1')\n";
        let commands = extract_commands(source).unwrap();
        assert!(commands.contains("env.PATH.append"));
        assert!(commands.contains("setenv('FOO'"));
    }

    #[test]
    fn extract_commands_stops_at_dedent() {
        let source = "def commands():\n    setenv('A', '1')\n\nname = \"x\"\n";
        let commands = extract_commands(source).unwrap();
        assert!(commands.contains("setenv('A'"));
        assert!(!commands.contains("name ="));
    }

    #[test]
    fn extract_commands_returns_none_without_a_block() {
        assert!(extract_commands("name = \"x\"\nversion = \"1\"\n").is_none());
    }

    #[test]
    fn separator_matches_the_host() {
        if cfg!(windows) {
            assert_eq!(separator(), ";");
        } else {
            assert_eq!(separator(), ":");
        }
    }

    #[test]
    fn empty_requests_are_rejected() {
        let err = resolve_env(&ResolveRequest::new(Vec::<String>::new())).unwrap_err();
        assert!(matches!(err, Error::Resolve { .. }));
    }

    #[test]
    fn missing_package_paths_are_rejected() {
        let mut request = ResolveRequest::new(["python"]);
        request.package_paths = Some(Vec::new());
        let err = resolve_env(&request).unwrap_err();
        match err {
            Error::Resolve { reason, .. } => assert!(reason.contains("empty")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn unusable_package_path_is_reported() {
        let mut request = ResolveRequest::new(["python"]);
        request.package_paths = Some(vec![PathBuf::from("/definitely/not/here/at/all")]);
        let err = resolve_env(&request).unwrap_err();
        assert!(matches!(err, Error::PackagePath { .. }));
    }

    #[test]
    fn configured_paths_are_used_without_ambient_config() {
        let request = ResolveRequest {
            package_paths: Some(vec![PathBuf::from("some/path")]),
            ..ResolveRequest::new(["python"])
        };
        assert_eq!(
            request.resolved_package_paths().unwrap(),
            vec![PathBuf::from("some/path")]
        );
    }

    #[test]
    fn prepend_change_is_recorded_as_a_prepend() {
        let mut delta = EnvDelta::new();
        let sep = separator();
        push_value_change(
            &mut delta,
            "PATH",
            "/usr/bin",
            &format!("/opt/pkg/bin{}/usr/bin", sep),
        );
        assert_eq!(
            delta.actions("PATH"),
            &[EnvAction::Prepend("/opt/pkg/bin".to_string(), sep)]
        );
    }

    #[test]
    fn append_change_is_recorded_as_an_append() {
        let mut delta = EnvDelta::new();
        let sep = separator();
        push_value_change(
            &mut delta,
            "PATH",
            "/usr/bin",
            &format!("/usr/bin{}/opt/pkg/bin", sep),
        );
        assert_eq!(
            delta.actions("PATH"),
            &[EnvAction::Append("/opt/pkg/bin".to_string(), sep)]
        );
    }

    #[test]
    fn unrelated_change_is_recorded_as_a_set() {
        let mut delta = EnvDelta::new();
        push_value_change(&mut delta, "FOO", "before", "after");
        assert_eq!(delta.actions("FOO"), &[EnvAction::Set("after".to_string())]);
    }

    #[test]
    fn delta_covers_added_modified_and_removed() {
        let parent = HashMap::from([
            ("KEPT".to_string(), "same".to_string()),
            ("CHANGED".to_string(), "old".to_string()),
            ("GONE".to_string(), "value".to_string()),
        ]);
        let environment = HashMap::from([
            ("KEPT".to_string(), "same".to_string()),
            ("CHANGED".to_string(), "new".to_string()),
            ("FRESH".to_string(), "value".to_string()),
        ]);
        let delta = delta_against(&parent, &environment);

        assert!(delta.actions("KEPT").is_empty());
        assert_eq!(
            delta.actions("FRESH"),
            &[EnvAction::Set("value".to_string())]
        );
        assert_eq!(delta.actions("GONE"), &[EnvAction::Unset]);
        assert_eq!(delta.actions("CHANGED").len(), 1);
    }

    /// End-to-end: a constructed package on disk resolves to an environment
    /// carrying the variable its Rex commands set.
    #[test]
    fn resolves_a_constructed_package() {
        let temp = tempfile::tempdir().unwrap();
        let root = write_package(
            temp.path(),
            "dummy",
            "1.0.0",
            "setenv('DUMMY_ROOT', 'expected')",
        );

        let mut request = ResolveRequest::new(["dummy"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let resolved = resolve_env(&request).unwrap();
        assert_eq!(resolved.package_roots, vec![root]);
        assert_eq!(resolved.resolved_requests, vec!["dummy".to_string()]);
        assert_eq!(
            resolved.environment.get("DUMMY_ROOT").map(String::as_str),
            Some("expected")
        );
    }

    /// Prepends are the common case: nearly every rez package adds itself to
    /// PATH, and that has to survive into the rendered environment.
    #[test]
    fn resolves_a_package_that_prepends_to_path() {
        let temp = tempfile::tempdir().unwrap();
        write_package(
            temp.path(),
            "dummy",
            "1.0.0",
            "env.prepend_path('PATH', '/opt/dummy/bin')",
        );

        let mut request = ResolveRequest::new(["dummy"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let resolved = resolve_env(&request).unwrap();
        let path = resolved.environment.get("PATH").expect("PATH is set");
        assert!(
            path.contains("/opt/dummy/bin"),
            "PATH did not pick up the package: {path}"
        );
    }

    /// The highest version wins, which is what a bare `rez-env <name>` means.
    #[test]
    fn the_highest_version_is_selected() {
        let temp = tempfile::tempdir().unwrap();
        write_package(temp.path(), "dummy", "1.0.0", "setenv('DUMMY', 'one')");
        let newer = write_package(temp.path(), "dummy", "2.0.0", "setenv('DUMMY', 'two')");

        let mut request = ResolveRequest::new(["dummy"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let resolved = resolve_env(&request).unwrap();
        assert_eq!(resolved.package_roots, vec![newer]);
        assert_eq!(
            resolved.environment.get("DUMMY").map(String::as_str),
            Some("two")
        );
    }

    /// A package whose Rex commands the interpreter rejects must surface as a
    /// resolve error, not a panic.
    #[test]
    fn invalid_rex_commands_are_reported() {
        let temp = tempfile::tempdir().unwrap();
        write_package(temp.path(), "broken", "1.0.0", "this is not rex");

        let mut request = ResolveRequest::new(["broken"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let err = resolve_env(&request).unwrap_err();
        match err {
            Error::Resolve { reason, .. } => assert!(reason.contains("Rex")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// The failure path: a request with no matching package reports which
    /// request failed instead of panicking.
    #[test]
    fn missing_package_is_reported_not_panicked() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = ResolveRequest::new(["nope"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let err = resolve_env(&request).unwrap_err();
        match err {
            Error::Resolve { request, reason } => {
                assert_eq!(request, "nope");
                assert!(reason.contains("no package"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// A malformed request fails at parse time with the parser's message.
    #[test]
    fn malformed_request_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = ResolveRequest::new(["!!! not a request"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let err = resolve_env(&request).unwrap_err();
        match err {
            Error::Resolve { request, .. } => assert_eq!(request, "!!! not a request"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// Without any package path configuration the resolve says so, rather than
    /// silently returning an empty environment.
    #[test]
    fn no_package_paths_is_reported() {
        let request = ResolveRequest::new(["python"]);
        let err = resolve_env(&request).unwrap_err();
        match err {
            Error::Resolve { reason, .. } => assert!(reason.contains("no package paths")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn summarize_reports_all_three_classes() {
        let parent = HashMap::from([("B".to_string(), "gone".to_string())]);
        let environment = HashMap::from([("A".to_string(), "1".to_string())]);
        let delta = delta_against(&parent, &environment);
        assert_eq!(delta.actions("A").len(), 1);
        assert_eq!(delta.actions("B"), &[EnvAction::Unset]);
    }
}
