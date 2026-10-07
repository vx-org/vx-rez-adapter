//! Describing and reporting the launch of a tool inside a resolved environment.
//!
//! This module defines the request and outcome types only. Spawning is part of
//! the next stage, which needs the rez-next resolution backend.

use std::path::{Path, PathBuf};

use crate::Environment;

/// A request to launch one program inside a prepared environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LaunchRequest {
    /// Program to execute. Resolved through `PATH` when not absolute.
    pub program: PathBuf,
    /// Arguments passed to the program, excluding `program` itself.
    pub args: Vec<String>,
    /// Full environment handed to the child. When empty the child inherits the
    /// parent process environment.
    pub environment: Environment,
    /// Working directory of the child. When `None` the parent's is inherited.
    pub working_dir: Option<PathBuf>,
}

impl LaunchRequest {
    /// Creates a launch request for `program` with no arguments.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            environment: Environment::new(),
            working_dir: None,
        }
    }

    /// Appends a single argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Appends several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the full environment handed to the child.
    #[must_use]
    pub fn environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }

    /// Sets the child's working directory.
    #[must_use]
    pub fn working_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.working_dir = Some(dir.as_ref().to_path_buf());
        self
    }
}

/// The result of running a [`LaunchRequest`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LaunchOutcome {
    /// Process exit code. `None` when the child was terminated by a signal.
    pub code: Option<i32>,
    /// The signal that terminated the child, when it was signalled.
    ///
    /// Carrying the number rather than a bare `signaled: bool` keeps the
    /// information a caller needs to report *which* signal killed the child.
    pub signal: Option<i32>,
}

impl LaunchOutcome {
    /// Returns true when the child exited with status zero.
    #[must_use]
    pub fn success(&self) -> bool {
        self.signal.is_none() && self.code == Some(0)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn builder_collects_args_and_environment() {
        let request = LaunchRequest::new("maya")
            .arg("-batch")
            .args(["-file", "scene.ma"])
            .environment(BTreeMap::from([("REZ_USED".to_string(), "1".to_string())]))
            .working_dir("/tmp");

        assert_eq!(request.program, PathBuf::from("maya"));
        assert_eq!(request.args, vec!["-batch", "-file", "scene.ma"]);
        assert_eq!(
            request.environment.get("REZ_USED").map(String::as_str),
            Some("1")
        );
        assert_eq!(request.working_dir, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn defaults_are_empty() {
        let request = LaunchRequest::new("python");
        assert!(request.args.is_empty());
        assert!(request.environment.is_empty());
        assert!(request.working_dir.is_none());
    }

    #[test]
    fn success_requires_zero_exit_and_no_signal() {
        assert!(
            LaunchOutcome {
                code: Some(0),
                signal: None
            }
            .success()
        );
        assert!(
            !LaunchOutcome {
                code: Some(1),
                signal: None
            }
            .success()
        );
        assert!(
            !LaunchOutcome {
                code: Some(0),
                signal: Some(9)
            }
            .success()
        );
    }

    #[test]
    fn signal_number_is_retained() {
        let outcome = LaunchOutcome {
            code: None,
            signal: Some(9),
        };
        assert!(!outcome.success());
        assert_eq!(outcome.signal, Some(9));
        assert_eq!(outcome.code, None);
    }
}
