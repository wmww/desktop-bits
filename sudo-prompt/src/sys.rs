//! Thin wrappers over the libc calls the gate needs.

use std::ffi::CStr;
use std::os::unix::ffi::OsStringExt;

/// The caller's working directory, read before the environment is touched. `None` if it is gone —
/// a caller whose cwd vanished should still be able to run sudo.
pub fn getcwd_bytes() -> Option<Vec<u8>> {
    std::env::current_dir().ok().map(|p| p.into_os_string().into_vec())
}

fn passwd(uid: u32) -> Option<libc::passwd> {
    // SAFETY: getpwuid returns a pointer into a static buffer, valid until the next call. We copy
    // out immediately and never keep the pointer.
    unsafe {
        let p = libc::getpwuid(uid as libc::uid_t);
        if p.is_null() {
            None
        } else {
            Some(*p)
        }
    }
}

pub fn passwd_name(uid: u32) -> Option<Vec<u8>> {
    let p = passwd(uid)?;
    if p.pw_name.is_null() {
        return None;
    }
    // SAFETY: pw_name is a NUL-terminated C string owned by libc's static buffer.
    Some(unsafe { CStr::from_ptr(p.pw_name) }.to_bytes().to_vec())
}

/// root's login shell, for the command environment's SHELL.
pub fn root_shell() -> Vec<u8> {
    match passwd(0) {
        Some(p) if !p.pw_shell.is_null() => {
            // SAFETY: as above.
            let s = unsafe { CStr::from_ptr(p.pw_shell) }.to_bytes().to_vec();
            if s.is_empty() {
                b"/bin/sh".to_vec()
            } else {
                s
            }
        }
        _ => b"/bin/sh".to_vec(),
    }
}

pub fn errno_message(context: &str) -> String {
    format!("{context}: {}", std::io::Error::last_os_error())
}
