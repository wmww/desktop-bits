//! Display selection.
//!
//! Caller XDG_RUNTIME_DIR and WAYLAND_DISPLAY are ignored; there is no --auto-display. Non-root
//! runtime dirs on this host hold real, non-root-owned wayland sockets next to symlinks to root's,
//! so "scan /run/user/* for a socket" can land on a caller-controlled compositor. The gate looks
//! only in root's runtime directory and confirms the peer with SO_PEERCRED.

use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::sys::errno_message;

#[derive(Debug)]
pub struct Selected {
    /// The socket name, for the log record.
    pub name: String,
    /// A connected, validated fd. Ownership passes to libwayland via WAYLAND_SOCKET.
    pub fd: RawFd,
}

/// Validate `runtime_dir`, pick the lowest-numbered `wayland-N` in it, connect, and confirm the
/// peer is `expect_uid`.
pub fn select(runtime_dir: &Path, expect_uid: u32) -> Result<Selected, String> {
    let md = std::fs::symlink_metadata(runtime_dir)
        .map_err(|e| format!("cannot stat {}: {e}", runtime_dir.display()))?;
    check_dir(&md, runtime_dir, expect_uid)?;

    let mut candidates: Vec<(u32, String)> = Vec::new();
    let entries = std::fs::read_dir(runtime_dir)
        .map_err(|e| format!("cannot read {}: {e}", runtime_dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(n) = wayland_number(name.as_bytes()) else { continue };
        candidates.push((n, name.to_string_lossy().into_owned()));
    }
    candidates.sort_by_key(|(n, _)| *n);

    // Take the lowest N and do not probe the others: a stale socket from a dead compositor fails
    // the gate until it is removed from a TTY, which is accepted deliberately — recovery is
    // already the root TTY.
    let Some((_, name)) = candidates.first() else {
        return Err(format!("no wayland-N socket in {}", runtime_dir.display()));
    };
    let path = runtime_dir.join(name);

    let md = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
    if md.file_type().is_symlink() {
        return Err(format!("{} is a symlink", path.display()));
    }
    if !is_socket(&md) {
        return Err(format!("{} is not a unix socket", path.display()));
    }
    if owner(&md) != expect_uid {
        return Err(format!("{} is owned by uid {}, not {expect_uid}", path.display(), owner(&md)));
    }

    let fd = connect(&path)?;
    match peer_uid(fd) {
        Ok(uid) if uid == expect_uid => {}
        Ok(uid) => {
            close(fd);
            return Err(format!("{} peer is uid {uid}, not {expect_uid}", path.display()));
        }
        Err(e) => {
            close(fd);
            return Err(e);
        }
    }

    Ok(Selected { name: name.clone(), fd })
}

fn check_dir(md: &std::fs::Metadata, path: &Path, expect_uid: u32) -> Result<(), String> {
    if md.file_type().is_symlink() {
        return Err(format!("{} is a symlink", path.display()));
    }
    if !md.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    if owner(md) != expect_uid {
        return Err(format!("{} is owned by uid {}, not {expect_uid}", path.display(), owner(md)));
    }
    if group_or_other_writable(md) {
        return Err(format!("{} is group or other writable", path.display()));
    }
    Ok(())
}

fn owner(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    md.uid()
}

fn group_or_other_writable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    md.mode() & (libc::S_IWGRP | libc::S_IWOTH) != 0
}

fn is_socket(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    md.file_type().is_socket()
}

/// `wayland-N` where N is a decimal number.
fn wayland_number(name: &[u8]) -> Option<u32> {
    let rest = name.strip_prefix(b"wayland-")?;
    if rest.is_empty() || !rest.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(rest).ok()?.parse().ok()
}

fn connect(path: &Path) -> Result<RawFd, String> {
    let bytes = path.as_os_str().as_bytes();
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if bytes.len() >= std::mem::size_of_val(&addr.sun_path) {
        return Err(format!("{} is too long for a unix socket address", path.display()));
    }
    for (slot, b) in addr.sun_path.iter_mut().zip(bytes) {
        *slot = *b as libc::c_char;
    }

    // SAFETY: ordinary socket calls; addr is fully initialized above.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(errno_message("socket()"));
    }
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        let msg = errno_message(&format!("connect({})", path.display()));
        close(fd);
        return Err(msg);
    }
    Ok(fd)
}

fn peer_uid(fd: RawFd) -> Result<u32, String> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len match what SO_PEERCRED expects.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if rc < 0 {
        return Err(errno_message("getsockopt(SO_PEERCRED)"));
    }
    Ok(cred.uid)
}

fn close(fd: RawFd) {
    // SAFETY: fd is ours and not used again.
    unsafe { libc::close(fd) };
}

/// Clear FD_CLOEXEC so libwayland can use this fd directly via WAYLAND_SOCKET, and no second
/// lookup happens. It must be re-armed before the approval exec — see `exec::tighten_fds`.
pub fn clear_cloexec(fd: RawFd) -> Result<(), String> {
    // SAFETY: plain fcntl on an fd we own.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(errno_message("fcntl(F_GETFD)"));
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(errno_message("fcntl(F_SETFD)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn socket_name_matching() {
        assert_eq!(wayland_number(b"wayland-0"), Some(0));
        assert_eq!(wayland_number(b"wayland-1"), Some(1));
        assert_eq!(wayland_number(b"wayland-12"), Some(12));
        assert_eq!(wayland_number(b"wayland-"), None);
        assert_eq!(wayland_number(b"wayland-0.lock"), None);
        assert_eq!(wayland_number(b"wayland-root"), None);
        assert_eq!(wayland_number(b"wayland-1x"), None);
        assert_eq!(wayland_number(b"xwayland-1"), None);
        assert_eq!(wayland_number(b"pulse"), None);
    }

    #[test]
    fn picks_the_lowest_numbered_socket() {
        let dir = tempfile::tempdir().unwrap();
        // Deliberately created out of order, and with distractors. The listeners must stay alive
        // or the connect would be refused.
        let _listeners: Vec<UnixListener> = ["wayland-3", "wayland-10", "wayland-2"]
            .iter()
            .map(|name| UnixListener::bind(dir.path().join(name)).unwrap())
            .collect();
        std::fs::write(dir.path().join("wayland-1.lock"), b"").unwrap();
        std::os::unix::fs::symlink("/nonexistent", dir.path().join("wayland-0")).unwrap();

        // wayland-0 is a symlink and is rejected outright rather than skipped.
        let err = select(dir.path(), unsafe { libc::geteuid() }).unwrap_err();
        assert!(err.contains("symlink"), "{err}");

        std::fs::remove_file(dir.path().join("wayland-0")).unwrap();
        let sel = select(dir.path(), unsafe { libc::geteuid() }).unwrap();
        assert_eq!(sel.name, "wayland-2");
        close(sel.fd);
    }

    #[test]
    fn rejects_a_wrongly_owned_directory() {
        let dir = tempfile::tempdir().unwrap();
        UnixListener::bind(dir.path().join("wayland-0")).unwrap();
        let err = select(dir.path(), 0).unwrap_err();
        assert!(err.contains("owned by uid"), "{err}");
    }

    #[test]
    fn rejects_a_group_writable_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        UnixListener::bind(dir.path().join("wayland-0")).unwrap();
        let err = select(dir.path(), unsafe { libc::geteuid() }).unwrap_err();
        assert!(err.contains("group or other writable"), "{err}");
    }

    #[test]
    fn rejects_a_non_socket() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wayland-0"), b"").unwrap();
        let err = select(dir.path(), unsafe { libc::geteuid() }).unwrap_err();
        assert!(err.contains("not a unix socket"), "{err}");
    }

    #[test]
    fn reports_a_stale_socket_rather_than_moving_on() {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("wayland-0")).unwrap();
        UnixListener::bind(dir.path().join("wayland-1")).unwrap();
        drop(listener);
        std::fs::remove_file(dir.path().join("wayland-0")).unwrap();
        // Re-create wayland-0 as a socket file with nothing listening.
        let stale = UnixListener::bind(dir.path().join("wayland-0")).unwrap();
        drop(stale);
        let err = select(dir.path(), unsafe { libc::geteuid() }).unwrap_err();
        assert!(err.contains("connect("), "{err}");
    }

    #[test]
    fn empty_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = select(dir.path(), unsafe { libc::geteuid() }).unwrap_err();
        assert!(err.contains("no wayland-N socket"), "{err}");
    }
}
