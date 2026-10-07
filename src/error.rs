//! Error and result types for the adapter.

use std::io;
use std::path::PathBuf;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by the adapter.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A capability that this release does not provide yet.
    ///
    /// The adapter ships its public surface ahead of its implementation so that
    /// downstream consumers can compile against it while the resolution and
    /// launch backends land.
    NotImplemented {
        /// Description of the unimplemented operation.
        what: String,
    },

    /// A package request could not be resolved.
    Resolve {
        /// The request that failed to resolve.
        request: String,
        /// Why the resolve failed.
        reason: String,
    },

    /// A configured package path does not exist or is not readable.
    PackagePath {
        /// The offending path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// A package definition exists but could not be read.
    ///
    /// Distinct from [`Error::PackagePath`], which is about the search path:
    /// this is a package directory that is present and named like a package but
    /// whose definition the loader rejected.
    PackageDefinition {
        /// The package directory that could not be read.
        path: PathBuf,
        /// Why loading failed.
        reason: String,
    },

    /// Spawning the requested program failed.
    Spawn {
        /// The program that could not be spawned.
        program: String,
        /// The underlying I/O error.
        source: io::Error,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotImplemented { what } => {
                write!(f, "not implemented yet: {what}")
            }
            Self::Resolve { request, reason } => {
                write!(f, "failed to resolve request `{request}`: {reason}")
            }
            Self::PackagePath { path, .. } => {
                write!(f, "unusable package path `{}`", path.display())
            }
            Self::PackageDefinition { path, reason } => {
                write!(
                    f,
                    "unreadable package definition `{}`: {reason}",
                    path.display()
                )
            }
            Self::Spawn { program, .. } => {
                write!(f, "failed to spawn program `{program}`")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PackagePath { source, .. } | Self::Spawn { source, .. } => Some(source),
            Self::NotImplemented { .. } | Self::Resolve { .. } | Self::PackageDefinition { .. } => {
                None
            }
        }
    }
}

impl Error {
    /// Constructs a [`Error::NotImplemented`] for the named operation.
    #[must_use]
    pub fn not_implemented(what: impl Into<String>) -> Self {
        Self::NotImplemented { what: what.into() }
    }
}
