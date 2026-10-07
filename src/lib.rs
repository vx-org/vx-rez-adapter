//! `vx-rez-adapter` bridges [vx](https://github.com/vx-org) and
//! [rez-next](https://github.com/loonghao/rez-next) package environments.
//!
//! The crate is intended to expose two capabilities:
//!
//! 1. Resolving the environment of one or more rez packages.
//! 2. Launching a tool as a child process with that environment applied.
//!
//! This is the initial scaffolding release. The module layout and the shape of
//! the public surface are in place, but the resolution and launch paths are not
//! implemented yet: they deliberately return [`Error::NotImplemented`] so the
//! crate compiles and releases while the upstream SDK dependency is still being
//! settled.

#![deny(missing_docs)]

use std::collections::BTreeMap;
use std::path::PathBuf;

pub mod env;
pub mod error;
pub mod launch;

pub use env::{EnvDelta, ResolvedEnv};
pub use error::{Error, Result};
pub use launch::{LaunchOutcome, LaunchRequest};

/// A request to resolve the environment of a set of rez packages.
///
/// Package selectors use rez syntax, e.g. `"python-3.11"` or `"maya-2024"`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveRequest {
    /// Package requests to resolve, in rez request syntax.
    pub requests: Vec<String>,
    /// Additional packages required by the caller but not part of the resolve.
    pub implicit_requests: Vec<String>,
    /// Paths searched for packages, overriding the ambient configuration.
    pub package_paths: Option<Vec<PathBuf>>,
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
        }
    }
}

/// Resolves rez package environments and launches tools inside them.
///
/// This is the top-level entry point of the crate. Both verbs are unimplemented
/// in this release; see the crate-level docs for the roadmap.
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
    /// # Errors
    ///
    /// Always returns [`Error::NotImplemented`] in this release.
    pub fn resolve_env(&self, request: &ResolveRequest) -> Result<ResolvedEnv> {
        Err(Error::NotImplemented {
            what: format!("resolve_env for [{}]", request.requests.join(", ")),
        })
    }

    /// Launches `request.program` with the resolved environment applied.
    ///
    /// # Errors
    ///
    /// Always returns [`Error::NotImplemented`] in this release.
    pub fn launch(&self, request: &LaunchRequest) -> Result<LaunchOutcome> {
        Err(Error::NotImplemented {
            what: format!("launch of {:?}", request.program),
        })
    }
}

/// An ordered map of environment variable name to value.
///
/// `BTreeMap` keeps the rendering of an environment deterministic, which
/// matters for diffing and for reproducible launches.
pub type Environment = BTreeMap<String, String>;
