//! Fixture-driven acceptance tests: each `tests/fixtures/<case>/` directory
//! is a worked example isolating one gafos feature. For every case, this
//! copies the case's `spec.yaml`, `gafos.yaml`, and `route.yaml` (when
//! present) into a fresh temp dir, runs the built binary, and asserts the
//! produced `route.yaml` matches the case's committed `expected.yaml`
//! byte-for-byte. A second `--check` run then proves the result is
//! considered in sync (idempotence).

use std::path::{Path, PathBuf};
use std::process::Command;

fn gafos_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gafos")
}

fn fixture_dir(case: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(case)
}

fn run_gafos(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(gafos_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("should run gafos binary")
}

/// Copy a case's inputs into a fresh temp dir, run gafos, assert the
/// produced `route.yaml` matches `expected.yaml` byte-for-byte, then run
/// `--check` and assert it reports the file as in sync.
fn run_case(case: &str) {
    let case_dir = fixture_dir(case);

    let spec_src = case_dir.join("spec.yaml");
    let config_src = case_dir.join("gafos.yaml");
    let route_src = case_dir.join("route.yaml");
    let expected_path = case_dir.join("expected.yaml");

    assert!(
        spec_src.is_file(),
        "case {case}: missing spec.yaml at {}",
        spec_src.display()
    );
    assert!(
        config_src.is_file(),
        "case {case}: missing gafos.yaml at {}",
        config_src.display()
    );
    assert!(
        expected_path.is_file(),
        "case {case}: missing expected.yaml at {}",
        expected_path.display()
    );

    let temp = tempfile::tempdir().expect("should create temp dir");
    let dir = temp.path();

    std::fs::copy(&spec_src, dir.join("spec.yaml"))
        .unwrap_or_else(|e| panic!("case {case}: copying spec.yaml: {e}"));
    std::fs::copy(&config_src, dir.join("gafos.yaml"))
        .unwrap_or_else(|e| panic!("case {case}: copying gafos.yaml: {e}"));
    if route_src.is_file() {
        std::fs::copy(&route_src, dir.join("route.yaml"))
            .unwrap_or_else(|e| panic!("case {case}: copying route.yaml: {e}"));
    }

    let output = run_gafos(dir, &["--config", "gafos.yaml"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "case {case}: expected exit 0, got {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let produced = std::fs::read(dir.join("route.yaml"))
        .unwrap_or_else(|e| panic!("case {case}: reading produced route.yaml: {e}"));
    let expected = std::fs::read(&expected_path)
        .unwrap_or_else(|e| panic!("case {case}: reading expected.yaml: {e}"));

    assert!(
        produced == expected,
        "case {case}: produced route.yaml does not match expected.yaml byte-for-byte\n\
         --- produced ---\n{}\n--- expected ---\n{}",
        String::from_utf8_lossy(&produced),
        String::from_utf8_lossy(&expected)
    );

    let checked = run_gafos(dir, &["--config", "gafos.yaml", "--check"]);
    assert_eq!(
        checked.status.code(),
        Some(0),
        "case {case}: expected --check to report in-sync (exit 0), got {:?}\nstdout: {}\nstderr: {}",
        checked.status.code(),
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn path_only() {
    run_case("path-only");
}

#[test]
fn methods() {
    run_case("methods");
}

#[test]
fn path_regex() {
    run_case("path-regex");
}

#[test]
fn path_prefix() {
    run_case("path-prefix");
}

#[test]
fn base_path() {
    run_case("base-path");
}

#[test]
fn query_params() {
    run_case("query-params");
}

#[test]
fn header_params() {
    run_case("header-params");
}

#[test]
fn backend_preserved() {
    run_case("backend-preserved");
}

#[test]
fn scaffold_missing() {
    run_case("scaffold-missing");
}
