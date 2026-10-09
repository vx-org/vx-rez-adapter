//! Dependency resolution and activation through the Rez Next runtime facade.
//!
//! The SDK owns repository lookup, dependency and variant selection, package
//! materialization, and Rex activation. The adapter only converts the resulting
//! environment into its stable public representation and computes a delta.

use std::path::{Path, PathBuf};

use rez_next_runtime::RezRuntime;

use crate::env::{EnvAction, EnvDelta, ResolvedEnv, env_key, path_separator};
use crate::error::{Error, Result};
use crate::{Environment, ResolveRequest};

/// Resolves packages and their environment from synchronous code.
///
/// # Errors
///
/// Returns [`Error::Resolve`] for SDK resolution and activation failures, or
/// when called inside a Tokio runtime. Async callers must use
/// [`resolve_env_async`]. Invalid repository paths return [`Error::PackagePath`].
pub fn resolve_env(request: &ResolveRequest) -> Result<ResolvedEnv> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(resolve_error(
            request,
            "resolve_env cannot run inside a Tokio runtime; use resolve_env_async(...).await",
        ));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| {
            resolve_error(
                request,
                format!("could not start the resolve runtime: {source}"),
            )
        })?;
    runtime.block_on(resolve_env_async(request))
}

/// Resolves packages and their environment using the calling async runtime.
///
/// All explicit and implicit requests take part in one dependency solve, so
/// transitive requirements and variant constraints cannot be bypassed.
///
/// # Errors
///
/// Returns [`Error::Resolve`] for invalid requests, incompatible dependencies,
/// unsupported targets, or activation failures. Invalid repository paths return
/// [`Error::PackagePath`].
pub async fn resolve_env_async(request: &ResolveRequest) -> Result<ResolvedEnv> {
    if request.requests.is_empty() {
        return Err(resolve_error(request, "no package requests were given"));
    }
    let paths = package_paths(request)?;
    for path in &paths {
        validate_package_path(path)?;
    }

    let parent: Environment = match &request.parent_environment {
        Some(environment) => normalize_environment(environment.clone()),
        None => normalize_environment(std::env::vars().collect()),
    };
    let mut runtime = RezRuntime::new(paths)
        .map_err(|source| resolve_error(request, source.to_string()))?
        .with_parent_environment(parent.clone().into_iter().collect());
    if let Some(target) = &request.target {
        runtime = runtime
            .with_target(&target.platform, &target.architecture)
            .map_err(|source| resolve_error(request, source.to_string()))?;
    }

    let requests: Vec<String> = request
        .requests
        .iter()
        .chain(&request.implicit_requests)
        .cloned()
        .collect();
    let resolved = runtime
        .resolve(requests.clone())
        .await
        .map_err(|source| resolve_error(request, source.to_string()))?;
    let (context, environment) = resolved.into_parts();
    let environment = normalize_environment(environment.into_iter().collect());
    let delta = delta_against(&parent, &environment);
    let package_roots = context
        .resolved_packages
        .iter()
        .filter_map(|package| package.root().map(PathBuf::from))
        .collect();

    Ok(ResolvedEnv {
        environment,
        delta,
        resolved_requests: requests,
        package_roots,
    })
}

fn resolve_error(request: &ResolveRequest, reason: impl Into<String>) -> Error {
    Error::Resolve {
        request: request
            .requests
            .iter()
            .chain(&request.implicit_requests)
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        reason: reason.into(),
    }
}

fn package_paths(request: &ResolveRequest) -> Result<Vec<PathBuf>> {
    let paths = match &request.package_paths {
        Some(paths) if paths.is_empty() => {
            return Err(resolve_error(request, "package_paths was set but empty"));
        }
        Some(paths) => paths.clone(),
        None => std::env::var_os("REZ_PACKAGES_PATH")
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|path| !path.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    };
    if paths.is_empty() {
        return Err(resolve_error(
            request,
            "no package paths configured; set package_paths or REZ_PACKAGES_PATH",
        ));
    }
    Ok(paths)
}

fn validate_package_path(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path).map_err(|source| Error::PackagePath {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_dir() {
        return Err(Error::PackagePath {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "package search path must be a directory",
            ),
        });
    }
    Ok(())
}

fn normalize_environment(environment: Environment) -> Environment {
    environment
        .into_iter()
        .map(|(name, value)| (env_key(&name), value))
        .collect()
}

/// Derives an exact delta while preserving simple path prepends and appends.
fn delta_against(parent: &Environment, environment: &Environment) -> EnvDelta {
    let mut delta = EnvDelta::new();
    let mut keys: Vec<&String> = parent.keys().chain(environment.keys()).collect();
    keys.sort_unstable();
    keys.dedup();

    for key in keys {
        match (parent.get(key), environment.get(key)) {
            (None, Some(value)) => delta.push_action(key.clone(), EnvAction::Set(value.clone())),
            (Some(_), None) => delta.push_action(key.clone(), EnvAction::Unset),
            (Some(before), Some(after)) if before != after => {
                push_value_change(&mut delta, key, before, after);
            }
            _ => {}
        }
    }
    delta
}

fn push_value_change(delta: &mut EnvDelta, key: &str, before: &str, after: &str) {
    let separator = path_separator();
    if is_path_like(key) {
        let before_parts: Vec<&str> = before.split(separator).collect();
        let after_parts: Vec<&str> = after.split(separator).collect();
        if after_parts.len() > before_parts.len() {
            if after_parts.ends_with(&before_parts) {
                let added = &after_parts[..after_parts.len() - before_parts.len()];
                delta.push_action(
                    key,
                    EnvAction::Prepend(added.join(separator), separator.to_owned()),
                );
                return;
            }
            if after_parts.starts_with(&before_parts) {
                let added = &after_parts[before_parts.len()..];
                delta.push_action(
                    key,
                    EnvAction::Append(added.join(separator), separator.to_owned()),
                );
                return;
            }
        }
    }
    delta.push_action(key, EnvAction::Set(after.to_owned()));
}

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
