//! The gate's complete production interface:
//!
//! ```text
//! sudo-prompt -- [NAME=value ...] COMMAND [ARGS...]
//! ```
//!
//! There are no other options. A sudoers-approved binary with prompt options would let a requester
//! disable the settle delay, pick a weaker surface, or narrate a privileged action as something
//! harmless.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Environment assignments for the command, deduplicated by name with the last winning.
    pub assignments: Vec<Assignment>,
    pub command: Vec<u8>,
    pub args: Vec<Vec<u8>>,
}

impl Request {
    /// The command line as stock sudo records it in SUDO_COMMAND: space joined, no assignments,
    /// raw bytes rather than the escaped display form.
    pub fn sudo_command(&self) -> Vec<u8> {
        let mut out = self.command.clone();
        for arg in &self.args {
            out.push(b' ');
            out.extend_from_slice(arg);
        }
        out
    }
}

pub fn basename(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|b| *b == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// True for a token sudo would read as an environment assignment: a valid NAME followed by `=`.
/// This is sudo's own rule, so `./a=b` is a command and not a malformed assignment.
fn split_assignment(token: &[u8]) -> Option<(&[u8], &[u8])> {
    let eq = token.iter().position(|b| *b == b'=')?;
    let (name, rest) = token.split_at(eq);
    if !valid_name(name) {
        return None;
    }
    Some((name, &rest[1..]))
}

pub fn valid_name(name: &[u8]) -> bool {
    match name.first() {
        Some(b'A'..=b'Z') | Some(b'a'..=b'z') | Some(b'_') => {}
        _ => return false,
    }
    name.iter().all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

/// Parse the gate's argv, excluding argv[0].
pub fn parse(argv: &[OsString]) -> Result<Request, String> {
    let argv: Vec<&[u8]> = argv.iter().map(|a| a.as_bytes()).collect();

    if argv.is_empty() {
        return Err("empty argv: expected `-- [NAME=value ...] COMMAND [ARGS...]`".to_string());
    }
    if argv[0] != b"--" {
        if argv.iter().any(|t| *t == b"--") {
            return Err("unexpected argument before the `--` delimiter".to_string());
        }
        return Err("missing the `--` delimiter".to_string());
    }

    let rest = &argv[1..];
    if rest.is_empty() {
        return Err("nothing after the `--` delimiter".to_string());
    }

    let mut assignments: Vec<Assignment> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let Some((name, value)) = split_assignment(rest[i]) else { break };
        if name.starts_with(b"SUDO_") {
            // Provenance is the gate's to set, and an assignment that silently would not take
            // effect must not be displayed as though it would.
            return Err(format!(
                "request assigns {}: SUDO_* is set by the gate and cannot be requested",
                String::from_utf8_lossy(name)
            ));
        }
        // Last occurrence wins, and takes the position of the last occurrence.
        assignments.retain(|a| a.name != name);
        assignments.push(Assignment { name: name.to_vec(), value: value.to_vec() });
        i += 1;
    }

    let Some(command) = rest.get(i) else {
        return Err("request contains only environment assignments, no command".to_string());
    };
    if command.is_empty() {
        return Err("empty COMMAND".to_string());
    }
    if basename(command) == b"sudoedit" {
        // sudoedit has no supported path; edit files as root instead.
        return Err("sudoedit is not a supported request".to_string());
    }

    Ok(Request {
        assignments,
        command: command.to_vec(),
        args: rest[i + 1..].iter().map(|t| t.to_vec()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn ok(items: &[&str]) -> Request {
        parse(&argv(items)).expect("should parse")
    }

    fn err(items: &[&str]) -> String {
        parse(&argv(items)).expect_err("should be rejected")
    }

    #[test]
    fn plain_command() {
        let r = ok(&["--", "/usr/bin/ls", "-l", "/tmp"]);
        assert!(r.assignments.is_empty());
        assert_eq!(r.command, b"/usr/bin/ls");
        assert_eq!(r.args, vec![b"-l".to_vec(), b"/tmp".to_vec()]);
    }

    #[test]
    fn assignments_then_command() {
        let r = ok(&["--", "FOO=bar", "EMPTY=", "id", "-u"]);
        assert_eq!(r.assignments.len(), 2);
        assert_eq!(r.assignments[0], Assignment { name: b"FOO".to_vec(), value: b"bar".to_vec() });
        assert_eq!(r.assignments[1], Assignment { name: b"EMPTY".to_vec(), value: b"".to_vec() });
        assert_eq!(r.command, b"id");
    }

    #[test]
    fn assignments_deduplicate_last_wins() {
        let r = ok(&["--", "A=1", "B=2", "A=3", "id"]);
        assert_eq!(r.assignments.len(), 2);
        assert_eq!(r.assignments[0].name, b"B");
        assert_eq!(r.assignments[1], Assignment { name: b"A".to_vec(), value: b"3".to_vec() });
    }

    #[test]
    fn nothing_after_the_command_is_reinterpreted() {
        let r = ok(&["--", "env", "FOO=bar", "--", "-x"]);
        assert!(r.assignments.is_empty());
        assert_eq!(r.command, b"env");
        assert_eq!(r.args.len(), 3);
    }

    #[test]
    fn dot_slash_a_equals_b_is_a_command() {
        let r = ok(&["--", "./a=b"]);
        assert!(r.assignments.is_empty());
        assert_eq!(r.command, b"./a=b");
    }

    #[test]
    fn invalid_assignment_name_is_a_command() {
        let r = ok(&["--", "9A=b"]);
        assert_eq!(r.command, b"9A=b");
    }

    #[test]
    fn rejections() {
        assert!(err(&[]).contains("empty argv"));
        assert!(err(&["id"]).contains("missing the `--`"));
        assert!(err(&["id", "--", "x"]).contains("before the `--`"));
        assert!(err(&["--"]).contains("nothing after"));
        assert!(err(&["--", "A=1"]).contains("only environment assignments"));
        assert!(err(&["--", ""]).contains("empty COMMAND"));
        assert!(err(&["--", "SUDO_UID=0", "id"]).contains("SUDO_*"));
        assert!(err(&["--", "SUDO_USER=root", "id"]).contains("SUDO_*"));
        assert!(err(&["--", "/usr/bin/sudoedit", "f"]).contains("sudoedit"));
        assert!(err(&["--", "sudoedit", "f"]).contains("sudoedit"));
    }

    #[test]
    fn non_utf8_argv_survives_byte_for_byte() {
        use std::os::unix::ffi::OsStringExt;
        let a = vec![
            OsString::from("--"),
            OsString::from_vec(b"/tmp/\xff\xfe".to_vec()),
            OsString::from_vec(b"\x80".to_vec()),
        ];
        let r = parse(&a).unwrap();
        assert_eq!(r.command, b"/tmp/\xff\xfe");
        assert_eq!(r.args, vec![b"\x80".to_vec()]);
    }

    #[test]
    fn sudo_command_string() {
        let r = ok(&["--", "FOO=bar", "/usr/bin/ls", "-l", "a b"]);
        assert_eq!(r.sudo_command(), b"/usr/bin/ls -l a b");
    }
}
