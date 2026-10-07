//! Environment representation produced by a rez resolve.
//!
//! A rez resolve does not produce a bare set of variables: it produces the
//! difference against the parent environment (paths get prepended, variables
//! get overridden, some get unset). [`EnvDelta`] models that difference and
//! [`ResolvedEnv`] models the result of applying it.
//!
//! Every package in a resolve emits its own environment commands, so a delta
//! must be able to hold **several actions for the same variable**. Nearly every
//! rez package prepends to `PATH`; a delta that kept only the last action for a
//! variable would silently drop the others, which is the worst failure mode a
//! package-manager bridge can have — a missing path entry with no error.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::Environment;

/// How a single environment variable is modified by one package command.
///
/// The variants mirror the upstream `rez-next-context` contract
/// (`EnvOperation`) so stage 2 can convert between the two without loss.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvAction {
    /// The variable is removed from the environment.
    Unset,
    /// The variable is set to this exact value, replacing any previous one.
    Set(String),
    /// The value is prepended to the variable using the given separator.
    /// An absent variable is treated as empty.
    Prepend(String, String),
    /// The value is appended to the variable using the given separator.
    /// An absent variable is treated as empty.
    Append(String, String),
    /// The variable is set only when it is absent or empty in the parent
    /// environment.
    SetIfEmpty(String),
}

/// The ordered set of variable changes a resolve applies to its parent.
///
/// Keys are stored in a `BTreeMap` so rendering a delta twice yields
/// byte-identical output, which keeps environment diffs reviewable. Each key
/// maps to a `Vec` because a resolve may apply many actions to one variable,
/// and they take effect in insertion order.
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
/// use vx_rez_adapter::{EnvAction, EnvDelta, ResolvedEnv, path_separator};
///
/// let sep = path_separator();
/// let mut delta = EnvDelta::new();
///
/// // Every package in a resolve emits its own commands, so a delta holds a
/// // *list* of actions per variable. These two both survive:
/// delta.push_action("PATH", EnvAction::Prepend("/pkg/a/bin".to_string(), sep.to_string()));
/// delta.push_action("PATH", EnvAction::Prepend("/pkg/b/bin".to_string(), sep.to_string()));
/// delta.push_action("REZ_USED", EnvAction::Set("1".to_string()));
/// delta.push_action("OLD_VAR", EnvAction::Unset);
///
/// let parent = BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
/// let resolved = ResolvedEnv::from_delta(delta, &parent);
///
/// // Later prepends land in front, matching rez activation order.
/// let path = &resolved.environment["PATH"];
/// assert!(path.starts_with(&format!("/pkg/b/bin{sep}/pkg/a/bin{sep}")));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvDelta {
    actions: BTreeMap<String, Vec<EnvAction>>,
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

    /// Appends an action for `key`.
    ///
    /// Actions for the same key accumulate in call order rather than
    /// overwriting, so multiple packages contributing to `PATH` are all kept.
    pub fn push_action(&mut self, key: impl Into<String>, action: EnvAction) {
        self.actions.entry(key.into()).or_default().push(action);
    }

    /// Replaces all actions recorded for `key` with a single `action`.
    ///
    /// Use this only when a later command is meant to override everything that
    /// came before it for that variable.
    pub fn set_action(&mut self, key: impl Into<String>, action: EnvAction) {
        self.actions.insert(key.into(), vec![action]);
    }

    /// Looks up the actions recorded for `key`, in application order.
    #[must_use]
    pub fn actions(&self, key: &str) -> &[EnvAction] {
        self.actions.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Iterates over the recorded `(key, actions)` pairs in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Vec<EnvAction>)> {
        self.actions.iter()
    }

    /// Total number of actions across all keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.actions.values().map(Vec::len).sum()
    }

    /// Applies the delta to `parent` and returns a new environment.
    ///
    /// `parent` is left untouched, so the same delta can be applied to several
    /// parents (for example to diff a resolve against two shells).
    #[must_use]
    pub fn apply(&self, parent: &Environment) -> Environment {
        let mut out = parent.clone();
        for (key, actions) in &self.actions {
            for action in actions {
                match action {
                    EnvAction::Unset => {
                        out.remove(key);
                    }
                    EnvAction::Set(value) => {
                        out.insert(key.clone(), value.clone());
                    }
                    EnvAction::SetIfEmpty(value) => {
                        let is_empty = out.get(key).is_none_or(|v| v.is_empty());
                        if is_empty {
                            out.insert(key.clone(), value.clone());
                        }
                    }
                    EnvAction::Prepend(value, sep) => {
                        let entry = out.entry(key.clone()).or_default();
                        join_path_like(entry, value, sep, JoinSide::Prepend);
                    }
                    EnvAction::Append(value, sep) => {
                        let entry = out.entry(key.clone()).or_default();
                        join_path_like(entry, value, sep, JoinSide::Append);
                    }
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

/// Joins `value` into `existing` using `sep`, skipping the separator when
/// either side is empty so no stray leading/trailing separator appears.
fn join_path_like(existing: &mut String, value: &str, sep: &str, side: JoinSide) {
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
            existing.push_str(sep);
            existing.push_str(value);
        }
    }
}

/// Returns the platform path list separator (`:` on unix, `;` on windows).
///
/// Prefer carrying an explicit separator on each [`EnvAction`]; this helper
/// exists for callers that need to build actions against the current host.
#[must_use]
pub fn path_separator() -> &'static str {
    if cfg!(windows) { ";" } else { ":" }
}

/// Normalizes an environment variable name for use as a map key.
///
/// Windows environment variables are case-insensitive, so `Path` and `PATH`
/// name the same variable. Folding to uppercase there keeps a resolve from
/// producing two entries that the OS would then collapse unpredictably. On
/// unix, names are case-sensitive and pass through unchanged.
///
/// This matches the upstream `rez-next-context` key normalization.
#[must_use]
pub fn env_key(name: &str) -> String {
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

/// The outcome of a successful resolve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
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
    /// Rendering is infallible: the returned `Vec` is already sorted because
    /// [`Environment`] is a `BTreeMap`, so no `Result` is needed.
    #[must_use]
    pub fn to_sorted_lines(&self) -> Vec<String> {
        self.environment
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sep() -> &'static str {
        path_separator()
    }

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
        delta.push_action("REZ_USED", EnvAction::Set("2".to_string()));
        let out = delta.apply(&parent());
        assert_eq!(out.get("REZ_USED").map(String::as_str), Some("2"));
    }

    #[test]
    fn unset_removes_variable() {
        let mut delta = EnvDelta::new();
        delta.push_action("REZ_USED", EnvAction::Unset);
        let out = delta.apply(&parent());
        assert!(!out.contains_key("REZ_USED"));
    }

    #[test]
    fn prepend_uses_given_separator() {
        let mut delta = EnvDelta::new();
        delta.push_action(
            "PATH",
            EnvAction::Prepend("/opt/rez/bin".to_string(), sep().to_string()),
        );
        let out = delta.apply(&parent());
        assert_eq!(
            out.get("PATH").map(String::as_str),
            Some(format!("/opt/rez/bin{}/usr/bin", sep()).as_str())
        );
    }

    #[test]
    fn prepend_onto_absent_variable_does_not_emit_leading_separator() {
        let mut delta = EnvDelta::new();
        delta.push_action(
            "MAYA_PATH",
            EnvAction::Prepend("/opt/maya".to_string(), sep().to_string()),
        );
        let out = delta.apply(&parent());
        assert_eq!(out.get("MAYA_PATH").map(String::as_str), Some("/opt/maya"));
    }

    #[test]
    fn append_to_existing_variable() {
        let mut delta = EnvDelta::new();
        delta.push_action(
            "PATH",
            EnvAction::Append("/opt/extra".to_string(), sep().to_string()),
        );
        let out = delta.apply(&parent());
        assert_eq!(
            out.get("PATH").map(String::as_str),
            Some(format!("/usr/bin{}/opt/extra", sep()).as_str())
        );
    }

    /// The regression that motivated the multi-action model: two packages
    /// prepending to PATH must both survive.
    #[test]
    fn two_packages_prepending_path_both_survive() {
        let mut delta = EnvDelta::new();
        delta.push_action(
            "PATH",
            EnvAction::Prepend("/pkg/a/bin".to_string(), sep().to_string()),
        );
        delta.push_action(
            "PATH",
            EnvAction::Prepend("/pkg/b/bin".to_string(), sep().to_string()),
        );
        let out = delta.apply(&parent());
        let s = sep();
        assert_eq!(
            out.get("PATH").map(String::as_str),
            Some(format!("/pkg/b/bin{s}/pkg/a/bin{s}/usr/bin").as_str())
        );
    }

    /// Later prepends land in front of earlier ones, matching rez activation
    /// order: the last package activated wins the front of PATH.
    #[test]
    fn repeated_prepends_keep_insertion_order() {
        let mut delta = EnvDelta::new();
        for p in ["/one", "/two", "/three"] {
            delta.push_action("PATH", EnvAction::Prepend(p.to_string(), sep().to_string()));
        }
        let out = delta.apply(&parent());
        let s = sep();
        assert_eq!(
            out.get("PATH").map(String::as_str),
            Some(format!("/three{s}/two{s}/one{s}/usr/bin").as_str())
        );
    }

    #[test]
    fn prepend_then_append_on_same_key_both_apply() {
        let mut delta = EnvDelta::new();
        delta.push_action(
            "PATH",
            EnvAction::Prepend("/front".to_string(), sep().to_string()),
        );
        delta.push_action(
            "PATH",
            EnvAction::Append("/back".to_string(), sep().to_string()),
        );
        let out = delta.apply(&parent());
        let s = sep();
        assert_eq!(
            out.get("PATH").map(String::as_str),
            Some(format!("/front{s}/usr/bin{s}/back").as_str())
        );
    }

    #[test]
    fn many_actions_on_one_key_are_all_recorded() {
        let mut delta = EnvDelta::new();
        delta.push_action("PATH", EnvAction::Unset);
        delta.push_action(
            "PATH",
            EnvAction::Prepend("/a".to_string(), sep().to_string()),
        );
        delta.push_action(
            "PATH",
            EnvAction::Append("/b".to_string(), sep().to_string()),
        );
        assert_eq!(delta.actions("PATH").len(), 3);
        assert_eq!(delta.len(), 3);
    }

    #[test]
    fn set_action_replaces_prior_actions() {
        let mut delta = EnvDelta::new();
        delta.push_action(
            "PATH",
            EnvAction::Prepend("/a".to_string(), sep().to_string()),
        );
        delta.set_action("PATH", EnvAction::Set("/only".to_string()));
        assert_eq!(delta.actions("PATH").len(), 1);
        assert_eq!(
            delta.apply(&parent()).get("PATH").map(String::as_str),
            Some("/only")
        );
    }

    #[test]
    fn set_if_empty_fills_absent_variable() {
        let mut delta = EnvDelta::new();
        delta.push_action("MAYA_VERSION", EnvAction::SetIfEmpty("2024".to_string()));
        let out = delta.apply(&parent());
        assert_eq!(out.get("MAYA_VERSION").map(String::as_str), Some("2024"));
    }

    #[test]
    fn set_if_empty_leaves_existing_value_alone() {
        let mut delta = EnvDelta::new();
        delta.push_action("REZ_USED", EnvAction::SetIfEmpty("9".to_string()));
        let out = delta.apply(&parent());
        assert_eq!(out.get("REZ_USED").map(String::as_str), Some("1"));
    }

    #[test]
    fn set_if_empty_treats_empty_string_as_empty() {
        let mut parent = parent();
        parent.insert("BLANK".to_string(), String::new());
        let mut delta = EnvDelta::new();
        delta.push_action("BLANK", EnvAction::SetIfEmpty("filled".to_string()));
        let out = delta.apply(&parent);
        assert_eq!(out.get("BLANK").map(String::as_str), Some("filled"));
    }

    #[test]
    fn applying_empty_value_is_a_no_op() {
        let mut delta = EnvDelta::new();
        delta.push_action("PATH", EnvAction::Prepend(String::new(), sep().to_string()));
        assert_eq!(delta.apply(&parent()), parent());
    }

    #[test]
    fn custom_separator_is_honoured() {
        let mut delta = EnvDelta::new();
        delta.push_action("LIST", EnvAction::Append("b".to_string(), ",".to_string()));
        let mut parent = parent();
        parent.insert("LIST".to_string(), "a".to_string());
        let out = delta.apply(&parent);
        assert_eq!(out.get("LIST").map(String::as_str), Some("a,b"));
    }

    #[test]
    fn from_delta_records_rendered_environment() {
        let mut delta = EnvDelta::new();
        delta.push_action("REZ_USED", EnvAction::Set("3".to_string()));
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
        assert_eq!(
            resolved.to_sorted_lines(),
            vec!["PATH=/usr/bin".to_string(), "REZ_USED=1".to_string()]
        );
    }

    #[test]
    fn parent_is_not_mutated_by_apply() {
        let before = parent();
        let mut delta = EnvDelta::new();
        delta.push_action(
            "PATH",
            EnvAction::Prepend("/opt/rez/bin".to_string(), sep().to_string()),
        );
        let _ = delta.apply(&before);
        assert_eq!(before.get("PATH").map(String::as_str), Some("/usr/bin"));
    }

    #[test]
    fn env_key_folds_case_only_on_windows() {
        if cfg!(windows) {
            assert_eq!(env_key("Path"), "PATH");
            assert_eq!(env_key("path"), "PATH");
        } else {
            assert_eq!(env_key("Path"), "Path");
        }
    }

    /// On Windows, keys differing only by case must collapse to one entry, or
    /// the OS picks one arbitrarily when the environment reaches a child. On
    /// unix the names stay distinct, because there the OS treats them as two
    /// different variables.
    #[test]
    fn case_variant_keys_collapse_only_where_the_os_is_case_insensitive() {
        let mut delta = EnvDelta::new();
        delta.push_action(
            env_key("Path"),
            EnvAction::Prepend("/from-path".to_string(), sep().to_string()),
        );
        delta.push_action(
            env_key("PATH"),
            EnvAction::Prepend("/from-PATH".to_string(), sep().to_string()),
        );
        let out = delta.apply(&parent());
        let path_entries = out
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("PATH"))
            .count();

        if cfg!(windows) {
            assert_eq!(path_entries, 1);
        } else {
            assert_eq!(path_entries, 2);
        }
    }
}
