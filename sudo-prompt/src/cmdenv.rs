//! The approved command's environment is constructed, not inherited.
//!
//! sudo cannot hand us a clean environment — env_reset has a hardcoded survivor set
//! (TERM/PATH/HOME/MAIL/SHELL/LOGNAME/USER/SUDO_*) that no sudoers setting removes, and HOME and
//! TERM in it are the *caller's* values. So the gate ignores its inherited environment as a source
//! and builds the command's environment from exactly three things: a root-controlled base, a short
//! validated passthrough list, and the request's assignments.
//!
//! That makes the prompt's Environment field a *complete* account of the caller-controlled data
//! entering the root command rather than an almost-complete one.

use std::ffi::CString;

use crate::cli::Request;
use crate::envsetup::Captured;

/// Fallback if sudo handed us no PATH at all. secure_path being set is a verification item.
const FALLBACK_PATH: &[u8] = b"/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// A passthrough value longer than this is dropped rather than sanitized.
const MAX_PASSTHROUGH_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub uid: u32,
    pub gid: u32,
    pub user: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEnv {
    /// The final environment, in application order.
    pub vars: Vec<(Vec<u8>, Vec<u8>)>,
    /// The caller-controlled subset, in display order: surviving passthrough variables followed by
    /// the request's assignments. This is what the prompt's Environment field lists.
    pub caller: Vec<(Vec<u8>, Vec<u8>)>,
    /// Passthrough variables dropped for failing validation. Noted in the prompt.
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

/// Conservative charset and length. A failing value is dropped, never sanitized.
pub fn passthrough_ok(value: &[u8]) -> bool {
    value.len() <= MAX_PASSTHROUGH_LEN
        && value.iter().all(|b| {
            matches!(b,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
                | b'_' | b'.' | b':' | b'+' | b',' | b'=' | b'@' | b'/' | b'-')
        })
}

pub fn build(
    cap: &Captured,
    req: &Request,
    prov: &Provenance,
    root_shell: &[u8],
) -> CommandEnv {
    let path = cap.secure_path.clone().unwrap_or_else(|| FALLBACK_PATH.to_vec());

    // 1. Root-controlled base. None of it is caller data.
    let mut vars: Vec<(Vec<u8>, Vec<u8>)> = vec![
        (b"PATH".to_vec(), path),
        (b"HOME".to_vec(), b"/root".to_vec()),
        (b"USER".to_vec(), b"root".to_vec()),
        (b"LOGNAME".to_vec(), b"root".to_vec()),
        (b"SHELL".to_vec(), root_shell.to_vec()),
    ];

    // 2. The passthrough list: the only variables copied out of the inherited environment.
    let mut caller = Vec::new();
    let mut dropped = Vec::new();
    for (name, value) in &cap.passthrough {
        if passthrough_ok(value) {
            set(&mut vars, name, value);
            caller.push((name.clone(), value.clone()));
        } else {
            dropped.push(name.clone());
        }
    }

    // 3. The request's assignments, applied last so they override both. Deliberately unfiltered:
    // there is no env_check/env_delete equivalent and LD_PRELOAD is not special-cased. The
    // prompt's Environment field is the mitigation.
    for a in &req.assignments {
        set(&mut vars, &a.name, &a.value);
        caller.push((a.name.clone(), a.value.clone()));
    }

    // The gate sets provenance itself, after the assignments, so it cannot be rewritten by the
    // request. Requests naming SUDO_* are rejected outright at parse time.
    set(&mut vars, b"SUDO_UID", prov.uid.to_string().as_bytes());
    set(&mut vars, b"SUDO_GID", prov.gid.to_string().as_bytes());
    set(&mut vars, b"SUDO_USER", &prov.user);
    set(&mut vars, b"SUDO_COMMAND", &req.sudo_command());

    CommandEnv { vars, caller, dropped }
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
            secure_path: Some(b"/usr/bin:/bin".to_vec()),
            sudo_uid: Some(b"1006".to_vec()),
            sudo_gid: Some(b"1007".to_vec()),
            sudo_user: Some(b"ai".to_vec()),
            passthrough: vec![
                (b"TERM".to_vec(), b"xterm-256color".to_vec()),
                (b"LANG".to_vec(), b"en_US.UTF-8".to_vec()),
            ],
        }
    }

    fn prov() -> Provenance {
        Provenance { uid: 1006, gid: 1007, user: b"ai".to_vec() }
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
        assert_eq!(e.get(b"PATH").unwrap(), b"/usr/bin:/bin");
        assert_eq!(e.get(b"HOME").unwrap(), b"/root");
        assert_eq!(e.get(b"USER").unwrap(), b"root");
        assert_eq!(e.get(b"LOGNAME").unwrap(), b"root");
        assert_eq!(e.get(b"SHELL").unwrap(), b"/bin/bash");
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
                b"TERM",
                b"LANG",
                b"LC_ALL",
                b"SUDO_UID",
                b"SUDO_GID",
                b"SUDO_USER",
                b"SUDO_COMMAND",
            ]
        );
    }

    #[test]
    fn passthrough_survivors_are_listed_as_caller_data() {
        let e = build(&cap(), &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(
            e.caller,
            vec![
                (b"TERM".to_vec(), b"xterm-256color".to_vec()),
                (b"LANG".to_vec(), b"en_US.UTF-8".to_vec()),
            ]
        );
        assert!(e.dropped.is_empty());
    }

    #[test]
    fn failing_passthrough_is_dropped_not_sanitized() {
        let mut c = cap();
        c.passthrough.push((b"LC_ALL".to_vec(), b"C; rm -rf /".to_vec()));
        c.passthrough.push((b"LC_TIME".to_vec(), vec![b'x'; 1000]));
        c.passthrough.push((b"COLORTERM".to_vec(), b"tru\x1becolor".to_vec()));
        let e = build(&c, &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"LC_ALL"), None);
        assert_eq!(e.get(b"LC_TIME"), None);
        assert_eq!(e.get(b"COLORTERM"), None);
        assert_eq!(
            e.dropped,
            vec![b"LC_ALL".to_vec(), b"LC_TIME".to_vec(), b"COLORTERM".to_vec()]
        );
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
        // Two passthrough survivors plus all three assignments are displayed as caller data.
        assert_eq!(e.caller.len(), 5);
    }

    #[test]
    fn loader_variables_are_carried_but_displayed() {
        let e = build(&cap(), &req(&[("LD_PRELOAD", "/tmp/evil.so")], "id", &[]), &prov(), b"/bin/sh");
        assert_eq!(e.get(b"LD_PRELOAD").unwrap(), b"/tmp/evil.so");
        assert!(e.caller.contains(&(b"LD_PRELOAD".to_vec(), b"/tmp/evil.so".to_vec())));
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

    #[test]
    fn missing_secure_path_falls_back() {
        let mut c = cap();
        c.secure_path = None;
        let e = build(&c, &req(&[], "id", &[]), &prov(), b"/bin/bash");
        assert_eq!(e.get(b"PATH").unwrap(), FALLBACK_PATH);
    }

    #[test]
    fn passthrough_validation() {
        assert!(passthrough_ok(b"xterm-256color"));
        assert!(passthrough_ok(b"en_US.UTF-8"));
        assert!(passthrough_ok(b""));
        assert!(!passthrough_ok(b"a b"));
        assert!(!passthrough_ok(b"a\nb"));
        assert!(!passthrough_ok(b"a\x1bb"));
        assert!(!passthrough_ok(b"a$b"));
        assert!(!passthrough_ok(&vec![b'x'; MAX_PASSTHROUGH_LEN + 1]));
        assert!(passthrough_ok(&vec![b'x'; MAX_PASSTHROUGH_LEN]));
    }
}
