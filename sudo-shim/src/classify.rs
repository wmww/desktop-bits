//! Classification: a pure function over argv and the shim's own environment.
//!
//! The shim is not a security boundary — it runs as the caller, and anything it gets wrong the
//! caller could have done by invoking /usr/bin/sudo directly. It is Rust for engineering reasons
//! only: byte-exact argv handling, environment lookup, one exec, and a classification that unit
//! tests can drive directly.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

pub const REAL_SUDO: &str = "/usr/bin/sudo";
pub const GATE: &str = "/usr/local/bin/sudo-prompt";

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// `exec /usr/bin/sudo "$@"` with the raw argv. Informational flags work normally; anything
    /// carrying a command is denied because no sudoers rule permits it.
    PassThrough,
    /// `exec /usr/bin/sudo <these>`, which start with the gate path and `--`.
    Gate(Vec<OsString>),
}

fn os(bytes: &[u8]) -> OsString {
    OsString::from_vec(bytes.to_vec())
}

fn valid_name(name: &[u8]) -> bool {
    match name.first() {
        Some(b'A'..=b'Z') | Some(b'a'..=b'z') | Some(b'_') => {}
        _ => return false,
    }
    name.iter().all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn split_assignment(token: &[u8]) -> Option<(&[u8], &[u8])> {
    let eq = token.iter().position(|b| *b == b'=')?;
    let (name, rest) = token.split_at(eq);
    if !valid_name(name) {
        return None;
    }
    Some((name, &rest[1..]))
}

/// Classify one invocation. `env` looks a name up in the shim's own environment.
pub fn classify(
    euid: u32,
    argv: &[OsString],
    env: &dyn Fn(&[u8]) -> Option<Vec<u8>>,
) -> Decision {
    // Inside a root shell euid is 0, the shim passes straight through, and root's own sudoers rule
    // needs no approval. An empty argv is real sudo's usage error to print.
    if euid == 0 || argv.is_empty() {
        return Decision::PassThrough;
    }

    let bytes: Vec<&[u8]> = argv.iter().map(|a| a.as_bytes()).collect();

    let mut assignments: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    // Index of the first *collected* token — flag or assignment. Starting RAWARGV here rather
    // than at the first flag matters: `sudo FOO=bar -u ff cmd` would otherwise drop `FOO=bar`.
    let mut first_collected: Option<usize> = None;
    // `--preserve-env=LIST` tokens and what they expand to, by index.
    let mut expansions: Vec<(usize, Vec<Vec<u8>>)> = Vec::new();
    let mut interpreter = false;
    let mut shell_flag = false;
    let mut command_at: Option<usize> = None;

    let mut i = 0;
    while i < bytes.len() {
        let tok = bytes[i];

        if tok == b"--" {
            // End of options: consume it and stop scanning.
            command_at = if i + 1 < bytes.len() { Some(i + 1) } else { None };
            break;
        }

        if let Some((name, value)) = split_assignment(tok) {
            first_collected = first_collected.or(Some(i));
            set_assignment(&mut assignments, name, value);
            i += 1;
            continue;
        }

        if let Some(list) = tok.strip_prefix(b"--preserve-env=") {
            // Expanded, never forwarded, so the gate never sees --preserve-env in any spelling and
            // its interpreter whitelist does not need to know the flag exists.
            first_collected = first_collected.or(Some(i));
            let mut expanded = Vec::new();
            for name in list.split(|b| *b == b',') {
                // Unset and invalid names are silently skipped, matching sudo.
                if !valid_name(name) {
                    continue;
                }
                let Some(value) = env(name) else { continue };
                let mut token = name.to_vec();
                token.push(b'=');
                token.extend_from_slice(&value);
                expanded.push(token);
                set_assignment(&mut assignments, name, &value);
            }
            expansions.push((i, expanded));
            i += 1;
            continue;
        }

        match tok {
            b"-i" | b"-s" => {
                first_collected = first_collected.or(Some(i));
                interpreter = true;
                shell_flag = true;
            }
            b"-u" | b"-g" | b"--user" | b"--group" => {
                first_collected = first_collected.or(Some(i));
                interpreter = true;
                if i + 1 >= bytes.len() {
                    // No argument: real sudo prints its own usage error.
                    break;
                }
                // Consumes the following token as its argument.
                i += 1;
            }
            _ if tok.starts_with(b"--user=") || tok.starts_with(b"--group=") => {
                first_collected = first_collected.or(Some(i));
                interpreter = true;
            }
            // Any other token starting with `-`, including sudo's unambiguous long-option
            // abbreviations (`--us=ff`, `--ed`) and compact forms (`-uff`). Matching is on exact
            // spellings only, because modelling getopt_long's abbreviation rules would mean
            // tracking sudo's whole option table.
            _ if tok.starts_with(b"-") => return Decision::PassThrough,
            _ => {
                command_at = Some(i);
                break;
            }
        }
        i += 1;
    }

    if command_at.is_none() && !shell_flag {
        return Decision::PassThrough;
    }

    let mut out = vec![OsString::from(GATE), OsString::from("--")];

    if interpreter {
        // The request runs through real sudo as the interpreter. Every assignment, whether the
        // caller wrote it or --preserve-env produced it, becomes a command-line variable for that
        // inner sudo — the only place it can be and still survive that sudo's env_reset. So the
        // gate's own assignment list is always empty on this path.
        out.push(OsString::from(REAL_SUDO));
        let start = first_collected.expect("an interpreter flag was collected");
        for (n, token) in bytes.iter().enumerate().skip(start) {
            match expansions.iter().find(|(idx, _)| *idx == n) {
                // Substituted in place, by zero tokens if nothing in LIST was set. No other token
                // is altered and no order is rewritten.
                Some((_, expanded)) => out.extend(expanded.iter().map(|t| os(t))),
                None => out.push(os(token)),
            }
        }
    } else {
        // Applied by the gate itself after approval — there is no env(1) in the chain. Using env
        // would mean the prompt's command field named /usr/bin/env instead of the real command,
        // and would break on a command token containing `=` or starting with `-`.
        for (name, value) in &assignments {
            let mut token = name.clone();
            token.push(b'=');
            token.extend_from_slice(value);
            out.push(os(&token));
        }
        let start = command_at.expect("checked above");
        out.extend(bytes[start..].iter().map(|t| os(t)));
    }

    Decision::Gate(out)
}

fn set_assignment(list: &mut Vec<(Vec<u8>, Vec<u8>)>, name: &[u8], value: &[u8]) {
    // Deduplicated by name, last occurrence winning.
    list.retain(|(n, _)| n != name);
    list.push((name.to_vec(), value.to_vec()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn empty_env(_: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn classify_str(items: &[&str]) -> Decision {
        classify(1006, &argv(items), &empty_env)
    }

    /// The gate argv as a readable vector, for byte-exact assertions.
    fn gate(items: &[&str]) -> Vec<String> {
        match classify_str(items) {
            Decision::Gate(v) => v.iter().map(|s| s.to_string_lossy().into_owned()).collect(),
            Decision::PassThrough => panic!("expected Gate for {items:?}"),
        }
    }

    fn gate_with_env(items: &[&str], vars: &[(&str, &str)]) -> Vec<String> {
        let vars: Vec<(Vec<u8>, Vec<u8>)> = vars
            .iter()
            .map(|(n, v)| (n.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect();
        let lookup = move |name: &[u8]| {
            vars.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
        };
        match classify(1006, &argv(items), &lookup) {
            Decision::Gate(v) => v.iter().map(|s| s.to_string_lossy().into_owned()).collect(),
            Decision::PassThrough => panic!("expected Gate for {items:?}"),
        }
    }

    fn passes(items: &[&str]) {
        assert_eq!(classify_str(items), Decision::PassThrough, "{items:?}");
    }

    #[test]
    fn root_and_empty_argv_pass_through() {
        assert_eq!(classify(0, &argv(&["id"]), &empty_env), Decision::PassThrough);
        assert_eq!(classify(1006, &[], &empty_env), Decision::PassThrough);
    }

    #[test]
    fn informational_flags_pass_through() {
        for flags in [
            &["-l"][..],
            &["-ll"],
            &["-V"],
            &["-v"],
            &["-k"],
            &["-K"],
            &["-h"],
            &["--help"],
            &["--version"],
            &["--list"],
            &["--list=id"],
            &["--validate"],
            &["--reset-timestamp"],
            &["--remove-timestamp"],
            &["-kK"],
            &["-k", "id"],
        ] {
            passes(flags);
        }
    }

    #[test]
    fn unsupported_flags_pass_through() {
        for flags in [
            &["-e", "/etc/passwd"][..],
            &["--edit", "/etc/passwd"],
            &["--ed", "/etc/passwd"],
            &["-E", "id"],
            &["--preserve-env", "id"],
            &["-n", "id"],
            &["-b", "id"],
            &["--chdir=/tmp", "id"],
            &["-uff", "id"],
            &["--us=ff", "id"],
            &["-", "id"],
        ] {
            passes(flags);
        }
    }

    #[test]
    fn plain_command() {
        assert_eq!(gate(&["id"]), vec![GATE, "--", "id"]);
        assert_eq!(gate(&["/usr/bin/ls", "-l", "/tmp"]), vec![GATE, "--", "/usr/bin/ls", "-l", "/tmp"]);
    }

    #[test]
    fn assignments_are_forwarded_as_gate_assignments() {
        assert_eq!(gate(&["FOO=bar", "id"]), vec![GATE, "--", "FOO=bar", "id"]);
        assert_eq!(
            gate(&["A=1", "B=2", "A=3", "id"]),
            vec![GATE, "--", "B=2", "A=3", "id"]
        );
    }

    #[test]
    fn a_command_token_containing_equals_is_not_an_assignment() {
        assert_eq!(gate(&["./a=b"]), vec![GATE, "--", "./a=b"]);
        assert_eq!(gate(&["9A=b"]), vec![GATE, "--", "9A=b"]);
    }

    #[test]
    fn double_dash_ends_the_options() {
        assert_eq!(gate(&["--", "-x"]), vec![GATE, "--", "-x"]);
        assert_eq!(gate(&["FOO=b", "--", "id"]), vec![GATE, "--", "FOO=b", "id"]);
        // Nothing after `--` and no shell flag: real sudo's usage error.
        passes(&["--"]);
    }

    #[test]
    fn no_command_and_no_shell_flag_passes_through() {
        passes(&["FOO=bar"]);
        passes(&["-u", "ff"]);
        passes(&["-u"]);
        passes(&["--group"]);
    }

    #[test]
    fn shell_flags_gate_with_no_command() {
        assert_eq!(gate(&["-i"]), vec![GATE, "--", REAL_SUDO, "-i"]);
        assert_eq!(gate(&["-s"]), vec![GATE, "--", REAL_SUDO, "-s"]);
    }

    #[test]
    fn interpreter_flags_route_through_real_sudo_byte_for_byte() {
        for (input, expected_tail) in [
            (&["-u", "ff", "id"][..], &["-u", "ff", "id"][..]),
            (&["--user=ff", "id"], &["--user=ff", "id"]),
            (&["--user", "ff", "id"], &["--user", "ff", "id"]),
            (&["-g", "video", "id"], &["-g", "video", "id"]),
            (&["--group=video", "id"], &["--group=video", "id"]),
            (&["--group", "video", "id"], &["--group", "video", "id"]),
            (&["-u", "ff", "-i"], &["-u", "ff", "-i"]),
            (&["-i", "rm", "-rf", "/"], &["-i", "rm", "-rf", "/"]),
        ] {
            let mut expected = vec![GATE.to_string(), "--".to_string(), REAL_SUDO.to_string()];
            expected.extend(expected_tail.iter().map(|s| s.to_string()));
            assert_eq!(gate(input), expected, "{input:?}");
        }
    }

    #[test]
    fn rawargv_starts_at_the_first_collected_token_not_the_first_flag() {
        // `FOO=bar` would otherwise be dropped entirely.
        assert_eq!(
            gate(&["FOO=bar", "-u", "ff", "id"]),
            vec![GATE, "--", REAL_SUDO, "FOO=bar", "-u", "ff", "id"]
        );
    }

    #[test]
    fn preserve_env_becomes_gate_assignments_on_the_plain_path() {
        assert_eq!(
            gate_with_env(&["--preserve-env=DISPLAY,XAUTHORITY", "id"], &[
                ("DISPLAY", ":0"),
                ("XAUTHORITY", "/run/x"),
            ]),
            vec![GATE, "--", "DISPLAY=:0", "XAUTHORITY=/run/x", "id"]
        );
    }

    #[test]
    fn preserve_env_skips_unset_and_invalid_names() {
        assert_eq!(
            gate_with_env(&["--preserve-env=SET,UNSET,9BAD,", "id"], &[("SET", "yes")]),
            vec![GATE, "--", "SET=yes", "id"]
        );
    }

    #[test]
    fn preserve_env_expanding_to_nothing_leaves_a_plain_request() {
        assert_eq!(gate_with_env(&["--preserve-env=NOPE", "id"], &[]), vec![GATE, "--", "id"]);
    }

    #[test]
    fn preserve_env_duplicate_names_collapse() {
        assert_eq!(
            gate_with_env(&["--preserve-env=A,A", "id"], &[("A", "1")]),
            vec![GATE, "--", "A=1", "id"]
        );
    }

    #[test]
    fn a_later_assignment_beats_a_preserve_env_value() {
        assert_eq!(
            gate_with_env(&["--preserve-env=A", "A=explicit", "id"], &[("A", "inherited")]),
            vec![GATE, "--", "A=explicit", "id"]
        );
    }

    #[test]
    fn preserve_env_is_substituted_in_place_on_the_interpreter_path() {
        assert_eq!(
            gate_with_env(&["--preserve-env=A,B", "-u", "ff", "id"], &[("A", "1"), ("B", "2")]),
            vec![GATE, "--", REAL_SUDO, "A=1", "B=2", "-u", "ff", "id"]
        );
        // The flag never appears in the gate argv, in any spelling.
        assert!(!gate_with_env(&["--preserve-env=A", "-u", "ff", "id"], &[("A", "1")])
            .iter()
            .any(|t| t.contains("preserve-env")));
    }

    #[test]
    fn preserve_env_expanding_to_zero_names_removes_the_token_entirely() {
        assert_eq!(
            gate_with_env(&["--preserve-env=NOPE", "-u", "ff", "id"], &[]),
            vec![GATE, "--", REAL_SUDO, "-u", "ff", "id"]
        );
    }

    #[test]
    fn preserve_env_after_the_flags_stays_where_it_stood() {
        assert_eq!(
            gate_with_env(&["-u", "ff", "--preserve-env=A", "id"], &[("A", "1")]),
            vec![GATE, "--", REAL_SUDO, "-u", "ff", "A=1", "id"]
        );
    }

    #[test]
    fn non_utf8_argv_is_forwarded_byte_for_byte() {
        let a = vec![
            OsString::from_vec(b"/tmp/\xff\xfe".to_vec()),
            OsString::from_vec(b"\x80arg".to_vec()),
        ];
        match classify(1006, &a, &empty_env) {
            Decision::Gate(v) => {
                assert_eq!(v[2].as_bytes(), b"/tmp/\xff\xfe");
                assert_eq!(v[3].as_bytes(), b"\x80arg");
            }
            Decision::PassThrough => panic!("expected Gate"),
        }
    }

    #[test]
    fn non_utf8_preserve_env_value_survives() {
        let vars = [(b"V".to_vec(), b"\xff\x00x".to_vec())];
        let lookup = move |name: &[u8]| {
            vars.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
        };
        let a = argv(&["--preserve-env=V", "id"]);
        match classify(1006, &a, &lookup) {
            Decision::Gate(v) => assert_eq!(v[2].as_bytes(), b"V=\xff\x00x"),
            Decision::PassThrough => panic!("expected Gate"),
        }
    }
}
