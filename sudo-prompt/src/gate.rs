//! The gate's control flow: one run, one decision, one exec.

use std::ffi::OsString;

use permission_prompt_ui::{PromptConfig, SurfaceMode, Verdict, SETTLE, SETTLE_CAP};

use crate::cmdenv::Provenance;
use crate::envsetup::Captured;
use crate::{cli, cmdenv, config, display, envsetup, exec, interp, journal, lockfile, present, sys};

/// The one sentence a denial prints. Kept exact: callers grep for it.
pub const DENIED_MESSAGE: &str = "User denied sudo :(";

/// Denial, and every operational error, exit 125. On approval the gate exec()s, so the command's
/// own status or signal is the result.
pub const EXIT_FAILURE: i32 = 125;

pub enum Fail {
    /// The human said no.
    Denied,
    /// A named check failed, or the prompt could not be trusted to have been answered.
    Error(String),
}

/// Never returns on approval: the gate exec()s the command.
pub fn run() -> Fail {
    // Read the cwd first, before the environment is touched, and tolerate it being gone: a caller
    // whose cwd vanished should still be able to run sudo.
    let cwd = sys::getcwd_bytes();

    if let Err(e) = config::check_privilege() {
        return Fail::Error(e);
    }

    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    let req = match cli::parse(&argv) {
        Ok(r) => r,
        Err(e) => return Fail::Error(e),
    };
    // When COMMAND is a sudo, the gate decides what that inner sudo may be asked to do.
    let interpreter = if interp::is_interpreter(&req) {
        match interp::scan(&req.args) {
            Ok(i) => Some(i),
            Err(e) => return Fail::Error(e),
        }
    } else {
        None
    };

    // Order matters: flock, then display selection, then lock(), then windows.
    let _flock = match lockfile::acquire(&config::lock_path(), config::owner_uid()) {
        Ok(l) => l,
        Err(e) => return Fail::Error(e),
    };

    // Resolved before the environment is cleared. In the release binary this is a constant; the
    // test-seams build reads it from the environment, which scrub() is about to wipe.
    let display_root = config::display_root();

    let captured = envsetup::capture();
    let provenance = match provenance(&captured) {
        Ok(p) => p,
        Err(e) => return Fail::Error(e),
    };
    // Everything the caller's environment could steer GTK with goes now, before GTK init.
    envsetup::scrub();

    let selected = match display::select(&display_root, config::owner_uid()) {
        Ok(s) => s,
        Err(e) => return Fail::Error(e),
    };
    if let Err(e) = display::clear_cloexec(selected.fd) {
        return Fail::Error(e);
    }
    envsetup::set_display_env(&display_root.to_string_lossy(), selected.fd);

    if let Err(e) = permission_prompt_ui::init() {
        return Fail::Error(e);
    }
    permission_prompt_ui::install_panic_hook();

    let env = cmdenv::build(&captured, &req, &provenance, &sys::root_shell());
    let rendered = present::build(&req, interpreter.as_ref(), &provenance, cwd.as_deref(), &env);

    // The prompt unlocks the session and waits for the compositor to process it before returning,
    // on every path including the settle cap and the signal handlers.
    let verdict = permission_prompt_ui::run(PromptConfig {
        spec: rendered.spec,
        mode: SurfaceMode::SessionLock,
        settle: SETTLE,
        cap: SETTLE_CAP,
        lock_required: true,
    });

    log_decision(&provenance, &selected.name, &rendered.command_log, &verdict);

    match verdict {
        Verdict::Approved => {
            // Resolve against the PATH of the *final* environment: the request's `PATH=`
            // assignment if it carried one — which the prompt showed — else the gate's own
            // root-controlled list. Never an inherited value.
            let path_var = env.get(b"PATH").unwrap_or_default().to_vec();
            let resolved = match exec::resolve(&req.command, &path_var) {
                Ok(r) => r,
                Err(e) => return Fail::Error(e),
            };
            let mut command_argv = vec![req.command.clone()];
            command_argv.extend(req.args.iter().cloned());
            exec::tighten_fds(Some(selected.fd));
            // Only reached if the exec failed. Execution failure is an error, not approval.
            Fail::Error(exec::exec(&resolved, &command_argv, &env.to_envp()))
        }
        Verdict::Denied => Fail::Denied,
        Verdict::DeniedSettleCap => Fail::Error(
            "input kept arriving; the prompt never settled, so nothing was approved".to_string(),
        ),
        Verdict::DeniedSignal(sig) => Fail::Error(format!("denied: received signal {sig}")),
        Verdict::Error(e) => Fail::Error(e),
    }
}

/// Parse SUDO_UID as a nonzero numeric uid and resolve its name through passwd. Reject missing or
/// inconsistent provenance rather than trusting SUDO_USER.
fn provenance(cap: &Captured) -> Result<Provenance, String> {
    let uid = parse_id(cap.sudo_uid.as_deref(), "SUDO_UID")?;
    if uid == 0 {
        return Err("SUDO_UID is 0: the gate cannot attribute this request".to_string());
    }
    let gid = parse_id(cap.sudo_gid.as_deref(), "SUDO_GID")?;
    let user = sys::passwd_name(uid)
        .ok_or_else(|| format!("uid {uid} has no passwd entry"))?;
    if let Some(claimed) = &cap.sudo_user {
        if claimed != &user {
            return Err(format!(
                "inconsistent provenance: SUDO_USER claims {} but uid {uid} is {}",
                String::from_utf8_lossy(claimed),
                String::from_utf8_lossy(&user)
            ));
        }
    }
    Ok(Provenance { uid, gid, user })
}

fn parse_id(value: Option<&[u8]>, name: &str) -> Result<u32, String> {
    let raw = value.ok_or_else(|| {
        format!("{name} is not set; sudo-prompt must be invoked through sudo")
    })?;
    std::str::from_utf8(raw)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| format!("{name} is not a number: {}", String::from_utf8_lossy(raw)))
}

/// One escaped record per decision. One line, no chatter.
fn log_decision(prov: &Provenance, display: &str, command: &str, verdict: &Verdict) {
    let outcome = match verdict {
        Verdict::Approved => "approve",
        Verdict::Denied => "deny",
        Verdict::DeniedSettleCap => "deny(settle-cap)",
        Verdict::DeniedSignal(sig) => {
            return log_record(prov, display, command, &format!("deny(signal {sig})"))
        }
        Verdict::Error(_) => "error",
    };
    log_record(prov, display, command, outcome)
}

fn log_record(prov: &Provenance, display: &str, command: &str, outcome: &str) {
    let user = String::from_utf8_lossy(&prov.user).to_string();
    log::info!(
        "{outcome}: uid={} user={} display={} command={}",
        prov.uid,
        user,
        display,
        command
    );
    journal::send(&[
        ("MESSAGE", &format!("sudo-prompt {outcome}: {command}")),
        ("SUDO_PROMPT_OUTCOME", outcome),
        ("SUDO_PROMPT_UID", &prov.uid.to_string()),
        ("SUDO_PROMPT_USER", &user),
        ("SUDO_PROMPT_DISPLAY", display),
        ("SUDO_PROMPT_COMMAND", command),
        ("SYSLOG_IDENTIFIER", "sudo-prompt"),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(uid: Option<&str>, gid: Option<&str>, user: Option<&str>) -> Captured {
        Captured {
            sudo_uid: uid.map(|s| s.as_bytes().to_vec()),
            sudo_gid: gid.map(|s| s.as_bytes().to_vec()),
            sudo_user: user.map(|s| s.as_bytes().to_vec()),
            ..Default::default()
        }
    }

    #[test]
    fn provenance_requires_sudo_uid() {
        let err = provenance(&cap(None, Some("0"), None)).unwrap_err();
        assert!(err.contains("SUDO_UID is not set"), "{err}");
    }

    #[test]
    fn provenance_rejects_root_and_junk() {
        assert!(provenance(&cap(Some("0"), Some("0"), None)).unwrap_err().contains("is 0"));
        assert!(provenance(&cap(Some("x"), Some("0"), None)).unwrap_err().contains("not a number"));
        assert!(provenance(&cap(Some("-1"), Some("0"), None)).unwrap_err().contains("not a number"));
        assert!(provenance(&cap(Some("1"), None, None)).unwrap_err().contains("SUDO_GID"));
    }

    #[test]
    fn provenance_resolves_our_own_uid_through_passwd() {
        // SAFETY: always safe.
        let uid = unsafe { libc::getuid() };
        if uid == 0 {
            return;
        }
        let name = sys::passwd_name(uid).expect("own passwd entry");
        let p = provenance(&cap(Some(&uid.to_string()), Some("1"), None)).unwrap();
        assert_eq!(p.uid, uid);
        assert_eq!(p.user, name);
    }

    #[test]
    fn provenance_rejects_a_lying_sudo_user() {
        // SAFETY: always safe.
        let uid = unsafe { libc::getuid() };
        if uid == 0 {
            return;
        }
        let err = provenance(&cap(Some(&uid.to_string()), Some("1"), Some("definitely-not-me")))
            .unwrap_err();
        assert!(err.contains("inconsistent provenance"), "{err}");
    }

    #[test]
    fn nonexistent_uid_has_no_passwd_entry() {
        let err = provenance(&cap(Some("4294967000"), Some("1"), None)).unwrap_err();
        assert!(err.contains("no passwd entry"), "{err}");
    }
}
