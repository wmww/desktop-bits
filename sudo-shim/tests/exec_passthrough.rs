//! With the exec target pointed at a fake sudo: the constructed argv byte-for-byte, and exit
//! status and signals passing through untouched.
//!
//! Requires the `test-exec-override` feature:
//! `cargo test -p sudo-shim --features test-exec-override`.
#![cfg(feature = "test-exec-override")]

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A fake sudo that records its argv NUL-separated and then does what the test asked.
fn fake_sudo(dir: &Path, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-sudo");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             : >\"$RECORD\"\n\
             for a in \"$@\"; do printf '%s\\0' \"$a\" >>\"$RECORD\"; done\n\
             {body}\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Run {
    argv: Vec<String>,
    status: std::process::ExitStatus,
}

fn run(args: &[&str], body: &str, env: &[(&str, &str)]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let sudo = fake_sudo(dir.path(), body);
    let record = dir.path().join("record");
    let status = Command::new(env!("CARGO_BIN_EXE_sudo-shim"))
        .args(args)
        .env("SUDO_SHIM_REAL_SUDO", &sudo)
        .env("RECORD", &record)
        .envs(env.iter().copied())
        .status()
        .unwrap();
    let recorded = std::fs::read(&record).unwrap_or_default();
    let argv = recorded
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    Run { argv, status }
}

#[test]
fn a_plain_command_reaches_the_gate_with_the_expected_argv() {
    let r = run(&["/usr/bin/ls", "-l", "/tmp"], "exit 0", &[]);
    assert_eq!(
        r.argv,
        vec!["/usr/local/bin/sudo-prompt", "--", "/usr/bin/ls", "-l", "/tmp"]
    );
}

#[test]
fn an_interpreter_request_routes_through_real_sudo() {
    let r = run(&["-u", "ff", "id"], "exit 0", &[]);
    assert_eq!(
        r.argv,
        vec!["/usr/local/bin/sudo-prompt", "--", "/usr/bin/sudo", "-u", "ff", "id"]
    );
}

#[test]
fn preserve_env_is_expanded_from_the_shims_own_environment() {
    let r = run(&["--preserve-env=PP_TEST_VAR", "id"], "exit 0", &[("PP_TEST_VAR", "value 1")]);
    assert_eq!(
        r.argv,
        vec!["/usr/local/bin/sudo-prompt", "--", "PP_TEST_VAR=value 1", "id"]
    );
}

#[test]
fn an_informational_flag_passes_the_raw_argv_through() {
    let r = run(&["-l"], "exit 0", &[]);
    assert_eq!(r.argv, vec!["-l"]);
}

#[test]
fn exit_status_passes_through() {
    assert_eq!(run(&["id"], "exit 42", &[]).status.code(), Some(42));
    assert_eq!(run(&["id"], "exit 125", &[]).status.code(), Some(125));
}

#[test]
fn signals_pass_through() {
    let r = run(&["id"], "kill -TERM $$", &[]);
    assert_eq!(r.status.signal(), Some(libc_sigterm()));
}

fn libc_sigterm() -> i32 {
    15
}
