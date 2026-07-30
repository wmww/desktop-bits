//! Every path exec()s in place, so exit status and signals pass through untouched, and argv is
//! forwarded as raw bytes with no re-quoting or lossy string conversion.

use std::ffi::{CString, OsString};
use std::os::unix::ffi::OsStrExt;

use sudo_shim::classify::{classify, Decision, REAL_SUDO};

/// If a shim exec itself fails, print the errno and exit 125.
const EXIT_FAILURE: i32 = 125;

fn main() {
    let argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    // SAFETY: geteuid is always safe.
    let euid = unsafe { libc_geteuid() };

    let decision = classify(euid, &argv, &|name| {
        std::env::var_os(std::ffi::OsStr::from_bytes(name)).map(|v| v.as_bytes().to_vec())
    });

    let args: Vec<OsString> = match decision {
        Decision::PassThrough => argv,
        Decision::Gate(gate_argv) => gate_argv,
    };

    let target = real_sudo();
    let err = exec(&target, &args);
    eprintln!("sudo: cannot execute {target}: {err}");
    std::process::exit(EXIT_FAILURE);
}

#[cfg(not(feature = "test-exec-override"))]
fn real_sudo() -> String {
    REAL_SUDO.to_string()
}

/// Test seam so an integration test can point the exec at a fake sudo and check the constructed
/// gate argv byte-for-byte. Never enabled in the installed binary. Harmless either way: the shim
/// runs as the caller, who could invoke anything directly.
#[cfg(feature = "test-exec-override")]
fn real_sudo() -> String {
    std::env::var("SUDO_SHIM_REAL_SUDO").unwrap_or_else(|_| REAL_SUDO.to_string())
}

/// Never returns on success.
fn exec(target: &str, args: &[OsString]) -> std::io::Error {
    let Ok(path) = CString::new(target.as_bytes()) else {
        return std::io::Error::other("target path contains a NUL byte");
    };
    // argv[0] is what sudo reports itself as; keep it "sudo" as the caller expects.
    let mut owned = vec![CString::new("sudo").expect("literal")];
    for a in args {
        match CString::new(a.as_bytes()) {
            Ok(c) => owned.push(c),
            Err(_) => return std::io::Error::other("argument contains a NUL byte"),
        }
    }
    let mut ptrs: Vec<*const i8> = owned.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());

    // SAFETY: both arrays are NUL-terminated and outlive the call. The environment is inherited
    // deliberately: the caller's environment is what real sudo applies its own policy to.
    unsafe {
        execv(path.as_ptr(), ptrs.as_ptr());
    }
    std::io::Error::last_os_error()
}

// Declared directly rather than depending on the libc crate: the shim deliberately has no
// dependencies.
extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
    fn execv(path: *const i8, argv: *const *const i8) -> i32;
}
