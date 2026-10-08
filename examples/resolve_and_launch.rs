//! Resolve a package environment and forward the command after `--` directly.
//!
//! ```sh
//! vx cargo run --example resolve_and_launch -- --packages-path /srv/packages python-3.11 -- python --version
//! ```

use std::collections::VecDeque;
use std::path::PathBuf;

use vx_rez_adapter::{LaunchRequest, ResolveRequest, RezAdapter};

const USAGE: &str =
    "Usage: resolve_and_launch [--packages-path PATH] <request>... -- <program> [arguments...]";

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let mut arguments: VecDeque<String> = std::env::args().skip(1).collect();
    if arguments
        .front()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        println!("{USAGE}");
        return Ok(0);
    }

    let mut paths = Vec::<PathBuf>::new();
    let mut requests = Vec::new();
    let mut command_separator = false;
    while let Some(argument) = arguments.pop_front() {
        match argument.as_str() {
            "--" => {
                command_separator = true;
                break;
            }
            "--packages-path" => {
                let path = arguments
                    .pop_front()
                    .ok_or("--packages-path requires a path")?;
                paths.push(PathBuf::from(path));
            }
            value if value.starts_with("--") => {
                return Err(format!("unknown option {value}; {USAGE}").into());
            }
            _ => requests.push(argument),
        }
    }
    if requests.is_empty() || !command_separator {
        return Err(USAGE.into());
    }
    let program = arguments.pop_front().ok_or(USAGE)?;
    let mut request = ResolveRequest::new(requests);
    if !paths.is_empty() {
        request = request.package_paths(paths);
    }

    let adapter = RezAdapter::new();
    let resolved = adapter.resolve_env(&request)?;
    let outcome = adapter.launch(
        &LaunchRequest::new(program)
            .args(arguments)
            .environment(resolved.environment),
    )?;
    Ok(outcome
        .code
        .unwrap_or_else(|| 128 + outcome.signal.unwrap_or(1)))
}
