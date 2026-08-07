//! The gate's *own* environment.
//!
//! sudo's env_reset still hands us caller-influenced survivors (HOME, TERM, DISPLAY, LANG,
//! LS_COLORS, …), and nothing the caller said may steer root's GTK. So: capture the few values
//! needed later, clear the environment, and set only root-controlled prerequisites — all before
//! GTK initialization or any threads exist.
//!
//! This is the only module in the workspace permitted to mutate `environ`; the source lint
//! enforces that. The command's environment is built separately (see [`crate::cmdenv`]) and passed
//! to `execve` as an owned list, never written here.

use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// Fixed, root-controlled PATH for the gate itself. The command gets its own, see [`crate::cmdenv`].
const GATE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Named passthrough candidates for the *command's* environment. TERM is here because without it
/// every interactive root command is unusable; its loader-dangerous relatives TERMINFO, TERMCAP
/// and TERMPATH are deliberately not.
pub const PASSTHROUGH: &[&str] = &["TERM", "COLORTERM", "LANG", "LANGUAGE"];

pub fn is_passthrough(name: &[u8]) -> bool {
    PASSTHROUGH.iter().any(|n| n.as_bytes() == name) || name.starts_with(b"LC_")
}

/// Everything kept from the inherited environment. Not the whole environment.
///
/// PATH is deliberately absent: sudo's env_reset preserves the caller's PATH unless secure_path is
/// set, and the gate cannot tell the two apart, so it reads neither and uses its own
/// [`crate::cmdenv::COMMAND_PATH`] instead.
#[derive(Debug, Default, Clone)]
pub struct Captured {
    pub sudo_uid: Option<Vec<u8>>,
    pub sudo_gid: Option<Vec<u8>>,
    pub sudo_user: Option<Vec<u8>>,
    /// Passthrough candidates, in the order the environment listed them.
    pub passthrough: Vec<(Vec<u8>, Vec<u8>)>,
}

pub fn capture() -> Captured {
    let mut cap = Captured::default();
    for (name, value) in std::env::vars_os() {
        let name = name.into_vec();
        let value = value.as_bytes().to_vec();
        match name.as_slice() {
            b"SUDO_UID" => cap.sudo_uid = Some(value),
            b"SUDO_GID" => cap.sudo_gid = Some(value),
            b"SUDO_USER" => cap.sudo_user = Some(value),
            _ if is_passthrough(&name) => cap.passthrough.push((name, value)),
            _ => {}
        }
    }
    cap
}

/// Clear the environment and set only root-controlled GTK prerequisites.
///
/// Never configure root GTK from caller GTK_THEME, GTK_MODULES, GIO_MODULE_DIR, XDG_DATA_DIRS,
/// runtime or display variables — so those are simply not set, and GTK uses system defaults.
pub fn scrub() {
    // SAFETY: single threaded, before GTK initialization.
    unsafe {
        libc::clearenv();
    }
    std::env::set_var("HOME", "/root");
    std::env::set_var("PATH", GATE_PATH);
    std::env::set_var("XDG_DATA_DIRS", "/usr/local/share:/usr/share");
    std::env::set_var("XDG_CONFIG_DIRS", "/etc/xdg");
    // A predictable UTF-8 locale for the gate's own rendering.
    std::env::set_var("LANG", "C.UTF-8");
}

/// Set the display variables, only after display selection has validated the runtime directory.
///
/// XDG_RUNTIME_DIR is set explicitly rather than left unset: GLib otherwise falls back to a
/// cache-directory runtime path, and GTK, dconf and the cursor cache all land somewhere
/// unintended.
pub fn set_display_env(runtime_dir: &str, wayland_socket_fd: std::os::fd::RawFd) {
    std::env::set_var("XDG_RUNTIME_DIR", runtime_dir);
    std::env::set_var("WAYLAND_SOCKET", wayland_socket_fd.to_string());
    // Belt and braces: WAYLAND_SOCKET wins, but never leave a caller-derived name behind.
    std::env::remove_var("WAYLAND_DISPLAY");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_membership() {
        assert!(is_passthrough(b"TERM"));
        assert!(is_passthrough(b"COLORTERM"));
        assert!(is_passthrough(b"LANG"));
        assert!(is_passthrough(b"LANGUAGE"));
        assert!(is_passthrough(b"LC_ALL"));
        assert!(is_passthrough(b"LC_TIME"));
        assert!(!is_passthrough(b"TERMINFO"));
        assert!(!is_passthrough(b"TERMCAP"));
        assert!(!is_passthrough(b"TERMPATH"));
        assert!(!is_passthrough(b"LD_PRELOAD"));
        assert!(!is_passthrough(b"DISPLAY"));
        assert!(!is_passthrough(b"XAUTHORITY"));
        assert!(!is_passthrough(b"LS_COLORS"));
        assert!(!is_passthrough(b"HOME"));
    }
}
