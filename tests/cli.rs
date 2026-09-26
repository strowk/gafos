//! Integration tests driving the built `gafos` binary end to end: config
//! discovery, spec parsing, rule translation, and manifest writing/checking.

use std::path::Path;
use std::process::Command;

const SPEC_HEALTH: &str = "\
openapi: 3.0.3
info:
  title: Test
  version: \"1.0\"
paths:
  /health:
    get:
      responses:
        \"200\":
          description: ok
";

const SPEC_EMPTY: &str = "\
openapi: 3.0.3
info:
  title: Test
  version: \"1.0\"
paths: {}
";

const ROUTE_SKELETON: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: myroute
spec:
  parentRefs:
    - name: gw
";

fn gafos_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gafos")
}

fn run_gafos(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(gafos_bin())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("should run gafos binary")
}

fn write_config(dir: &Path, extra: &str) {
    std::fs::write(
        dir.join("gafos.yaml"),
        format!("spec: openapi.yaml\nroute: httproute.yaml\n{extra}"),
    )
    .expect("write gafos.yaml");
}

#[test]
fn writes_route_from_spec() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "");
    std::fs::write(dir.path().join("openapi.yaml"), SPEC_HEALTH).expect("write spec");
    std::fs::write(dir.path().join("httproute.yaml"), ROUTE_SKELETON).expect("write route");

    let output = run_gafos(dir.path(), &[]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));

    let written = std::fs::read_to_string(dir.path().join("httproute.yaml")).expect("read route");
    assert!(written.contains("rules:"), "written manifest: {written}");
    assert!(written.contains("/health"), "written manifest: {written}");
}

#[test]
fn check_passes_when_in_sync() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "");
    std::fs::write(dir.path().join("openapi.yaml"), SPEC_HEALTH).expect("write spec");
    std::fs::write(dir.path().join("httproute.yaml"), ROUTE_SKELETON).expect("write route");

    let first = run_gafos(dir.path(), &[]);
    assert_eq!(first.status.code(), Some(0));

    let checked = run_gafos(dir.path(), &["--check"]);

    assert_eq!(
        checked.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );
}

#[test]
fn check_fails_when_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "");
    std::fs::write(dir.path().join("openapi.yaml"), SPEC_HEALTH).expect("write spec");
    std::fs::write(dir.path().join("httproute.yaml"), ROUTE_SKELETON).expect("write route");

    let first = run_gafos(dir.path(), &[]);
    assert_eq!(first.status.code(), Some(0));

    // Revert the route file to its pre-run (stale) state.
    std::fs::write(dir.path().join("httproute.yaml"), ROUTE_SKELETON).expect("revert route");

    let checked = run_gafos(dir.path(), &["--check"]);

    assert_eq!(checked.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&checked.stdout);
    assert!(!stdout.trim().is_empty(), "expected a diff on stdout");
    assert!(stdout.contains("rules:"), "stdout: {stdout}");
}

#[test]
fn empty_spec_is_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "");
    std::fs::write(dir.path().join("openapi.yaml"), SPEC_EMPTY).expect("write spec");
    std::fs::write(dir.path().join("httproute.yaml"), ROUTE_SKELETON).expect("write route");

    let output = run_gafos(dir.path(), &[]);

    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn cli_flag_overrides_config_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "match: { methods: false }\n");
    std::fs::write(dir.path().join("openapi.yaml"), SPEC_HEALTH).expect("write spec");
    std::fs::write(dir.path().join("httproute.yaml"), ROUTE_SKELETON).expect("write route");

    let output = run_gafos(dir.path(), &["--match-methods"]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&output.stderr));

    let written = std::fs::read_to_string(dir.path().join("httproute.yaml")).expect("read route");
    assert!(written.contains("method: GET"), "written manifest: {written}");
}

#[test]
fn idempotent_second_run_no_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_config(dir.path(), "");
    std::fs::write(dir.path().join("openapi.yaml"), SPEC_HEALTH).expect("write spec");
    std::fs::write(dir.path().join("httproute.yaml"), ROUTE_SKELETON).expect("write route");

    let first = run_gafos(dir.path(), &[]);
    assert_eq!(first.status.code(), Some(0));
    let after_first = std::fs::read_to_string(dir.path().join("httproute.yaml")).expect("read route");

    let second = run_gafos(dir.path(), &[]);
    assert_eq!(second.status.code(), Some(0));
    let after_second = std::fs::read_to_string(dir.path().join("httproute.yaml")).expect("read route");

    assert_eq!(after_first, after_second);

    let checked = run_gafos(dir.path(), &["--check"]);
    assert_eq!(checked.status.code(), Some(0));
}
