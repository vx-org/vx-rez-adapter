//! `vx-rez-adapter` bridges [vx](https://github.com/vx-org) and
//! [rez-next](https://github.com/vx-org/rez-next) package environments.
//!
//! The crate is intended to expose two capabilities:
//!
//! 1. Resolving the environment of one or more rez packages.
//! 2. Launching a tool as a child process with that environment applied.
//!
//! Both capabilities are implemented on top of the `rez-next` SDK:
//!
//! - [`RezAdapter::resolve_env`] delegates repository discovery, dependency
//!   resolution, variant selection, and Rex activation to `rez-next-runtime`.
//! - [`RezAdapter::launch`] spawns the program with that environment applied
//!   and reports its exit status.

#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::path::PathBuf;

pub mod env;
pub mod error;
pub mod launch;
pub mod process;
pub mod resolve;

pub use env::{EnvAction, EnvDelta, ResolvedEnv, env_key, path_separator};
pub use error::{Error, Result};
pub use launch::{LaunchOutcome, LaunchRequest};
pub use process::which;
pub use resolve::{resolve_env, resolve_env_async};

/// The platform and architecture used to select package variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveTarget {
    /// Rez platform name, such as `windows`, `linux`, or `osx`.
    pub platform: String,
    /// Rez architecture name, such as `AMD64`, `x86_64`, or `arm64`.
    pub architecture: String,
}

/// A request to resolve the environment of a set of rez packages.
///
/// Package selectors use rez syntax, e.g. `"python-3.11"` or `"maya-2024"`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveRequest {
    /// Package requests to resolve, in rez request syntax.
    pub requests: Vec<String>,
    /// Additional package requests included in the same dependency solve.
    pub implicit_requests: Vec<String>,
    /// Paths searched for packages, overriding the ambient configuration.
    pub package_paths: Option<Vec<PathBuf>>,
    /// Explicit target for variant selection; `None` adds no target constraints.
    pub target: Option<ResolveTarget>,
    /// Base environment for activation; `None` inherits the current process.
    /// An explicit empty map starts activation without parent variables.
    pub parent_environment: Option<Environment>,
}

impl ResolveRequest {
    /// Creates a request for the given package selectors.
    #[must_use]
    pub fn new<I, S>(requests: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            requests: requests.into_iter().map(Into::into).collect(),
            implicit_requests: Vec::new(),
            package_paths: None,
            target: None,
            parent_environment: None,
        }
    }

    /// Overrides the package search paths.
    ///
    /// Without this the resolve falls back to the `REZ_PACKAGES_PATH`
    /// environment variable.
    #[must_use]
    pub fn package_paths<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.package_paths = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Adds package requests to the same dependency solve.
    #[must_use]
    pub fn implicit_requests<I, S>(mut self, requests: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.implicit_requests = requests.into_iter().map(Into::into).collect();
        self
    }

    /// Selects package variants for an explicit platform and architecture.
    #[must_use]
    pub fn target(mut self, platform: impl Into<String>, architecture: impl Into<String>) -> Self {
        self.target = Some(ResolveTarget {
            platform: platform.into(),
            architecture: architecture.into(),
        });
        self
    }

    /// Supplies the exact base environment used by package activation.
    ///
    /// Pass an empty map to prevent ambient variables from entering the
    /// resolved environment. Windows variable names are normalized to uppercase.
    #[must_use]
    pub fn parent_environment<I, K, V>(mut self, environment: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.parent_environment = Some(
            environment
                .into_iter()
                .map(|(key, value)| (env_key(&key.into()), value.into()))
                .collect(),
        );
        self
    }
}

/// Resolves rez package environments and launches tools inside them.
///
/// This is the top-level entry point of the crate.
#[derive(Debug, Clone, Default)]
pub struct RezAdapter;

impl RezAdapter {
    /// Creates an adapter with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Resolves the environment described by `request`.
    ///
    /// Packages are looked up under [`ResolveRequest::package_paths`], or under
    /// `REZ_PACKAGES_PATH` when the request does not override them. Their Rex
    /// commands are then applied to the requested parent environment.
    /// In an existing Tokio runtime, use [`Self::resolve_env_async`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Resolve`] when a request cannot be parsed or no package
    /// satisfies it, and [`Error::PackagePath`] when a configured package path
    /// cannot be read.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use vx_rez_adapter::{RezAdapter, ResolveRequest};
    ///
    /// let request = ResolveRequest::new(["python-3.11"])
    ///     .package_paths(["/packages"]);
    /// let resolved = RezAdapter::new().resolve_env(&request)?;
    /// println!("{}", resolved.environment["PATH"]);
    /// # Ok::<(), vx_rez_adapter::Error>(())
    /// ```
    pub fn resolve_env(&self, request: &ResolveRequest) -> Result<ResolvedEnv> {
        resolve::resolve_env(request)
    }

    /// Resolves packages and their environment inside an existing async runtime.
    ///
    /// # Errors
    ///
    /// Returns the same repository, dependency, and activation diagnostics as
    /// [`Self::resolve_env`]. This method never creates a nested Tokio runtime.
    pub async fn resolve_env_async(&self, request: &ResolveRequest) -> Result<ResolvedEnv> {
        resolve::resolve_env_async(request).await
    }

    /// Launches `request.program` with the resolved environment applied.
    ///
    /// The child inherits this process's stdin, stdout, and stderr, and its
    /// exit code and terminating signal are returned in a [`LaunchOutcome`].
    ///
    /// A program that runs and exits non-zero is a **successful launch**: this
    /// returns `Ok(outcome)` with [`LaunchOutcome::success`] reporting `false`.
    /// Only a failure to *start* the program is an `Err`. Callers must check
    /// `success()` — inspecting only the `Result` cannot distinguish a tool
    /// that ran and failed from one that ran and succeeded.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawn`] when the program cannot be started.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use vx_rez_adapter::{LaunchRequest, RezAdapter, ResolveRequest};
    ///
    /// let adapter = RezAdapter::new();
    /// let resolved = adapter.resolve_env(&ResolveRequest::new(["python-3.11"]))?;
    ///
    /// let outcome = adapter.launch(
    ///     &LaunchRequest::new("python")
    ///         .arg("--version")
    ///         .environment(resolved.environment),
    /// )?;
    /// assert!(outcome.success());
    /// # Ok::<(), vx_rez_adapter::Error>(())
    /// ```
    pub fn launch(&self, request: &LaunchRequest) -> Result<LaunchOutcome> {
        process::launch(request)
    }
}

/// An ordered map of environment variable name to value.
///
/// `BTreeMap` keeps the rendering of an environment deterministic, which
/// matters for diffing and for reproducible launches.
///
/// Keys should be built with [`env_key`] so that the case-insensitive
/// environment of Windows collapses `Path` and `PATH` into one entry instead of
/// handing a child process two competing definitions.
pub type Environment = BTreeMap<String, String>;
