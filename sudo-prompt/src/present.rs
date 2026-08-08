//! Turning a request into the fixed presentation. The gate constructs this in code; there is no
//! reusable options parser it could be steered with.

use permission_prompt_ui::dialog::{DialogSpec, Field, Style};
use permission_prompt_ui::{Escaped, Untrusted};

use crate::cli::Request;
use crate::cmdenv::{CommandEnv, Provenance};
use crate::interp::Interpreter;

const TITLE: &str = "Run a command as root?";
const APPROVE: &str = "Run as root";
const DENY: &str = "Cancel";

pub struct Rendered {
    pub spec: DialogSpec,
    /// The escaped command line for the log record.
    pub command_log: String,
}

pub fn build(
    req: &Request,
    interp: Option<&Interpreter>,
    prov: &Provenance,
    cwd: Option<&[u8]>,
    env: &CommandEnv,
) -> Rendered {
    let mut fields = Vec::new();

    // Display argv exactly as requested: one shell-quoted token per line, no resolution. A
    // resolved path would promise an inode identity the gate cannot hold across the approval
    // window.
    let mut tokens = vec![Escaped::shell_token(&Untrusted::from_bytes(req.command.clone()))];
    for arg in &req.args {
        tokens.push(Escaped::shell_token(&Untrusted::from_bytes(arg.clone())));
    }
    let command_log =
        tokens.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(" ");
    // First and loudest, with no heading above it: the command *is* the question, and a sentence
    // asking it in the gate's own words would only push the answer further down.
    fields.push(Field::untrusted("", tokens).expanding().prominent());

    // Directly under the command, because it changes what the command means.
    if let Some(i) = interp {
        fields.push(Field::warning(
            crate::interp::prose(i).into_iter().map(Escaped::literal).collect(),
        ));
    }

    // Who asked and where from, on one line: both answer "is this the shell I am looking at?", and
    // uid/gid are noise next to the name — the log keeps the numbers. Flat rather than boxed: one
    // short line of context does not need a viewport around it.
    fields.push(
        Field::untrusted(
            "",
            vec![Escaped::concat([
                Escaped::of(&Untrusted::from_bytes(prov.user.clone())),
                Escaped::literal(" | "),
                match cwd {
                    Some(c) => {
                        Escaped::path(&Untrusted::from_bytes(abbreviate(c, prov.home.as_deref())))
                    }
                    None => Escaped::literal("(cwd unavailable)"),
                },
            ])],
        )
        .flat(),
    );

    // Only what the request asked for. The gate constructs the rest itself, and the one inherited
    // variable (TERM) is ambient shell state present on every request — listing it put three lines
    // of noise above the line that might say `LD_PRELOAD=`, which is the line this field exists
    // for. It is shape-validated instead, see `cmdenv::term_ok`.
    let mut env_field = Field::untrusted(
        "env",
        env.assigned
            .iter()
            .map(|(n, v)| {
                Escaped::assignment(
                    &Untrusted::from_bytes(n.clone()),
                    &Untrusted::from_bytes(v.clone()),
                )
            })
            .collect(),
    );
    if !env.dropped.is_empty() {
        let mut parts = vec![Escaped::literal("dropped as invalid: ")];
        for (n, name) in env.dropped.iter().enumerate() {
            if n > 0 {
                parts.push(Escaped::literal(", "));
            }
            parts.push(Escaped::of(&Untrusted::from_bytes(name.clone())));
        }
        env_field = env_field.with_note(Escaped::concat(parts));
    }
    // Omitted entirely when there is nothing to say, rather than shown as a label against blank
    // space: a request that sets no variables is the common one, and an absent row says so as
    // clearly as an empty one. The field appearing at all now means something.
    if !env_field.lines.is_empty() || !env_field.notes.is_empty() {
        fields.push(env_field);
    }

    Rendered {
        spec: DialogSpec {
            style: Style::Gate,
            title: TITLE,
            heading: None,
            subtitle: Vec::new(),
            fields,
            approve: APPROVE,
            deny: DENY,
        },
        command_log,
    }
}

/// `~` for the *requesting* user's home directory — never root's, which is the gate's own HOME and
/// has nothing to do with where the request came from.
fn abbreviate(cwd: &[u8], home: Option<&[u8]>) -> Vec<u8> {
    let Some(home) = home.filter(|h| !h.is_empty() && *h != b"/") else {
        return cwd.to_vec();
    };
    if cwd == home {
        return b"~".to_vec();
    }
    match cwd.strip_prefix(home) {
        Some(rest) if rest.starts_with(b"/") => {
            let mut out = b"~".to_vec();
            out.extend_from_slice(rest);
            out
        }
        _ => cwd.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli;
    use permission_prompt_ui::dialog::FieldKind;
    use std::ffi::OsString;

    fn prov() -> Provenance {
        Provenance {
            uid: 1006,
            gid: 1007,
            user: b"ai".to_vec(),
            home: Some(b"/home/ai".to_vec()),
        }
    }

    fn render(argv: &[&str]) -> Rendered {
        let req = cli::parse(&argv.iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
        let prov = prov();
        // A realistic capture: TERM is present on every request a real caller makes.
        let cap = crate::envsetup::Captured {
            passthrough: vec![(b"TERM".to_vec(), b"xterm-256color".to_vec())],
            ..Default::default()
        };
        let env = crate::cmdenv::build(&cap, &req, &prov, b"/bin/bash");
        let interp = crate::interp::is_interpreter(&req)
            .then(|| crate::interp::scan(&req.args).unwrap());
        build(&req, interp.as_ref(), &prov, Some(b"/home/ai/desktop-bits"), &env)
    }

    fn field<'a>(r: &'a Rendered, label: &str) -> &'a Field {
        r.spec.fields.iter().find(|f| f.label == label).expect("field present")
    }

    /// The command: the one prominent field, and the one that expands.
    fn command(r: &Rendered) -> &Field {
        r.spec.fields.iter().find(|f| f.prominent).expect("command field present")
    }

    /// The unlabelled line naming who asked and from where.
    fn origin(r: &Rendered) -> &Field {
        r.spec
            .fields
            .iter()
            .find(|f| f.kind == FieldKind::Untrusted && f.label.is_empty() && !f.prominent)
            .expect("origin field present")
    }

    #[test]
    fn every_caller_controlled_field_scrolls() {
        let r = render(&["--", "/usr/bin/ls", "-l"]);
        assert!(r.spec.fields.iter().all(|f| f.kind == FieldKind::Untrusted));
        assert_eq!(command(&r).label, "");
    }

    /// Nothing above the command: no icon, no heading, no subtitle.
    #[test]
    fn the_command_is_the_first_thing_on_screen() {
        let r = render(&["--", "/usr/bin/ls", "-l"]);
        assert_eq!(r.spec.heading, None);
        assert!(r.spec.subtitle.is_empty());
        assert!(std::ptr::eq(&r.spec.fields[0], command(&r)));
        assert!(command(&r).expand);
    }

    #[test]
    fn the_requester_and_their_cwd_share_one_line() {
        let r = render(&["--", "id"]);
        assert_eq!(origin(&r).lines.len(), 1);
        assert_eq!(origin(&r).lines[0].as_str(), "ai | ~/desktop-bits");
    }

    #[test]
    fn the_cwd_is_abbreviated_against_the_requesters_home_not_roots() {
        let home = Some(&b"/home/ai"[..]);
        assert_eq!(abbreviate(b"/home/ai", home), b"~");
        assert_eq!(abbreviate(b"/home/ai/src", home), b"~/src");
        assert_eq!(abbreviate(b"/root", home), b"/root");
        // A prefix that is not a path component boundary is not the home directory.
        assert_eq!(abbreviate(b"/home/aid", home), b"/home/aid");
        // No usable home: shown as it is, never abbreviated against `/`.
        assert_eq!(abbreviate(b"/etc", None), b"/etc");
        assert_eq!(abbreviate(b"/etc", Some(b"/")), b"/etc");
        assert_eq!(abbreviate(b"/etc", Some(b"")), b"/etc");
    }

    #[test]
    fn an_odd_cwd_is_quoted_but_a_tilde_alone_is_not() {
        let quoted = |cwd: &[u8]| {
            let req = cli::parse(&[OsString::from("--"), OsString::from("id")]).unwrap();
            let env = crate::cmdenv::build(
                &crate::envsetup::Captured::default(),
                &req,
                &prov(),
                b"/bin/sh",
            );
            let r = build(&req, None, &prov(), Some(cwd), &env);
            origin(&r).lines[0].as_str().to_string()
        };
        assert_eq!(quoted(b"/home/ai"), "ai | ~");
        assert_eq!(quoted(b"/home/ai/a b"), "ai | '~/a b'");
        assert_eq!(quoted(b"/tmp/x\ny"), "ai | '/tmp/x\\x0ay'");
    }

    #[test]
    fn one_shell_quoted_token_per_line() {
        let r = render(&["--", "/usr/bin/rm", "-rf", "/tmp/a b", "x'y"]);
        let f = command(&r);
        assert_eq!(
            f.lines.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
            vec!["/usr/bin/rm", "-rf", "'/tmp/a b'", "'x'\\''y'"]
        );
        assert_eq!(r.command_log, "/usr/bin/rm -rf '/tmp/a b' 'x'\\''y'");
    }

    #[test]
    fn markup_and_mnemonics_stay_literal() {
        let r = render(&["--", "/bin/echo", "<b>Run a command as root?</b>", "_Approve"]);
        let f = command(&r);
        assert!(f.lines[1].as_str().contains("<b>Run a command as root?</b>"));
        assert!(f.lines[2].as_str().contains("_Approve"));
    }

    #[test]
    fn control_bytes_in_argv_are_escaped_and_flagged() {
        let r = render(&["--", "/bin/echo", "a\u{1b}[31mred"]);
        let f = command(&r);
        assert!(f.lines[1].as_str().contains("\\x1b"));
        assert!(f.lines[1].was_escaped());
    }

    #[test]
    fn assignments_show_in_the_environment_field_not_the_command_field() {
        let r = render(&["--", "LD_PRELOAD=/tmp/evil.so", "/bin/id"]);
        assert_eq!(command(&r).lines.len(), 1);
        let env = field(&r, "env");
        assert!(env.lines.iter().any(|l| l.as_str() == "LD_PRELOAD=/tmp/evil.so"));
    }

    /// The env field is for what the caller *asked* for. Inherited shell state arrives on every
    /// request, so listing it taught the eye to skip the field that also carries `LD_PRELOAD=`.
    #[test]
    fn inherited_shell_state_is_not_listed() {
        // Nothing was asked for, so there is no env row at all — not an empty one.
        let r = render(&["--", "/bin/id"]);
        assert!(r.spec.fields.iter().all(|f| f.label != "env"));

        let r = render(&["--", "LD_PRELOAD=/tmp/evil.so", "/bin/id"]);
        let env = field(&r, "env");
        assert_eq!(
            env.lines.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
            vec!["LD_PRELOAD=/tmp/evil.so"]
        );
        assert!(env.notes.is_empty());
    }

    /// An ordinary TERM is silent, but a malformed one is exactly the anomaly worth surfacing.
    #[test]
    fn a_dropped_passthrough_is_still_noted() {
        let req = cli::parse(&[OsString::from("--"), OsString::from("id")]).unwrap();
        let prov = Provenance { uid: 1, gid: 1, user: b"x".to_vec(), home: None };
        let cap = crate::envsetup::Captured {
            passthrough: vec![(b"TERM".to_vec(), b"../../tmp/evil".to_vec())],
            ..Default::default()
        };
        let env = crate::cmdenv::build(&cap, &req, &prov, b"/bin/sh");
        let r = build(&req, None, &prov, Some(b"/home/x"), &env);
        let f = field(&r, "env");
        assert!(f.lines.is_empty());
        assert_eq!(f.notes.len(), 1);
        assert!(f.notes[0].as_str().contains("dropped as invalid: TERM"));
    }

    #[test]
    fn interpreter_requests_get_a_trusted_warning_under_the_command() {
        let r = render(&["--", "/usr/bin/sudo", "-u", "ff", "id"]);
        let w = &r.spec.fields[1];
        assert_eq!(w.kind, FieldKind::Warning);
        assert!(w.lines.iter().any(|l| l.as_str().contains("second sudo")));
        assert!(w.lines.iter().any(|l| l.as_str().contains("as another user")));
        // The path stays in the command field with the rest of the argv.
        assert_eq!(command(&r).lines[0].as_str(), "/usr/bin/sudo");
    }

    #[test]
    fn plain_requests_get_no_warning_field() {
        let r = render(&["--", "/usr/bin/ls"]);
        assert!(r.spec.fields.iter().all(|f| f.kind != FieldKind::Warning));
    }

    #[test]
    fn missing_cwd_is_named_rather_than_left_blank() {
        let req = cli::parse(&[OsString::from("--"), OsString::from("id")]).unwrap();
        let prov = Provenance { uid: 1, gid: 1, user: b"x".to_vec(), home: None };
        let env = crate::cmdenv::build(&crate::envsetup::Captured::default(), &req, &prov, b"/bin/sh");
        let r = build(&req, None, &prov, None, &env);
        assert_eq!(origin(&r).lines[0].as_str(), "x | (cwd unavailable)");
    }
}
