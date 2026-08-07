//! The single execution path: resolve the command ourselves, then `execve` with an explicit
//! environment.
//!
//! Not `execvp`/`execvpe`. `execvp` resolves against the live `environ`, so it would need `environ`
//! rewritten at approval time — after GTK init, with GLib worker threads running, where clearing or
//! replacing the environment races any concurrent read. `execvpe` avoids that but resolves PATH from
//! the *caller's* `environ` and not from the `envp` it passes on (verified against this host's
//! glibc), which with a scrubbed gate environment would silently ignore the command's PATH and any
//! `PATH=` assignment.

use std::ffi::CString;
use std::os::fd::RawFd;

use crate::sys::errno_message;

enum Probe {
    Runnable,
    Missing,
    Denied,
}

fn probe(candidate: &[u8]) -> Probe {
    let Ok(c) = CString::new(candidate.to_vec()) else { return Probe::Missing };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: c is NUL terminated, st is the right type.
    if unsafe { libc::stat(c.as_ptr(), &mut st) } < 0 {
        return match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EACCES) => Probe::Denied,
            _ => Probe::Missing,
        };
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Probe::Missing;
    }
    // SAFETY: as above.
    if unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0 {
        Probe::Runnable
    } else {
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EACCES) => Probe::Denied,
            _ => Probe::Missing,
        }
    }
}

/// Resolve COMMAND against the PATH of the *final* environment: the request's `PATH=` assignment
/// if it carried one, else the gate's own root-controlled list. Never an inherited PATH.
///
/// Empty path elements are skipped rather than treated as the current directory, matching sudo's
/// `ignore_dot` default.
pub fn resolve(command: &[u8], path_var: &[u8]) -> Result<Vec<u8>, String> {
    if command.contains(&b'/') {
        // A relative path means the user is approving whatever that path holds at exec time —
        // the same contract stock sudo offers.
        return Ok(command.to_vec());
    }

    let mut denied = false;
    for element in path_var.split(|b| *b == b':') {
        if element.is_empty() {
            continue;
        }
        let mut candidate = element.to_vec();
        if candidate.last() != Some(&b'/') {
            candidate.push(b'/');
        }
        candidate.extend_from_slice(command);
        match probe(&candidate) {
            Probe::Runnable => return Ok(candidate),
            Probe::Denied => denied = true,
            Probe::Missing => {}
        }
    }

    let name = String::from_utf8_lossy(command);
    if denied {
        Err(format!("{name}: found on PATH but not executable"))
    } else {
        Err(format!("{name}: command not found on PATH"))
    }
}

/// Re-arm FD_CLOEXEC on the Wayland fd and close everything else above fd 2.
///
/// Display selection deliberately cleared FD_CLOEXEC on the Wayland fd, and libwayland does not
/// necessarily restore it, so the approved command would otherwise inherit an open, root
/// authenticated connection to root's compositor. wlbouncer filters globals per uid at connect
/// time, so that connection would carry virtual keyboard, virtual pointer, screencopy, layer shell
/// and the session lock manager into a command that may be running as an unprivileged uid — exactly
/// the capability set the sandbox policy denies them.
///
/// GLib, dconf and GIO hold fds of their own, so "assert nothing above 2 is open" is not a property
/// that will hold; closing them is. And do not rely on the interpreter path's inner sudo doing
/// `closefrom` — it does, but only there, and only until someone grants closefrom_override.
pub fn tighten_fds(wayland_fd: Option<RawFd>) {
    if let Some(fd) = wayland_fd {
        set_cloexec(fd);
    }
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        log::warn!("cannot enumerate /proc/self/fd; relying on CLOEXEC alone");
        return;
    };
    let fds: Vec<RawFd> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().parse::<RawFd>().ok())
        .filter(|fd| *fd > 2)
        .collect();
    for fd in fds {
        set_cloexec(fd);
    }
}

fn set_cloexec(fd: RawFd) {
    // SAFETY: plain fcntl; a bad fd just returns an error we ignore.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// Never returns on success. The returned string is the failure to report.
pub fn exec(resolved: &[u8], argv: &[Vec<u8>], envp: &[CString]) -> String {
    let Ok(path) = CString::new(resolved.to_vec()) else {
        return "command path contains a NUL byte".to_string();
    };
    let mut args: Vec<CString> = Vec::with_capacity(argv.len());
    for a in argv {
        match CString::new(a.clone()) {
            Ok(c) => args.push(c),
            Err(_) => return "argument contains a NUL byte".to_string(),
        }
    }

    let mut argp: Vec<*const libc::c_char> = args.iter().map(|c| c.as_ptr()).collect();
    argp.push(std::ptr::null());
    let mut envpp: Vec<*const libc::c_char> = envp.iter().map(|c| c.as_ptr()).collect();
    envpp.push(std::ptr::null());

    // SAFETY: all three arrays are NUL-terminated and outlive the call.
    unsafe {
        libc::execve(path.as_ptr(), argp.as_ptr(), envpp.as_ptr());
    }
    errno_message(&format!("cannot execute {}", String::from_utf8_lossy(resolved)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn executable(dir: &Path, name: &str) {
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn bytes(p: &Path) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        p.as_os_str().as_bytes().to_vec()
    }

    #[test]
    fn absolute_and_relative_paths_are_used_verbatim() {
        assert_eq!(resolve(b"/usr/bin/ls", b"/nowhere").unwrap(), b"/usr/bin/ls");
        assert_eq!(resolve(b"./script", b"/nowhere").unwrap(), b"./script");
        assert_eq!(resolve(b"sub/dir/x", b"").unwrap(), b"sub/dir/x");
    }

    #[test]
    fn path_search_takes_the_first_match_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        executable(&a, "tool");
        executable(&b, "tool");
        let mut path = bytes(&b);
        path.push(b':');
        path.extend_from_slice(&bytes(&a));
        assert_eq!(resolve(b"tool", &path).unwrap(), bytes(&b.join("tool")));
    }

    #[test]
    fn empty_path_elements_are_skipped_not_treated_as_dot() {
        let dir = tempfile::tempdir().unwrap();
        executable(dir.path(), "tool");
        // A leading empty element would mean "." to a naive implementation.
        let mut path = b":".to_vec();
        path.extend_from_slice(&bytes(dir.path()));
        assert_eq!(resolve(b"tool", &path).unwrap(), bytes(&dir.path().join("tool")));
        assert!(resolve(b"tool", b"::").is_err());
    }

    #[test]
    fn trailing_slash_in_a_path_element_is_handled() {
        let dir = tempfile::tempdir().unwrap();
        executable(dir.path(), "tool");
        let mut path = bytes(dir.path());
        path.push(b'/');
        assert_eq!(resolve(b"tool", &path).unwrap(), bytes(&dir.path().join("tool")));
    }

    #[test]
    fn directories_and_non_executables_are_not_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tool")).unwrap();
        let err = resolve(b"tool", &bytes(dir.path())).unwrap_err();
        assert!(err.contains("command not found"), "{err}");

        let dir2 = tempfile::tempdir().unwrap();
        std::fs::write(dir2.path().join("tool"), b"").unwrap();
        std::fs::set_permissions(
            dir2.path().join("tool"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let err = resolve(b"tool", &bytes(dir2.path())).unwrap_err();
        assert!(err.contains("not executable"), "{err}");
    }

    #[test]
    fn missing_reports_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve(b"nope", &bytes(dir.path())).unwrap_err();
        assert!(err.contains("command not found on PATH"), "{err}");
    }

    #[test]
    fn a_later_match_wins_over_an_earlier_permission_error() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let bad = a.join("tool");
        std::fs::write(&bad, b"").unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();
        executable(&b, "tool");
        let mut path = bytes(&a);
        path.push(b':');
        path.extend_from_slice(&bytes(&b));
        assert_eq!(resolve(b"tool", &path).unwrap(), bytes(&b.join("tool")));
    }
}
