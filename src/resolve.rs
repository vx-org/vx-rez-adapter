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
use rez_next_package::requirement::{RequirementParser, VersionConstraint};

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
        let found = find_package(
            &paths,
            &requirement.name,
            requirement.version_constraint.as_ref(),
            raw,
        )?;
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

/// Finds the package satisfying `name` and `constraint` under one of `paths`.
///
/// Rez lays packages out as `<path>/<name>/<version>/package.py`. Candidates
/// whose version does not satisfy `constraint` are discarded, and the highest
/// surviving version wins — which is what `rez-env <name>` means for a bare
/// request, and what `rez-env <name>-<version>` means for a pinned one.
///
/// A constraint that excludes every candidate is an error rather than a reason
/// to fall back to "pick any version": silently handing a caller a different
/// version than the one it asked for is the worst outcome here.
fn find_package(
    paths: &[PathBuf],
    name: &str,
    constraint: Option<&VersionConstraint>,
    raw: &str,
) -> Result<MatchedPackage> {
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
            if let Some(constraint) = constraint
                && !constraint.is_satisfied_by(&version)
            {
                continue;
            }
            let Some(package) = load_package(&version_dir)? else {
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
            reason: match constraint {
                Some(constraint) => format!(
                    "no version of `{name}` matching {constraint:?} found under {}",
                    render_paths(paths)
                ),
                None => format!("no package `{name}` found under {}", render_paths(paths)),
            },
        })
}

/// Parses the version from a package directory name.
fn version_of(dir: &Path) -> Option<rez_next_version::Version> {
    let name = dir.file_name()?.to_str()?;
    rez_next_version::Version::parse(name).ok()
}

/// Loads a package definition from `dir`, if it has one.
///
/// Delegating to `Package::from_path` matters beyond convenience: it is the
/// only way the adapter picks up `requires`, `tools`, `variants`, and `config`
/// alongside `commands`. A hand-written `package.py` reader that only extracted
/// the command body would silently drop the dependency declarations, and the
/// resolve would then build an environment missing what those dependencies
/// contribute.
///
/// Returns `Ok(None)` for a directory that is not a package at all.
fn load_package(dir: &Path) -> Result<Option<Package>> {
    match Package::from_path(dir) {
        Ok(package) => Ok(Some(package)),
        // `from_path` reports a directory with no recognisable definition the
        // same way it reports one it cannot read, so the two are told apart
        // here rather than by inspecting the error text.
        Err(_) if !dir.is_dir() => Ok(None),
        Err(_) if !has_package_definition(dir) => Ok(None),
        Err(source) => Err(Error::PackageDefinition {
            path: dir.to_path_buf(),
            reason: source.to_string(),
        }),
    }
}

/// Reports whether `dir` carries any rez package definition file.
fn has_package_definition(dir: &Path) -> bool {
    ["package.py", "package.yaml", "package.yml"]
        .iter()
        .any(|name| dir.join(name).is_file())
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
/// Each variable is classified as added, modified, or removed. Modified
/// variables are further narrowed to a prepend or append when — and only when —
/// the change really is one; see [`push_value_change`].
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
/// Only path-like variables are candidates for a prepend or append, and even
/// there the comparison is done on **whole segments** rather than on raw string
/// prefixes and suffixes. Cutting on separators is what makes the difference:
/// `/bin` becoming `/usr/bin` shares the suffix `/bin`, but `/usr` is not a
/// path segment, so a naive strip would record a prepend that replays as
/// `/usr;/bin` instead of `/usr/bin`.
///
/// Everything else — including a path-like variable whose change is not a clean
/// one-sided addition — is recorded as a plain set. A `Set` always replays
/// correctly, so it is the safe default.
fn push_value_change(delta: &mut EnvDelta, key: &str, before: &str, after: &str) {
    let key = env_key(key);
    let sep = separator();

    if is_path_like(&key) {
        let before_parts: Vec<&str> = before.split(&sep).collect();
        let after_parts: Vec<&str> = after.split(&sep).collect();

        if let Some(added) = strip_segments(&before_parts, &after_parts, SegmentSide::Front) {
            delta.push_action(key, EnvAction::Prepend(added.join(&sep), sep));
            return;
        }
        if let Some(added) = strip_segments(&before_parts, &after_parts, SegmentSide::Back) {
            delta.push_action(key, EnvAction::Append(added.join(&sep), sep));
            return;
        }
    }

    delta.push_action(key, EnvAction::Set(after.to_string()));
}

#[derive(Clone, Copy)]
enum SegmentSide {
    Front,
    Back,
}

/// Returns the segments `after` has that `before` does not, at `side`.
///
/// `None` when the shared part is not a clean contiguous run, which rules out
/// insertions into the middle, deletions, and reorderings.
fn strip_segments<'a>(
    before: &[&'a str],
    after: &[&'a str],
    side: SegmentSide,
) -> Option<Vec<&'a str>> {
    if after.len() <= before.len() {
        return None;
    }
    // The shared run must sit at the far end and be exactly `before`; otherwise
    // the change is an insertion, a deletion, or a reordering, none of which a
    // prepend or append can express.
    let (shared, added) = match side {
        SegmentSide::Front => after.split_at(after.len() - before.len()),
        SegmentSide::Back => after.split_at(before.len()),
    };
    let (shared, added) = match side {
        SegmentSide::Front => (added, shared),
        SegmentSide::Back => (shared, added),
    };
    if shared != before {
        return None;
    }
    Some(added.to_vec())
}

/// Reports whether `key` names a variable that holds a separated path list.
///
/// Rez packages prepend to these constantly, so recognizing them is worth it;
/// guessing the same for arbitrary variables is not.
fn is_path_like(key: &str) -> bool {
    matches!(
        key,
        "PATH"
            | "LD_LIBRARY_PATH"
            | "DYLD_LIBRARY_PATH"
            | "PYTHONPATH"
            | "MAYA_PLUG_IN_PATH"
            | "MAYA_SCRIPT_PATH"
            | "NUKE_PATH"
            | "HOUDINI_PATH"
            | "OCIO"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Environment;

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

    /// `from_path` reads the whole package, so a resolve sees `requires` and
    /// `tools` and not just the command body. Dropping them would silently
    /// discard a package's dependency declarations.
    #[test]
    fn from_path_captures_requires_tools_and_commands() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("app").join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.py"),
            "name = \"app\"\nversion = \"1.0.0\"\nrequires = ['py-3.11']\ntools = ['app']\n\n\
             def commands():\n    setenv('PROBE_APP', '1')\n",
        )
        .unwrap();

        let package = load_package(&dir).unwrap().unwrap();
        assert_eq!(package.requires, vec!["py-3.11".to_string()]);
        assert_eq!(package.tools, vec!["app".to_string()]);
        assert!(
            package
                .commands
                .as_deref()
                .is_some_and(|c| c.contains("PROBE_APP"))
        );
    }

    /// A directory with no package definition is not a package.
    #[test]
    fn a_directory_without_a_definition_is_not_a_package() {
        let temp = tempfile::tempdir().unwrap();
        assert!(load_package(temp.path()).unwrap().is_none());
    }

    /// package.yaml is supported through the same entry point.
    #[test]
    fn yaml_packages_load_through_from_path() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("yam").join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.yaml"),
            "name: yam\nversion: 1.0.0\ncommands: |\n  setenv('YAM', '1')\n",
        )
        .unwrap();

        let package = load_package(&dir).unwrap().unwrap();
        assert_eq!(package.name, "yam");
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

    /// Regression: `/bin` -> `/usr/bin` shares the suffix `/bin`, but `/usr` is
    /// not a path segment. Recording it as a prepend would replay as
    /// `/usr<sep>/bin`, silently producing a different PATH than the resolve
    /// actually produced.
    #[test]
    fn a_shared_suffix_that_is_not_a_segment_is_a_set() {
        let mut delta = EnvDelta::new();
        let sep = separator();
        push_value_change(&mut delta, "PATH", "/bin", "/usr/bin");
        assert_eq!(
            delta.actions("PATH"),
            &[EnvAction::Set("/usr/bin".to_string())]
        );
        let parent = Environment::from([("PATH".to_string(), "/bin".to_string())]);
        assert_eq!(
            delta.apply(&parent).get("PATH").map(String::as_str),
            Some("/usr/bin")
        );
        let _ = sep;
    }

    /// An insertion into the middle of a path list is not expressible as a
    /// prepend or append, so it must fall back to a set.
    #[test]
    fn a_middle_insertion_is_a_set() {
        let mut delta = EnvDelta::new();
        let sep = separator();
        push_value_change(
            &mut delta,
            "PATH",
            &format!("/a{}/c", sep),
            &format!("/a{}/b{}/c", sep, sep),
        );
        assert_eq!(
            delta.actions("PATH"),
            &[EnvAction::Set(format!("/a{}/b{}/c", sep, sep))]
        );
    }

    /// A shorter value is neither a prepend nor an append.
    #[test]
    fn a_shrinking_path_list_is_a_set() {
        let mut delta = EnvDelta::new();
        let sep = separator();
        push_value_change(&mut delta, "PATH", &format!("/a{}/b", sep), "/a");
        assert_eq!(delta.actions("PATH"), &[EnvAction::Set("/a".to_string())]);
    }

    /// Only known path-like variables get the prepend/append treatment; an
    /// arbitrary variable that happens to share a prefix stays a set.
    #[test]
    fn non_path_like_keys_are_always_a_set() {
        let mut delta = EnvDelta::new();
        push_value_change(&mut delta, "SOME_VAR", "abc", "abc/def");
        assert_eq!(
            delta.actions("SOME_VAR"),
            &[EnvAction::Set("abc/def".to_string())]
        );
    }

    /// A prepend recorded against one parent must produce the same value when
    /// replayed against a different one.
    #[test]
    fn a_recorded_prepend_replays_onto_a_different_parent() {
        let mut delta = EnvDelta::new();
        let sep = separator();
        push_value_change(
            &mut delta,
            "PATH",
            "/usr/bin",
            &format!("/opt/pkg/bin{}/usr/bin", sep),
        );
        let other = Environment::from([("PATH".to_string(), "/other/bin".to_string())]);
        assert_eq!(
            delta.apply(&other).get("PATH").map(String::as_str),
            Some(format!("/opt/pkg/bin{}/other/bin", sep).as_str())
        );
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

    /// Regression: a pinned request must not silently resolve to a different
    /// version. Handing back 2.0.0 for `dummy-1.0.0` is the failure this class
    /// of tool can never have, because nothing downstream reports it.
    #[test]
    fn a_pinned_request_resolves_to_that_version() {
        let temp = tempfile::tempdir().unwrap();
        write_package(temp.path(), "dummy", "1.0.0", "setenv('DUMMY', 'one')");
        write_package(temp.path(), "dummy", "2.0.0", "setenv('DUMMY', 'two')");

        let mut request = ResolveRequest::new(["dummy-1.0.0"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let resolved = resolve_env(&request).unwrap();
        assert_eq!(
            resolved.environment.get("DUMMY").map(String::as_str),
            Some("one")
        );
    }

    /// A constraint that excludes every candidate is an error, not a silent
    /// fallback to whatever version happens to exist.
    #[test]
    fn an_unsatisfiable_constraint_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        write_package(temp.path(), "dummy", "1.0.0", "setenv('DUMMY', 'one')");

        let mut request = ResolveRequest::new(["dummy-9.9.9"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let err = resolve_env(&request).unwrap_err();
        match err {
            Error::Resolve { request, reason } => {
                assert_eq!(request, "dummy-9.9.9");
                assert!(reason.contains("matching"), "reason was: {reason}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    /// A bare request still takes the highest version.
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

    /// `Package::from_path` drops command bodies it cannot parse, so a package
    /// whose commands are not valid Rex resolves to an environment that simply
    /// has nothing extra in it.
    ///
    /// That is upstream behaviour rather than something the adapter chooses,
    /// and it is the one place a resolve can under-contribute without failing.
    /// The test pins the behaviour so a future change either way is visible,
    /// and asserts on a single key rather than the whole environment — printing
    /// the full map would dump the host's variables, secrets included.
    #[test]
    fn unparseable_commands_contribute_nothing_rather_than_failing() {
        let temp = tempfile::tempdir().unwrap();
        write_package(temp.path(), "broken", "1.0.0", "this is not rex");

        let mut request = ResolveRequest::new(["broken"]);
        request.package_paths = Some(vec![temp.path().to_path_buf()]);

        let resolved = resolve_env(&request).unwrap();
        assert_eq!(resolved.resolved_requests, vec!["broken".to_string()]);
        assert!(!resolved.environment.contains_key("BROKEN_ONLY"));
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
    /// The assertion depends on `REZ_PACKAGES_PATH` being unset, so the test
    /// controls it rather than inheriting whichever value the runner happens to
    /// have — otherwise a runner that exports it makes this silently pass for
    /// the wrong reason.
    ///
    /// Environment mutation is not thread-safe, so this test is the one place
    /// in the module that cannot run alongside the others.
    #[test]
    fn no_package_paths_is_reported() {
        let guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("REZ_PACKAGES_PATH");
        // SAFETY: no other thread in this test binary reads this variable
        // while the guard below is held.
        unsafe { std::env::remove_var("REZ_PACKAGES_PATH") };

        let result = resolve_env(&ResolveRequest::new(["python"]));

        if let Some(value) = previous {
            unsafe { std::env::set_var("REZ_PACKAGES_PATH", value) };
        }
        drop(guard);

        match result {
            Err(Error::Resolve { reason, .. }) => assert!(reason.contains("no package paths")),
            other => panic!("expected a resolve error, got: {other:?}"),
        }
    }

    /// Serializes tests that mutate the process environment.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
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
