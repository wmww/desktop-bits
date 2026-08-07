//! Interpreter requests: when COMMAND's basename is `sudo`, this request runs a second sudo as
//! root, so the gate — not the shim — decides what that inner sudo may be asked to do.
//!
//! This is a whitelist, not a blacklist, because sudo accepts unambiguous long-option
//! abbreviations: a scan looking for `-e`/`--edit` misses `--ed`, `--edi` and `--e`, all of which
//! mean `--edit` and land on an editor running as root. The whitelist denies those and every
//! future option nobody has thought about, at the cost of denying spellings the shim never
//! produces anyway.
//!
//! It bounds what the shim's generated requests can be and keeps a directly-invoked request
//! readable. It does not and cannot stop an approved root command from reaching an editor by some
//! other route — root editing files is not a privilege escalation.

use crate::cli::{basename, valid_name, Request};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Interpreter {
    /// `-u`/`-g` in any accepted spelling.
    pub as_other_user: bool,
    /// `-i` or `-s`.
    pub shell: bool,
    /// A token follows the option region.
    pub inner_command: bool,
}

/// Is this request an interpreter request at all?
pub fn is_interpreter(req: &Request) -> bool {
    basename(&req.command) == b"sudo"
}

/// Scan the tokens after COMMAND. Anything unrecognised is a denial, without prompting.
pub fn scan(args: &[Vec<u8>]) -> Result<Interpreter, String> {
    let mut out = Interpreter::default();
    let mut i = 0;
    while i < args.len() {
        let tok = args[i].as_slice();
        match tok {
            b"-i" | b"-s" => out.shell = true,
            b"-u" | b"-g" | b"--user" | b"--group" => {
                out.as_other_user = true;
                // Consumes the following token as its argument, whatever it looks like.
                if i + 1 >= args.len() {
                    return Err(format!(
                        "inner sudo option {} has no argument",
                        String::from_utf8_lossy(tok)
                    ));
                }
                i += 1;
            }
            b"--" => {
                // End of the option region. Nothing after it is scanned.
                out.inner_command = i + 1 < args.len();
                return Ok(out);
            }
            _ if tok.starts_with(b"--user=") || tok.starts_with(b"--group=") => {
                out.as_other_user = true;
            }
            _ if is_assignment(tok) => {}
            _ if tok.starts_with(b"-") => {
                return Err(format!(
                    "inner sudo option {} is not permitted (only -i, -s, -u, -g, --user, \
                     --group, NAME=value and -- are)",
                    String::from_utf8_lossy(tok)
                ));
            }
            _ => {
                out.inner_command = true;
                return Ok(out);
            }
        }
        i += 1;
    }
    Ok(out)
}

fn is_assignment(tok: &[u8]) -> bool {
    match tok.iter().position(|b| *b == b'=') {
        Some(eq) => valid_name(&tok[..eq]),
        None => false,
    }
}

/// Fixed compiled-in prose for the trusted warning field, picked from the scan's own result and
/// never built out of the request text.
pub fn prose(i: &Interpreter) -> Vec<&'static str> {
    let mut out = vec!["Runs a second sudo as root."];
    if i.shell {
        out.push("Gives an interactive root shell: nothing it then runs will prompt again.");
        if i.as_other_user {
            out.push("The shell runs as another user or group, and still never prompts again.");
        }
        if i.inner_command {
            out.push("The arguments below are shell code, not a command.");
        }
    } else if i.as_other_user {
        out.push("Runs the command below as another user or group.");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(items: &[&str]) -> Vec<Vec<u8>> {
        items.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    fn ok(items: &[&str]) -> Interpreter {
        scan(&toks(items)).expect("should be accepted")
    }

    fn denied(items: &[&str]) -> String {
        scan(&toks(items)).expect_err("should be denied")
    }

    #[test]
    fn detects_interpreter_requests_by_basename() {
        let mk = |c: &str| Request {
            assignments: vec![],
            command: c.as_bytes().to_vec(),
            args: vec![],
        };
        assert!(is_interpreter(&mk("/usr/bin/sudo")));
        assert!(is_interpreter(&mk("sudo")));
        assert!(!is_interpreter(&mk("/usr/bin/sudoku")));
        assert!(!is_interpreter(&mk("/usr/bin/ls")));
    }

    #[test]
    fn accepted_spellings() {
        assert_eq!(ok(&["-i"]), Interpreter { shell: true, ..Default::default() });
        assert_eq!(ok(&["-s"]), Interpreter { shell: true, ..Default::default() });
        for spelling in [
            &["-u", "ff", "id"][..],
            &["--user=ff", "id"][..],
            &["--user", "ff", "id"][..],
            &["-g", "video", "id"][..],
            &["--group=video", "id"][..],
            &["--group", "video", "id"][..],
        ] {
            assert_eq!(
                ok(spelling),
                Interpreter { as_other_user: true, shell: false, inner_command: true },
                "{spelling:?}"
            );
        }
    }

    #[test]
    fn assignments_are_accepted_in_the_option_region() {
        assert_eq!(
            ok(&["FOO=bar", "-u", "ff", "BAZ=qux", "id"]),
            Interpreter { as_other_user: true, shell: false, inner_command: true }
        );
    }

    #[test]
    fn double_dash_ends_the_scan() {
        assert_eq!(
            ok(&["-u", "ff", "--", "-e", "/etc/passwd"]),
            Interpreter { as_other_user: true, shell: false, inner_command: true }
        );
        assert_eq!(ok(&["--"]), Interpreter::default());
    }

    #[test]
    fn shell_with_trailing_tokens() {
        assert_eq!(
            ok(&["-i", "rm", "-rf", "/"]),
            Interpreter { as_other_user: false, shell: true, inner_command: true }
        );
    }

    #[test]
    fn edit_flags_and_their_abbreviations_are_denied() {
        for flag in ["-e", "--edit", "--ed", "--edi", "--e", "-eu", "-se", "-E"] {
            assert!(denied(&[flag]).contains("not permitted"), "{flag} should be denied");
        }
    }

    #[test]
    fn preserve_env_in_every_spelling_is_denied() {
        // The shim expands --preserve-env into assignments and never forwards the flag, so a
        // request carrying it did not come from the shim.
        for flag in ["--preserve-env", "--preserve-env=DISPLAY", "--preserve", "--pres", "--p"] {
            assert!(denied(&[flag]).contains("not permitted"), "{flag} should be denied");
        }
    }

    #[test]
    fn other_unrecognised_flags_are_denied() {
        for flag in ["-n", "-b", "--chdir=/tmp", "-uff", "--us=ff", "-l", "-V", "--help", "-"] {
            assert!(denied(&[flag]).contains("not permitted"), "{flag} should be denied");
        }
    }

    #[test]
    fn missing_option_argument_is_denied() {
        assert!(denied(&["-u"]).contains("no argument"));
        assert!(denied(&["--group"]).contains("no argument"));
    }

    #[test]
    fn option_arguments_are_consumed_even_when_flag_shaped() {
        // `-u -e` asks for a user named "-e"; the token is an argument, not an option.
        assert_eq!(
            ok(&["-u", "-e"]),
            Interpreter { as_other_user: true, shell: false, inner_command: false }
        );
    }

    #[test]
    fn prose_is_picked_from_the_scan() {
        let shell_only = prose(&Interpreter { shell: true, ..Default::default() });
        assert_eq!(shell_only.len(), 2);
        assert!(shell_only[1].contains("interactive root shell"));

        let shell_args =
            prose(&Interpreter { shell: true, inner_command: true, ..Default::default() });
        assert!(shell_args.iter().any(|l| l.contains("shell code, not a command")));

        let as_user =
            prose(&Interpreter { as_other_user: true, inner_command: true, ..Default::default() });
        assert!(as_user.iter().any(|l| l.contains("as another user")));
        assert!(!as_user.iter().any(|l| l.contains("interactive root shell")));

        let both = prose(&Interpreter { as_other_user: true, shell: true, inner_command: false });
        assert!(both.iter().any(|l| l.contains("interactive root shell")));
        assert!(both.iter().any(|l| l.contains("runs as another user")));
    }
}
