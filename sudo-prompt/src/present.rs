//! Turning a request into the fixed presentation. The gate constructs this in code; there is no
//! reusable options parser it could be steered with.

use permission_prompt_ui::dialog::{DialogSpec, Field, Style};
use permission_prompt_ui::{Escaped, Untrusted};

use crate::cli::Request;
use crate::cmdenv::{CommandEnv, Provenance};
use crate::interp::Interpreter;

const HEADING: &str = "Run a command as root?";
const APPROVE: &str = "Run as root";
const DENY: &str = "Cancel";
const FOOTER: &str = "Enter runs it as root · Escape cancels";
const BASE_ENV_NOTE: &str =
    "Plus root's own PATH, HOME, USER, LOGNAME, SHELL and SUDO_* — not caller data.";

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

    // The uid field is the only thing separating "I typed this" from "the agent typed this", so it
    // is trusted, prominent, and never adjacent to caller-controlled text.
    fields.push(Field::trusted(
        "Requested by",
        vec![Escaped::concat([
            Escaped::literal("uid "),
            Escaped::number(prov.uid as u64),
            Escaped::literal(" ("),
            Escaped::of(&Untrusted::from_bytes(prov.user.clone())),
            Escaped::literal("), gid "),
            Escaped::number(prov.gid as u64),
        ])],
    ));

    if let Some(i) = interp {
        fields.push(Field::trusted(
            "Warning",
            crate::interp::prose(i).into_iter().map(Escaped::literal).collect(),
        ));
    }

    // Display argv exactly as requested: one shell-quoted token per line, no resolution. A
    // resolved path would promise an inode identity the gate cannot hold across the approval
    // window.
    let mut tokens = vec![Escaped::shell_token(&Untrusted::from_bytes(req.command.clone()))];
    for arg in &req.args {
        tokens.push(Escaped::shell_token(&Untrusted::from_bytes(arg.clone())));
    }
    let command_log =
        tokens.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(" ");
    fields.push(Field::untrusted("Command", tokens).expanding());

    let mut env_field = Field::untrusted(
        "Command environment",
        env.caller
            .iter()
            .map(|(n, v)| {
                Escaped::assignment(
                    &Untrusted::from_bytes(n.clone()),
                    &Untrusted::from_bytes(v.clone()),
                )
            })
            .collect(),
    )
    .with_note(Escaped::literal(BASE_ENV_NOTE));
    if !env.dropped.is_empty() {
        let mut parts = vec![Escaped::literal("Dropped for failing validation: ")];
        for (n, name) in env.dropped.iter().enumerate() {
            if n > 0 {
                parts.push(Escaped::literal(", "));
            }
            parts.push(Escaped::of(&Untrusted::from_bytes(name.clone())));
        }
        env_field = env_field.with_note(Escaped::concat(parts));
    }
    fields.push(env_field);

    fields.push(Field::untrusted(
        "Working directory",
        vec![match cwd {
            Some(c) => Escaped::shell_token(&Untrusted::from_bytes(c.to_vec())),
            None => Escaped::literal("(unavailable)"),
        }],
    ));

    Rendered {
        spec: DialogSpec {
            style: Style::Gate,
            heading: HEADING,
            icon: Some("dialog-warning-symbolic".to_string()),
            fields,
            approve: APPROVE,
            deny: DENY,
            footer: FOOTER,
        },
        command_log,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli;
    use permission_prompt_ui::dialog::FieldKind;
    use std::ffi::OsString;

    fn render(argv: &[&str]) -> Rendered {
        let req = cli::parse(&argv.iter().map(OsString::from).collect::<Vec<_>>()).unwrap();
        let prov = Provenance { uid: 1006, gid: 1007, user: b"ai".to_vec() };
        let cap = crate::envsetup::Captured {
            secure_path: Some(b"/usr/bin".to_vec()),
            ..crate::envsetup::Captured::default()
        };
        let env = crate::cmdenv::build(&cap, &req, &prov, b"/bin/bash");
        let interp = crate::interp::is_interpreter(&req)
            .then(|| crate::interp::scan(&req.args).unwrap());
        build(&req, interp.as_ref(), &prov, Some(b"/home/ai"), &env)
    }

    fn field<'a>(r: &'a Rendered, label: &str) -> &'a Field {
        r.spec.fields.iter().find(|f| f.label == label).expect("field present")
    }

    #[test]
    fn trusted_fields_are_not_scrolling_and_caller_fields_are() {
        let r = render(&["--", "/usr/bin/ls", "-l"]);
        assert_eq!(field(&r, "Requested by").kind, FieldKind::Trusted);
        assert_eq!(field(&r, "Command").kind, FieldKind::Untrusted);
        assert_eq!(field(&r, "Command environment").kind, FieldKind::Untrusted);
        assert_eq!(field(&r, "Working directory").kind, FieldKind::Untrusted);
    }

    #[test]
    fn requesting_uid_is_rendered_from_provenance() {
        let r = render(&["--", "id"]);
        let f = field(&r, "Requested by");
        assert_eq!(f.lines[0].as_str(), "uid 1006 (ai), gid 1007");
    }

    #[test]
    fn one_shell_quoted_token_per_line() {
        let r = render(&["--", "/usr/bin/rm", "-rf", "/tmp/a b", "x'y"]);
        let f = field(&r, "Command");
        assert_eq!(
            f.lines.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
            vec!["/usr/bin/rm", "-rf", "'/tmp/a b'", "'x'\\''y'"]
        );
        assert_eq!(r.command_log, "/usr/bin/rm -rf '/tmp/a b' 'x'\\''y'");
    }

    #[test]
    fn markup_and_mnemonics_stay_literal() {
        let r = render(&["--", "/bin/echo", "<b>Run a command as root?</b>", "_Approve"]);
        let f = field(&r, "Command");
        assert!(f.lines[1].as_str().contains("<b>Run a command as root?</b>"));
        assert!(f.lines[2].as_str().contains("_Approve"));
    }

    #[test]
    fn control_bytes_in_argv_are_escaped_and_flagged() {
        let r = render(&["--", "/bin/echo", "a\u{1b}[31mred"]);
        let f = field(&r, "Command");
        assert!(f.lines[1].as_str().contains("\\x1b"));
        assert!(f.lines[1].was_escaped());
    }

    #[test]
    fn assignments_show_in_the_environment_field_not_the_command_field() {
        let r = render(&["--", "LD_PRELOAD=/tmp/evil.so", "/bin/id"]);
        let cmd = field(&r, "Command");
        assert_eq!(cmd.lines.len(), 1);
        let env = field(&r, "Command environment");
        assert!(env.lines.iter().any(|l| l.as_str() == "LD_PRELOAD=/tmp/evil.so"));
        assert!(env.notes[0].as_str().contains("not caller data"));
    }

    #[test]
    fn interpreter_requests_get_a_trusted_warning() {
        let r = render(&["--", "/usr/bin/sudo", "-u", "ff", "id"]);
        let w = field(&r, "Warning");
        assert_eq!(w.kind, FieldKind::Trusted);
        assert!(w.lines.iter().any(|l| l.as_str().contains("second sudo")));
        assert!(w.lines.iter().any(|l| l.as_str().contains("as another user")));
        // The path stays in the command field with the rest of the argv.
        assert_eq!(field(&r, "Command").lines[0].as_str(), "/usr/bin/sudo");
    }

    #[test]
    fn plain_requests_get_no_warning_field() {
        let r = render(&["--", "/usr/bin/ls"]);
        assert!(r.spec.fields.iter().all(|f| f.label != "Warning"));
    }

    #[test]
    fn missing_cwd_renders_as_trusted_prose() {
        let req = cli::parse(&[OsString::from("--"), OsString::from("id")]).unwrap();
        let prov = Provenance { uid: 1, gid: 1, user: b"x".to_vec() };
        let env = crate::cmdenv::build(&crate::envsetup::Captured::default(), &req, &prov, b"/bin/sh");
        let r = build(&req, None, &prov, None, &env);
        assert_eq!(field(&r, "Working directory").lines[0].as_str(), "(unavailable)");
    }
}
