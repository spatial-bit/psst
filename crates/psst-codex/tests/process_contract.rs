use std::process::Command;

#[test]
fn version_is_exact_and_unknown_arguments_fail_before_activation() {
    let binary = env!("CARGO_BIN_EXE_psst-codex");
    let version = Command::new(binary).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(version.stdout, b"psst-codex 0.1.0-alpha.1\n");
    assert!(version.stderr.is_empty());

    let failure = Command::new(binary).arg("--unknown").output().unwrap();
    assert_eq!(failure.status.code(), Some(64));
    assert!(failure.stdout.is_empty());
    assert_eq!(
        failure.stderr,
        b"psst-codex: unexpected command-line arguments\n"
    );
}
