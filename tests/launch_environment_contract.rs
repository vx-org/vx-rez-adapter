use rstest::rstest;

use vx_rez_adapter::{Environment, LaunchRequest, RezAdapter};

#[rstest]
fn test_launch_explicit_empty_environment_clears_parent() {
    assert_launch_environment(Environment::new());
}

#[rstest]
fn test_launch_explicit_environment_is_exact() {
    assert_launch_environment(Environment::from([(
        "VX_ADAPTER_ENV_PROOF".to_owned(),
        "exact".to_owned(),
    )]));
}

fn assert_launch_environment(environment: Environment) {
    #[cfg(target_os = "macos")]
    let directory = tempfile::tempdir().unwrap();
    #[cfg(target_os = "macos")]
    let request = LaunchRequest::new(compile_environment_probe(directory.path())).args(
        environment
            .iter()
            .map(|(name, value)| format!("{name}={value}")),
    );

    #[cfg(not(target_os = "macos"))]
    let request = LaunchRequest::new(std::env::current_exe().unwrap()).args([
        "--ignored",
        "--exact",
        if environment.is_empty() {
            "test_child_empty_environment"
        } else {
            "test_child_exact_environment"
        },
    ]);

    let request = request.environment(environment);
    assert!(RezAdapter::new().launch(&request).unwrap().success());
}

#[cfg(target_os = "macos")]
fn compile_environment_probe(directory: &std::path::Path) -> std::path::PathBuf {
    // CoreFoundation can set __CF_USER_TEXT_ENCODING during initialization:
    // https://github.com/apple-oss-distributions/CF/blob/main/CFRuntime.c
    // https://github.com/apple-oss-distributions/CF/blob/main/CFStringEncodings.c
    // Check the delivered environment with a libSystem-only native executable,
    // before application frameworks can change it. No variables are exempted.
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/launch_environment_probe.c");
    let executable = directory.join("launch_environment_probe");
    let output = std::process::Command::new("/usr/bin/cc")
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("the macOS native compiler must be available for the environment probe");
    assert!(
        output.status.success(),
        "native environment probe compilation failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}

#[cfg(target_os = "macos")]
#[rstest]
#[case(false, false)]
#[case(true, false)]
#[case(true, true)]
fn test_native_environment_probe_rejects_mismatched_environment(
    #[case] expect_marker: bool,
    #[case] wrong_value: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut request = LaunchRequest::new(compile_environment_probe(directory.path()));
    let mut environment = Environment::new();
    if expect_marker {
        request = request.arg("VX_ADAPTER_ENV_PROOF=exact");
        environment.insert(
            "VX_ADAPTER_ENV_PROOF".to_owned(),
            if wrong_value { "wrong" } else { "exact" }.to_owned(),
        );
    }
    if !wrong_value {
        environment.insert(
            "VX_ADAPTER_UNEXPECTED_VARIABLE".to_owned(),
            "extra".to_owned(),
        );
    }
    let outcome = RezAdapter::new()
        .launch(&request.environment(environment))
        .unwrap();
    assert_eq!(outcome.code, Some(1));
}

#[cfg(not(target_os = "macos"))]
#[rstest]
#[ignore = "executed by the parent launch contract with an empty environment"]
fn test_child_empty_environment() {
    let variable_names = std::env::vars_os()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert!(
        variable_names.is_empty(),
        "unexpected child environment variable names: {variable_names:?}"
    );
}

#[cfg(not(target_os = "macos"))]
#[rstest]
#[ignore = "executed by the parent launch contract with an exact environment"]
fn test_child_exact_environment() {
    assert_eq!(std::env::var("VX_ADAPTER_ENV_PROOF").unwrap(), "exact");
    let variable_names = std::env::vars_os()
        .map(|(name, _)| name)
        .collect::<Vec<_>>();
    assert_eq!(
        variable_names,
        [std::ffi::OsString::from("VX_ADAPTER_ENV_PROOF")],
        "unexpected child environment variable names"
    );
}

#[cfg(windows)]
#[rstest]
#[case("absent")]
#[case("empty")]
#[case("disjoint")]
#[case("selected_native")]
#[case("selected_pathext")]
#[case("selected_extension")]
#[case("selected_relative")]
#[case("inherited")]
#[case("explicit_path")]
fn test_launch_program_lookup_respects_environment(#[case] scenario: &str) {
    let directory = tempfile::tempdir().unwrap();
    let ambient = directory.path().join("ambient");
    let selected = directory.path().join("selected");
    let empty = directory.path().join("empty");
    for path in [&ambient, &selected, &empty] {
        std::fs::create_dir(path).unwrap();
    }
    let executable = std::env::current_exe().unwrap();
    for target in [
        ambient.join("vx_adapter_path_probe.exe"),
        selected.join("vx_adapter_path_probe.exe"),
        selected.join("vx_adapter_path_probe.VXPROBE"),
    ] {
        std::fs::copy(&executable, target).unwrap();
    }

    // Run the assertion in a separate process whose ambient PATH contains a
    // competing executable, without changing this parallel test runner's env.
    let environment = Environment::from([
        ("PATH".to_owned(), ambient.display().to_string()),
        ("VX_ADAPTER_LOOKUP_SCENARIO".to_owned(), scenario.to_owned()),
        (
            "VX_ADAPTER_LOOKUP_ROOT".to_owned(),
            directory.path().display().to_string(),
        ),
        (
            "VX_ADAPTER_EXPECTED_EXECUTABLE".to_owned(),
            ambient
                .join("vx_adapter_path_probe.exe")
                .display()
                .to_string(),
        ),
    ]);
    let request = LaunchRequest::new(executable)
        .args(["--ignored", "--exact", "test_child_program_lookup_contract"])
        .environment(environment);
    assert!(
        RezAdapter::new().launch(&request).unwrap().success(),
        "program lookup failed for {scenario}"
    );
}

#[cfg(windows)]
#[rstest]
#[ignore = "executed by the parent launch contract with a competing ambient PATH"]
fn test_child_program_lookup_contract() {
    use std::io;
    use std::path::PathBuf;

    use vx_rez_adapter::Error;

    let scenario = std::env::var("VX_ADAPTER_LOOKUP_SCENARIO").unwrap();
    let root = PathBuf::from(std::env::var("VX_ADAPTER_LOOKUP_ROOT").unwrap());
    let selected = root.join("selected");
    let mut request = LaunchRequest::new("vx_adapter_path_probe").args([
        "--ignored",
        "--exact",
        "test_child_path_probe",
    ]);
    let mut environment = Environment::new();

    match scenario.as_str() {
        "absent" => {}
        "empty" => {
            environment.insert("PATH".to_owned(), String::new());
        }
        "disjoint" => {
            environment.insert("PATH".to_owned(), root.join("empty").display().to_string());
        }
        "selected_native" | "selected_pathext" | "selected_extension" | "selected_relative" => {
            let filename = if scenario == "selected_pathext" {
                "vx_adapter_path_probe.VXPROBE"
            } else {
                "vx_adapter_path_probe.exe"
            };
            environment.insert(
                "pAtH".to_owned(),
                if scenario == "selected_relative" {
                    "selected".to_owned()
                } else {
                    selected.display().to_string()
                },
            );
            environment.insert(
                "PaThExT".to_owned(),
                if scenario == "selected_pathext" {
                    ".VXPROBE".to_owned()
                } else {
                    ".CMD".to_owned()
                },
            );
            environment.insert(
                "VX_ADAPTER_EXPECTED_EXECUTABLE".to_owned(),
                selected.join(filename).display().to_string(),
            );
            if scenario == "selected_pathext" {
                // Only the custom native executable suffix may match here.
                std::fs::remove_file(selected.join("vx_adapter_path_probe.exe")).unwrap();
            } else if scenario == "selected_extension" {
                request.program = PathBuf::from(filename);
            } else if scenario == "selected_relative" {
                request = request.working_dir(&root);
            }
        }
        "inherited" => {
            assert!(RezAdapter::new().launch(&request).unwrap().success());
            return;
        }
        "explicit_path" => {
            request.program = root.join("ambient/vx_adapter_path_probe.exe");
            request.args = vec![
                "--ignored".to_owned(),
                "--exact".to_owned(),
                "test_child_empty_environment".to_owned(),
            ];
        }
        other => panic!("unknown lookup scenario {other}"),
    }

    let result = RezAdapter::new().launch(&request.environment(environment));
    if matches!(scenario.as_str(), "absent" | "empty" | "disjoint") {
        match result {
            Err(Error::Spawn { program, source }) => {
                assert_eq!(program, "vx_adapter_path_probe");
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("ambient executable was not rejected: {other:?}"),
        }
    } else {
        assert!(result.unwrap().success());
    }
}

#[cfg(windows)]
#[rstest]
#[ignore = "executed as a copied native executable by the launch lookup contract"]
fn test_child_path_probe() {
    if let Some(expected) = std::env::var_os("VX_ADAPTER_EXPECTED_EXECUTABLE") {
        assert_eq!(
            std::env::current_exe().unwrap().canonicalize().unwrap(),
            std::path::PathBuf::from(expected).canonicalize().unwrap()
        );
    }
}
