//! Drive the *real* gate parser with the shim's own output, so the two lists cannot drift apart.
//!
//! The shim's whitelist and the gate's interpreter whitelist are separate code with overlapping
//! rules. Every request the shim can generate must be one the gate accepts.

use std::ffi::OsString;

use sudo_prompt::{cli, interp};
use sudo_shim::classify::{classify, Decision, GATE};

fn shim(items: &[&str], vars: &[(&str, &str)]) -> Decision {
    let argv: Vec<OsString> = items.iter().map(OsString::from).collect();
    let vars: Vec<(Vec<u8>, Vec<u8>)> =
        vars.iter().map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec())).collect();
    classify(1006, &argv, &move |name: &[u8]| {
        vars.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    })
}

/// One invocation: the argv the caller typed, and the environment the shim would look names up in.
type Case = (&'static [&'static str], &'static [(&'static str, &'static str)]);

/// Everything the shim can send to the gate, one entry per interesting shape.
const GATE_CASES: &[Case] = &[
    (&["id"], &[]),
    (&["/usr/bin/ls", "-l", "/tmp"], &[]),
    (&["FOO=bar", "id"], &[]),
    (&["A=1", "B=2", "A=3", "id"], &[]),
    (&["./a=b"], &[]),
    (&["--", "-x"], &[]),
    (&["FOO=b", "--", "id"], &[]),
    (&["-i"], &[]),
    (&["-s"], &[]),
    (&["-i", "rm", "-rf", "/"], &[]),
    (&["-u", "ff", "id"], &[]),
    (&["--user=ff", "id"], &[]),
    (&["--user", "ff", "id"], &[]),
    (&["-g", "video", "id"], &[]),
    (&["--group=video", "id"], &[]),
    (&["--group", "video", "id"], &[]),
    (&["-u", "ff", "-i"], &[]),
    (&["FOO=bar", "-u", "ff", "id"], &[]),
    (&["--preserve-env=DISPLAY", "id"], &[("DISPLAY", ":0")]),
    (&["--preserve-env=NOPE", "id"], &[]),
    (&["--preserve-env=DISPLAY", "-u", "ff", "id"], &[("DISPLAY", ":0")]),
    (&["--preserve-env=NOPE", "-u", "ff", "id"], &[]),
    (&["-u", "ff", "--preserve-env=A", "id"], &[("A", "1")]),
    (&["-u", "ff", "--", "-e", "/etc/passwd"], &[]),
];

#[test]
fn every_generated_request_is_one_the_gate_accepts() {
    for (items, vars) in GATE_CASES {
        let Decision::Gate(argv) = shim(items, vars) else {
            panic!("{items:?} should reach the gate");
        };
        assert_eq!(argv[0], OsString::from(GATE), "{items:?}");

        // The gate sees everything after its own path.
        let req = cli::parse(&argv[1..])
            .unwrap_or_else(|e| panic!("gate rejected the shim's output for {items:?}: {e}"));

        if interp::is_interpreter(&req) {
            interp::scan(&req.args).unwrap_or_else(|e| {
                panic!("gate's interpreter whitelist rejected {items:?}: {e}")
            });
        }
    }
}

#[test]
fn the_gate_never_sees_preserve_env_in_any_spelling() {
    for (items, vars) in GATE_CASES {
        if let Decision::Gate(argv) = shim(items, vars) {
            for token in &argv {
                assert!(
                    !token.to_string_lossy().contains("preserve"),
                    "{items:?} leaked {token:?} to the gate"
                );
            }
        }
    }
}

#[test]
fn the_interpreter_path_carries_no_gate_assignments() {
    // Every assignment becomes a command-line variable for the inner sudo instead, which is the
    // only place it can be and still survive that sudo's env_reset.
    for (items, vars) in GATE_CASES {
        let Decision::Gate(argv) = shim(items, vars) else { continue };
        let req = cli::parse(&argv[1..]).unwrap();
        if interp::is_interpreter(&req) {
            assert!(req.assignments.is_empty(), "{items:?} carried gate assignments");
        }
    }
}

#[test]
fn the_sudoers_pattern_shape_holds_for_every_request() {
    // `%sudo-prompt-users ALL=(root) NOPASSWD: /usr/local/bin/sudo-prompt -- *` needs the literal
    // `--` followed by at least one argument, joined with spaces.
    for (items, vars) in GATE_CASES {
        let Decision::Gate(argv) = shim(items, vars) else { continue };
        assert_eq!(argv[1], OsString::from("--"), "{items:?}");
        assert!(argv.len() >= 3, "{items:?} would not match the sudoers pattern");
    }
}
