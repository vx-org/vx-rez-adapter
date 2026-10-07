//! Environment representation produced by a rez resolve.
//!
//! A rez resolve does not produce a bare set of variables: it produces the
//! difference against the parent environment (paths get prepended, variables
//! get overridden, some get unset). [`EnvDelta`] models that difference and
//! [`ResolvedEnv`] models the result of applying it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::Environment;
use crate::error::Result;

/// How a single environment variable is modified by a resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvAction {
    /// The variable is removed from the environment.
    Unset,
    /// The variable is set to this exact value, replacing any previous one.
    Set(String),
    /// The value is prepended to the variable, separated by the platform path
    /// separator. An absent variable is treated as empty.
    Prepend(String),
    /// The value is appended to the variable, separated by the platform path
    /// separator. An absent variable is treated as empty.
    Append(String),
}

/// The ordered set of variable changes a resolve applies to its parent.
///
/// Stored as a `BTreeMap` so that rendering a delta twice yields byte-identical
/// output, which keeps environment diffs reviewable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvDelta {
    actions: BTreeMap<String, EnvAction>,
}

impl EnvDelta {
    /// Creates an empty delta.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when the delta changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Records an action for `key`, replacing any previous action for it.
    pub fn set_action(&mut self, key: impl Into<String>, action: EnvAction) {
        self.actions.insert(key.into(), action);
    }

    /// Looks up the action recorded for `key`.
    #[must_use]
    pub fn action(&self, key: &str) -> Option<&EnvAction> {
        self.actions.get(key)
    }

    /// Iterates over the recorded `(key, action)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &EnvAction)> {
        self.actions.iter()
    }

    /// Applies the delta to `parent` and returns a new environment.
    ///
    /// `parent` is left untouched, so the same delta can be applied to several
    /// parents (for example to diff a resolve against two shells).
    #[must_use]
    pub fn apply(&self, parent: &Environment) -> Environment {
        let mut out = parent.clone();
        for (key, action) in &self.actions {
            match action {
                EnvAction::Unset => {
                    out.remove(key);
                }
                EnvAction::Set(value) => {
                    out.insert(key.clone(), value.clone());
                }
                EnvAction::Prepend(value) => {
                    let entry = out.entry(key.clone()).or_default();
                    join_path_like(entry, value, JoinSide::Prepend);
                }
                EnvAction::Append(value) => {
                    let entry = out.entry(key.clone()).or_default();
                    join_path_like(entry, value, JoinSide::Append);
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy)]
enum JoinSide {
    Prepend,
    Append,
}

/// Joins `value` into `existing` using the platform path separator, skipping
/// the separator when either side is empty so no stray leading/trailing
/// separator is introduced.
fn join_path_like(existing: &mut String, value: &str, side: JoinSide) {
    let sep = path_separator();
    if value.is_empty() {
        return;
    }
    if existing.is_empty() {
        existing.push_str(value);
        return;
    }
    match side {
        JoinSide::Prepend => {
            let joined = format!("{value}{sep}{existing}");
            *existing = joined;
        }
        JoinSide::Append => {
            existing.push(sep);
            existing.push_str(value);
        }
    }
}

/// Returns the platform path list separator (`:` on unix, `;` on windows).
#[must_use]
pub fn path_separator() -> char {
    if cfg!(windows) { ';' } else { ':' }
}

/// The outcome of a successful resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedEnv {
    /// The fully rendered environment, ready to hand to a child process.
    pub environment: Environment,
    /// The delta against the parent environment, kept so callers can diff or
    /// re-apply the resolve against a different parent.
    pub delta: EnvDelta,
    /// The package requests that were resolved, in resolve order.
    pub resolved_requests: Vec<String>,
    /// Roots of the packages that took part in the resolve, if known.
    pub package_roots: Vec<PathBuf>,
}

impl ResolvedEnv {
    /// Builds a resolved environment from a delta and the parent it applies to.
    #[must_use]
    pub fn from_delta(delta: EnvDelta, parent: &Environment) -> Self {
        Self {
            environment: delta.apply(parent),
            delta,
            resolved_requests: Vec::new(),
            package_roots: Vec::new(),
        }
    }

    /// Renders the environment as `KEY=VALUE` lines, sorted by key.
    ///
    /// # Errors
    ///
    /// Never fails today; the signature is kept so serialization concerns stay
    /// inside the adapter rather than leaking to callers.
    pub fn to_sorted_lines(&self) -> Result<Vec<String>> {
        Ok(self
            .environment
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> Environment {
        BTreeMap::from([
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("REZ_USED".to_string(), "1".to_string()),
        ])
    }

    #[test]
    fn empty_delta_leaves_parent_untouched() {
        let delta = EnvDelta::new();
        assert!(delta.is_empty());
        assert_eq!(delta.apply(&parent()), parent());
    }

    #[test]
    fn set_overrides_existing_value() {
        let mut delta = EnvDelta::new();
        delta.set_action("REZ_USED", EnvAction::Set("2".to_string()));
        let out = delta.apply(&parent());
        assert_eq!(out.get("REZ_USED").map(String::as_str), Some("2"));
    }

    #[test]
    fn unset_removes_variable() {
        let mut delta = EnvDelta::new();
        delta.set_action("REZ_USED", EnvAction::Unset);
        let out = delta.apply(&parent());
        assert!(!out.contains_key("REZ_USED"));
    }

    #[test]
    fn prepend_uses_platform_separator() {
        let mut delta = EnvDelta::new();
        delta.set_action("PATH", EnvAction::Prepend("/opt/rez/bin".to_string()));
        let out = delta.apply(&parent());
        let sep = path_separator();
        assert_eq!(
            out.get("PATH").map(String::as_str),
            Some(format!("/opt/rez/bin{sep}/usr/bin").as_str())
        );
    }

    #[test]
    fn prepend_onto_absent_variable_does_not_emit_leading_separator() {
        let mut delta = EnvDelta::new();
        delta.set_action("MAYA_PATH", EnvAction::Prepend("/opt/maya".to_string()));
        let out = delta.apply(&parent());
        assert_eq!(out.get("MAYA_PATH").map(String::as_str), Some("/opt/maya"));
    }

    #[test]
    fn append_to_existing_variable() {
        let mut delta = EnvDelta::new();
        delta.set_action("PATH", EnvAction::Append("/opt/extra".to_string()));
        let out = delta.apply(&parent());
        let sep = path_separator();
        assert_eq!(
            out.get("PATH").map(String::as_str),
            Some(format!("/usr/bin{sep}/opt/extra").as_str())
        );
    }

    #[test]
    fn applying_empty_value_is_a_no_op() {
        let mut delta = EnvDelta::new();
        delta.set_action("PATH", EnvAction::Prepend(String::new()));
        assert_eq!(delta.apply(&parent()), parent());
    }

    #[test]
    fn from_delta_records_rendered_environment() {
        let mut delta = EnvDelta::new();
        delta.set_action("REZ_USED", EnvAction::Set("3".to_string()));
        let resolved = ResolvedEnv::from_delta(delta.clone(), &parent());
        assert_eq!(
            resolved.environment.get("REZ_USED").map(String::as_str),
            Some("3")
        );
        assert_eq!(resolved.delta, delta);
    }

    #[test]
    fn sorted_lines_are_deterministic() {
        let resolved = ResolvedEnv::from_delta(EnvDelta::new(), &parent());
        let lines = resolved.to_sorted_lines().expect("rendering never fails");
        assert_eq!(
            lines,
            vec!["PATH=/usr/bin".to_string(), "REZ_USED=1".to_string()]
        );
    }

    #[test]
    fn parent_is_not_mutated_by_apply() {
        let before = parent();
        let mut delta = EnvDelta::new();
        delta.set_action("PATH", EnvAction::Prepend("/opt/rez/bin".to_string()));
        let _ = delta.apply(&before);
        assert_eq!(before.get("PATH").map(String::as_str), Some("/usr/bin"));
    }
}
