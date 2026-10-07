//! Spawning a tool inside a resolved environment.
//!
//! A launch is more than `Command::spawn`: the child has to inherit the
//! terminal, the parent has to report the child's real exit status, and on
//! Windows a launched tool must not pop a console window of its own.

use std::io;
use std::process::{Command, ExitStatus, Stdio};

use crate::error::{Error, Result};
use crate::{LaunchOutcome, LaunchRequest};

/// Launches `request` and waits for it to finish.
///
/// The child inherits the parent's stdin, stdout, and stderr so an interactive
/// tool behaves the way a user expects. Its exit code and terminating signal
/// are reported in the returned [`LaunchOutcome`].
///
/// # Errors
///
/// Returns [`Error::Spawn`] when the program cannot be started. A program that
/// starts and then exits non-zero is a successful launch with
/// [`LaunchOutcome::success`] reporting `false`.
pub fn launch(request: &LaunchRequest) -> Result<LaunchOutcome> {
    let mut command = build_command(request);
    let status = command.status().map_err(|source| Error::Spawn {
        program: request.program.display().to_string(),
        source,
    })?;
    Ok(outcome_of(status))
}

/// Builds the platform-specific command for `request`.
fn build_command(request: &LaunchRequest) -> Command {
    let mut command = Command::new(&request.program);
    command
        .args(&request.args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if !request.environment.is_empty() {
        command.env_clear();
        for (name, value) in &request.environment {
            command.env(name, value);
        }
    }

    if let Some(dir) = &request.working_dir {
        command.current_dir(dir);
    }

    // A tool launched from a GUI or a terminal emulator must not open an extra
    // console window on Windows; the child writes into the parent's streams.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command
}

/// Converts a finished child's status into a [`LaunchOutcome`].
///
/// `ExitStatus::code` is `None` when a signal killed the child, and the signal
/// number is only available through the platform extension traits, so the two
/// are read separately rather than inferred from one another.
fn outcome_of(status: ExitStatus) -> LaunchOutcome {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        LaunchOutcome {
            code: status.code(),
            signal: status.signal(),
        }
    }

    #[cfg(not(unix))]
    {
        LaunchOutcome {
            code: status.code(),
            signal: None,
        }
    }
}

/// Finds `program` on `PATH` without spawning it.
///
/// Exposed so callers can check a tool is launchable before committing to a
/// resolve, and so tests can assert on lookup behaviour.
///
/// # Errors
///
/// Returns [`Error::Spawn`] when the program cannot be found or the calling
/// process lacks permission to execute it.
pub fn which(program: &str) -> Result<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(program);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::Spawn {
        program: program.to_string(),
        source: io::Error::new(
            io::ErrorKind::NotFound,
            format!("`{program}` was not found on PATH"),
        ),
    })
}

/// Reports whether `candidate` is a file this process can run.
fn is_executable(candidate: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(candidate) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Environment;

    /// A shell is the one program every CI platform has, and it is the only
    /// portable way to exercise "run a command and observe its exit code".
    fn shell() -> &'static str {
        if cfg!(windows) { "cmd" } else { "sh" }
    }

    fn shell_arg() -> &'static str {
        if cfg!(windows) { "/C" } else { "-c" }
    }

    #[test]
    fn a_successful_child_reports_success() {
        let request = LaunchRequest::new(shell()).arg(shell_arg()).arg("exit 0");
        let outcome = launch(&request).unwrap();
        assert!(outcome.success());
        assert_eq!(outcome.code, Some(0));
        assert_eq!(outcome.signal, None);
    }

    #[test]
    fn a_failing_child_reports_its_exit_code() {
        let request = LaunchRequest::new(shell()).arg(shell_arg()).arg("exit 3");
        let outcome = launch(&request).unwrap();
        assert!(!outcome.success());
        assert_eq!(outcome.code, Some(3));
    }

    #[test]
    fn a_missing_program_is_a_spawn_error_not_a_panic() {
        let request = LaunchRequest::new("definitely-not-a-real-program-xyz");
        let err = launch(&request).unwrap_err();
        match err {
            Error::Spawn { program, .. } => {
                assert_eq!(program, "definitely-not-a-real-program-xyz");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn the_child_sees_the_environment_it_was_given() {
        let marker = format!("ADAPTER_MARKER_{}", std::process::id());
        let script = if cfg!(windows) {
            format!("if \"%{marker}%\"==\"expected\" (exit 0) else (exit 1)")
        } else {
            format!("[ \"${marker}\" = \"expected\" ]")
        };

        let mut environment = Environment::new();
        environment.insert(marker.clone(), "expected".to_string());

        let request = LaunchRequest::new(shell())
            .arg(shell_arg())
            .arg(script)
            .environment(environment);

        let outcome = launch(&request).unwrap();
        assert!(outcome.success(), "child did not observe {marker}");
    }

    #[test]
    fn an_empty_environment_leaves_the_parent_intact() {
        // No env_clear() is applied when the caller supplies no environment, so
        // the child can still see the parent's PATH.
        let script = if cfg!(windows) {
            "if defined PATH (exit 0) else (exit 1)"
        } else {
            "test -n \"$PATH\""
        };
        let request = LaunchRequest::new(shell()).arg(shell_arg()).arg(script);
        assert!(launch(&request).unwrap().success());
    }

    /// `cmd.exe` lives in System32 and is a real file, but this process's PATH
    /// need not contain it, so the test picks a program it knows is present.
    #[test]
    fn which_finds_a_program_on_path() {
        let program = if cfg!(windows) { "cmd.exe" } else { "sh" };
        let found = which(program);
        assert!(found.is_ok(), "expected to find {program} on PATH");
        assert!(found.unwrap().is_file());
    }

    #[test]
    fn which_reports_a_missing_program() {
        let err = which("definitely-not-a-real-program-xyz").unwrap_err();
        assert!(matches!(err, Error::Spawn { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn a_signalled_child_reports_the_signal() {
        // `kill -TERM $$` terminates the shell with SIGTERM and no exit code.
        let request = LaunchRequest::new("sh").arg("-c").arg("kill -TERM $$");
        let outcome = launch(&request).unwrap();
        assert!(!outcome.success());
        assert_eq!(outcome.signal, Some(15));
        assert_eq!(outcome.code, None);
    }
}
