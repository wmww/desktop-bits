//! The approved command's environment is constructed, not inherited.
//!
//! sudo cannot hand us a clean environment — env_reset has a hardcoded survivor set
//! (TERM/PATH/HOME/MAIL/SHELL/LOGNAME/USER/SUDO_*) that no sudoers setting removes, and HOME and
//! TERM in it are the *caller's* values. So the gate ignores its inherited environment as a source
//! and builds the command's environment from exactly three things: a root-controlled base, a single
//! validated passthrough (TERM), and the request's assignments.
//!
//! The prompt lists the assignments and not TERM. That split is deliberate: an assignment is
//! something the caller *said*, and is the thing a reader needs to weigh; TERM is ambient shell
//! state that arrives on every single request, and listing it trained the eye to skip the field
//! that also carries `LD_PRELOAD=`. The cost of not showing it is that [`term_ok`] is now the only
//! thing between that value and root, so it is written as an allowlist of the shape a terminal
//! name actually has rather than as a charset — see the note there.

use std::ffi::CString;

use crate::cli::Request;
use crate::envsetup::Captured;

/// The command's PATH, always. Never the inherited one: sudo's env_reset preserves the *caller's*
/// PATH unless secure_path is set, and the two arrive identically, so a gate that read the
/// inherited value could not tell a root-controlled PATH from a caller-chosen one. A caller who
/// wants a different PATH asks for it with a `PATH=` assignment, which the prompt shows.
const COMMAND_PATH: &[u8] = b"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The command's locale, always. The caller's LANG/LANGUAGE/LC_* are not forwarded: they steer
/// number formatting, collation, rpmatch() and gettext output inside the root command, which is a
/// silent way to change what a root script does. A caller who genuinely wants a locale asks with a
/// `LANG=` assignment, which the prompt shows.
const COMMAND_LANG: &[u8] = b"C.UTF-8";

/// Longest TERM accepted. Real terminal names run to about thirty bytes.
const MAX_TERM_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub uid: u32,
    pub gid: u32,
    pub user: Vec<u8>,
    /// The requester's home directory from passwd, for abbreviating their cwd. Never used for the
    /// command's HOME, which is root's.
    pub home: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEnv {
    /// The final environment, in application order.
    pub vars: Vec<(Vec<u8>, Vec<u8>)>,
    /// What the request asked for, in request order. This is what the prompt's env field lists,
    /// and the only caller data in the environment that the caller had to be explicit about.
    pub assigned: Vec<(Vec<u8>, Vec<u8>)>,
    /// Inherited variables that survived validation. Ambient rather than requested, so deliberately
    /// not displayed. Kept as its own list rather than left implicit in `vars` so that "what
    /// arrived without the caller asking" stays a thing this module states and the tests can pin.
    pub inherited: Vec<(Vec<u8>, Vec<u8>)>,
    /// Passthrough variables dropped for failing validation. Noted in the prompt: a malformed TERM
    /// is rare enough to be worth a reader's attention, unlike an ordinary one.
    pub dropped: Vec<Vec<u8>>,
}

impl CommandEnv {
    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.vars.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.as_slice())
    }

    pub fn to_envp(&self) -> Vec<CString> {
        self.vars
            .iter()
            .filter_map(|(n, v)| {
                let mut buf = n.clone();
                buf.push(b'=');
                buf.extend_from_slice(v);
                CString::new(buf).ok()
            })
            .collect()
    }
}

/// Validation for one passthrough variable. A failing value is dropped, never sanitized.
///
/// Per-variable rather than one shared charset: with only TERM forwarded there is one shape to
/// describe, and describing it exactly is cheaper than arguing about which punctuation a
/// permissive charset can afford.
pub fn passthrough_ok(name: &[u8], value: &[u8]) -> bool {
    match name {
        b"TERM" => term_ok(value),
        // Nothing else is a passthrough candidate; if one is ever added it needs its own arm
        // rather than inheriting TERM's shape.
        _ => false,
    }
}

/// A terminfo entry name: leading letter, then letters, digits and `. _ + -`.
///
/// The exclusions carry the weight. No `/` and no leading `.`, because TERM is used downstream as
/// a path component under `/usr/share/terminfo` and `$HOME/.terminfo`, and a name that can hold a
/// separator or a `..` is a traversal waiting for a search root that isn't root-owned. Current
/// ncurses rejects those itself — verified, it does not even stat — but that is ncurses' invariant
/// to keep, not ours, and this value is no longer displayed for a human to catch.
///
/// Empty is rejected too: an empty TERM is not a terminal name, and leaving the variable unset
/// says the same thing without asking every downstream parser what it thinks "" means.
fn term_ok(value: &[u8]) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TERM_LEN
        && value[0].is_ascii_alphabetic()
        && value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

pub fn build(
    cap: &Captured,
    req: &Request,
    prov: &Provenance,
    root_shell: &[u8],
) -> CommandEnv {
    // 1. Root-controlled base. None of it is caller data.
    let mut vars: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (b"PATH".to_vec(), COMMAND_PATH.to_vec()),
        (b"HOME".to_vec(), b"/root".to_vec()),
        (b"USER".to_vec(), b"root".to_vec()),
        (b"LOGNAME".to_vec(), b"root".to_vec()),
        (b"SHELL".to_vec(), root_shell.to_vec()),
        (b"LANG".to_vec(), COMMAND_LANG.to_vec()),
    ];

    // 2. The passthrough list: the only variables copied out of the inherited environment.
    let mut inherited = Vec::new();
    let mut dropped = Vec::new();
    for (name, value) in &cap.passthrough {
        if passthrough_ok(name, value) {
            set(&mut vars, name, value);
            inherited.push((name.clone(), value.clone()));
        } else {
            dropped.push(name.clone());
        }
    }

    // 3. The request's assignments, applied last so they override both. Deliberately unfiltered:
    // there is no env_check/env_delete equivalent and LD_PRELOAD is not special-cased. The
    // prompt's env field is the mitigation, which is why it lists these and nothing else.
    let mut assigned = Vec::new();
    for a in &req.assignments {
        set(&mut vars, &a.name, &a.value);
        assigned.push((a.name.clone(), a.value.clone()));
    }

    // The gate sets provenance itself, after the assignments, so it cannot be rewritten by the
    // request. Requests naming SUDO_* are rejected outright at parse time.
    set(&mut vars, b"SUDO_UID", prov.uid.to_string().as_bytes());
    set(&mut vars, b"SUDO_GID", prov.gid.to_string().as_bytes());
    set(&mut vars, b"SUDO_USER", &prov.user);
    set(&mut vars, b"SUDO_COMMAND", &req.sudo_command());

    CommandEnv { vars, assigned, inherited, dropped }
}

fn set(vars: &mut Vec<(Vec<u8>, Vec<u8>)>, name: &[u8], value: &[u8]) {
    match vars.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = value.to_vec(),
        None => vars.push((name.to_vec(), value.to_vec())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Assignment;

    fn cap() -> Captured {
        Captured {
            sudo_uid: Some(b"1006".to_vec()),
            sudo_gid: Some(b"1007".to_vec()),
            sudo_user: Some(b"ai".to_vec()),
            // What `envsetup::capture` can actually produce: TERM and nothing else.
            passthrough: vec![(b"TERM".to_vec(), b"xterm-256color".to_vec())],
        }
    }

    fn prov() -> Provenance {
        Provenance { uid: 1006, gid: 1007, user: b"ai".to_vec(), home: Some(b"/home/ai".to_vec()) }
    }

    fn req(assignments: &[(&str, &str)], command: &str, args: &[&str]) -> Request {
        Request {
            assignments: assignments
                .iter()
                .map(|(n, v)| Assignment { name: n.as_bytes().to_vec(), value: v.as_bytes().to_vec() })
                .collect(),
            command: command.as_bytes().to_vec(),
            args: args.iter().map(|a| a.as_bytes().to_vec()).collect(),
        }
    }

    #[test]
    fn base_values_are_root_controlled() {
        let e = build(&cap(), &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"PATH").unwrap(), COMMAND_PATH);
        assert_eq!(e.get(b"HOME").unwrap(), b"/root");
        assert_eq!(e.get(b"USER").unwrap(), b"root");
        assert_eq!(e.get(b"LOGNAME").unwrap(), b"root");
        assert_eq!(e.get(b"SHELL").unwrap(), b"/bin/bash");
        assert_eq!(e.get(b"LANG").unwrap(), COMMAND_LANG);
    }

    /// The caller's locale never reaches the command: it silently changes number formatting,
    /// collation and gettext output inside whatever root runs.
    #[test]
    fn locale_is_root_controlled_not_inherited() {
        let mut c = cap();
        // Even if capture somehow offered them, they are not passthrough candidates.
        c.passthrough.push((b"LANG".to_vec(), b"en_US.UTF-8".to_vec()));
        c.passthrough.push((b"LC_ALL".to_vec(), b"tr_TR.UTF-8".to_vec()));
        c.passthrough.push((b"LANGUAGE".to_vec(), b"de".to_vec()));
        let e = build(&c, &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"LANG").unwrap(), COMMAND_LANG);
        assert_eq!(e.get(b"LC_ALL"), None);
        assert_eq!(e.get(b"LANGUAGE"), None);
        assert!(e.inherited.iter().all(|(n, _)| n == b"TERM"));
    }

    /// The one way a caller can change it, and the prompt shows the assignment.
    #[test]
    fn a_lang_assignment_overrides_the_default() {
        let e = build(&cap(), &req(&[("LANG", "en_GB.UTF-8")], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"LANG").unwrap(), b"en_GB.UTF-8");
        assert!(e.assigned.contains(&(b"LANG".to_vec(), b"en_GB.UTF-8".to_vec())));
    }

    #[test]
    fn nothing_else_from_the_inherited_environment_is_present() {
        let mut c = cap();
        // Things sudo's env_reset would have kept, which must not appear.
        c.passthrough.push((b"LC_ALL".to_vec(), b"C".to_vec()));
        let e = build(&c, &req(&[], "id", &[]), &prov(), b"/bin/bash");
        let names: Vec<&[u8]> = e.vars.iter().map(|(n, _)| n.as_slice()).collect();
        for absent in [
            &b"DISPLAY"[..],
            b"XAUTHORITY",
            b"LS_COLORS",
            b"MAIL",
            b"WAYLAND_SOCKET",
            b"WAYLAND_DISPLAY",
            b"XDG_RUNTIME_DIR",
        ] {
            assert!(!names.contains(&absent), "{} leaked", String::from_utf8_lossy(absent));
        }
        assert_eq!(
            names,
            vec![
                &b"PATH"[..],
                b"HOME",
                b"USER",
                b"LOGNAME",
                b"SHELL",
                b"LANG",
                b"TERM",
                b"SUDO_UID",
                b"SUDO_GID",
                b"SUDO_USER",
                b"SUDO_COMMAND",
            ]
        );
    }

    /// Inherited and requested are separate lists: the prompt shows one and not the other.
    #[test]
    fn inherited_values_are_not_listed_as_assignments() {
        let e = build(&cap(), &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.inherited, vec![(b"TERM".to_vec(), b"xterm-256color".to_vec())]);
        assert!(e.assigned.is_empty());
        assert!(e.dropped.is_empty());
    }

    #[test]
    fn failing_passthrough_is_dropped_not_sanitized() {
        let mut c = Captured { passthrough: vec![], ..cap() };
        c.passthrough.push((b"TERM".to_vec(), b"xterm\x1b[31m".to_vec()));
        let e = build(&c, &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"TERM"), None);
        assert_eq!(e.dropped, vec![b"TERM".to_vec()]);
        assert!(e.inherited.is_empty());
    }

    /// A name that is not a passthrough candidate is dropped even if capture hands one over, so a
    /// future edit to `envsetup::PASSTHROUGH` cannot silently forward an unvalidated value.
    #[test]
    fn unknown_passthrough_names_have_no_default_shape() {
        let mut c = cap();
        c.passthrough.push((b"TERMINFO".to_vec(), b"/tmp/evil".to_vec()));
        let e = build(&c, &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"TERMINFO"), None);
        assert_eq!(e.dropped, vec![b"TERMINFO".to_vec()]);
    }

    #[test]
    fn assignments_override_base_and_passthrough() {
        let e = build(
            &cap(),
            &req(&[("PATH", "/opt/bin"), ("TERM", "dumb"), ("HOME", "/tmp")], "id", &[]),
            &prov(),
            b"/bin/bash",
        );
        assert_eq!(e.get(b"PATH").unwrap(), b"/opt/bin");
        assert_eq!(e.get(b"TERM").unwrap(), b"dumb");
        assert_eq!(e.get(b"HOME").unwrap(), b"/tmp");
        // All three assignments are displayed; the inherited TERM they overrode is not.
        assert_eq!(e.assigned.len(), 3);
    }

    #[test]
    fn loader_variables_are_carried_but_displayed() {
        let e = build(&cap(), &req(&[("LD_PRELOAD", "/tmp/evil.so")], "id", &[]), &prov(), b"/bin/sh");
        assert_eq!(e.get(b"LD_PRELOAD").unwrap(), b"/tmp/evil.so");
        assert!(e.assigned.contains(&(b"LD_PRELOAD".to_vec(), b"/tmp/evil.so".to_vec())));
    }

    /// The whole point of hiding the inherited values: the one line worth reading is the only
    /// line, instead of the fourth of four.
    #[test]
    fn a_dangerous_assignment_is_the_only_thing_displayed() {
        let e = build(&cap(), &req(&[("LD_PRELOAD", "/tmp/evil.so")], "id", &[]), &prov(), b"/bin/sh");
        assert_eq!(e.assigned, vec![(b"LD_PRELOAD".to_vec(), b"/tmp/evil.so".to_vec())]);
    }

    #[test]
    fn provenance_is_applied_last_and_is_unspoofable() {
        // A SUDO_* assignment cannot reach here (the parser rejects it), but if it ever did the
        // gate's own values must still win.
        let mut r = req(&[], "/usr/bin/ls", &["-l"]);
        r.assignments.push(Assignment { name: b"SUDO_UID".to_vec(), value: b"0".to_vec() });
        let e = build(&cap(), &r, &prov(), b"/bin/bash");
        assert_eq!(e.get(b"SUDO_UID").unwrap(), b"1006");
        assert_eq!(e.get(b"SUDO_GID").unwrap(), b"1007");
        assert_eq!(e.get(b"SUDO_USER").unwrap(), b"ai");
        assert_eq!(e.get(b"SUDO_COMMAND").unwrap(), b"/usr/bin/ls -l");
    }

    #[test]
    fn envp_round_trip() {
        let e = build(&cap(), &req(&[("A", "b")], "id", &[]), &prov(), b"/bin/bash");
        let envp = e.to_envp();
        assert!(envp.iter().any(|c| c.as_bytes() == b"A=b"));
        assert!(envp.iter().any(|c| c.as_bytes() == b"HOME=/root"));
    }

    /// The inherited PATH is never a source, so nothing sudo does to it can steer resolution.
    #[test]
    fn path_is_always_root_controlled() {
        let e = build(&cap(), &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"PATH").unwrap(), COMMAND_PATH);
    }

    /// The one way a caller can change it, and the prompt shows the assignment.
    #[test]
    fn a_path_assignment_overrides_the_default() {
        let e = build(&cap(), &req(&[("PATH", "/opt/bin")], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"PATH").unwrap(), &b"/opt/bin"[..]);
    }

    #[test]
    fn real_terminal_names_are_accepted() {
        for ok in [
            &b"xterm"[..],
            b"xterm-256color",
            b"screen.xterm-256color",
            b"rxvt-unicode-256color",
            b"tmux-256color",
            b"vt100",
            b"Eterm",
            b"foot",
            b"linux",
            b"dumb",
        ] {
            assert!(term_ok(ok), "{} rejected", String::from_utf8_lossy(ok));
        }
        assert!(term_ok(&vec![b'x'; MAX_TERM_LEN]));
    }

    /// TERM indexes into `/usr/share/terminfo/<c>/<name>` and `$HOME/.terminfo`. ncurses rejects
    /// separators and `..` itself, but that is its invariant to keep and this value is no longer
    /// displayed for a human to catch, so the gate does not forward the shape at all.
    #[test]
    fn term_cannot_hold_a_path() {
        assert!(!term_ok(b"../../../tmp/evil"));
        assert!(!term_ok(b"x/../../tmp/evil"));
        assert!(!term_ok(b"/tmp/evil"));
        assert!(!term_ok(b".."));
        assert!(!term_ok(b"./x"));
        assert!(!term_ok(b"a/b"));
    }

    #[test]
    fn term_rejects_shell_and_control_bytes() {
        assert!(!term_ok(b"xterm 256color"));
        assert!(!term_ok(b"xterm\n256color"));
        assert!(!term_ok(b"xterm\x1b[31m"));
        assert!(!term_ok(b"xterm$(id)"));
        assert!(!term_ok(b"xterm;id"));
        assert!(!term_ok(b"xterm\0"));
        assert!(!term_ok(b"xterm=1"));
        assert!(!term_ok(&[0xff, 0xfe]));
    }

    #[test]
    fn term_rejects_the_empty_and_the_overlong() {
        assert!(!term_ok(b""));
        assert!(!term_ok(&vec![b'x'; MAX_TERM_LEN + 1]));
        // Leading non-letter: a terminal name starts with one, and it keeps `-` and `.` out of the
        // position where they mean something to a path or an option parser.
        assert!(!term_ok(b"-xterm"));
        assert!(!term_ok(b"256color"));
        assert!(!term_ok(b"_xterm"));
    }

    #[test]
    fn passthrough_dispatches_on_the_name() {
        assert!(passthrough_ok(b"TERM", b"xterm-256color"));
        assert!(!passthrough_ok(b"TERM", b"../evil"));
        // Not a candidate: no shape is defined for it, so it never passes.
        assert!(!passthrough_ok(b"LANG", b"en_US.UTF-8"));
        assert!(!passthrough_ok(b"TERMINFO", b"xterm"));
        assert!(!passthrough_ok(b"LD_PRELOAD", b"libc.so"));
    }
}
