use rstest::rstest;

use vx_rez_adapter::{Environment, LaunchRequest, RezAdapter};

#[rstest]
fn test_launch_explicit_empty_environment_clears_parent() {
    let request = LaunchRequest::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "test_child_empty_environment"])
        .environment(Environment::new());
    assert!(RezAdapter::new().launch(&request).unwrap().success());
}

#[rstest]
fn test_launch_explicit_environment_is_exact() {
    let request = LaunchRequest::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "test_child_exact_environment"])
        .environment(Environment::from([(
            "VX_ADAPTER_ENV_PROOF".to_owned(),
            "exact".to_owned(),
        )]));
    assert!(RezAdapter::new().launch(&request).unwrap().success());
}

#[rstest]
#[ignore = "executed by the parent launch contract with an empty environment"]
fn test_child_empty_environment() {
    assert_eq!(std::env::vars_os().count(), 0);
}

#[rstest]
#[ignore = "executed by the parent launch contract with an exact environment"]
fn test_child_exact_environment() {
    assert_eq!(std::env::var("VX_ADAPTER_ENV_PROOF").unwrap(), "exact");
    assert_eq!(std::env::vars_os().count(), 1);
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
