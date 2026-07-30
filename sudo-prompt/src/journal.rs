//! Best-effort systemd journal record, silently skipped when the socket is absent.
//!
//! sudo's own logging already records the gate invocation; this is the decision.

use std::os::unix::net::UnixDatagram;

const SOCKET: &str = "/run/systemd/journal/socket";

/// The journal's own limit is generous but not unlimited; anything larger is dropped rather than
/// silently mangled. The full text is in the stderr record either way.
const MAX_DATAGRAM: usize = 96 * 1024;

pub fn send(fields: &[(&str, &str)]) {
    if !std::path::Path::new(SOCKET).exists() {
        return;
    }
    let mut buf = Vec::new();
    for (name, value) in fields {
        // The binary form handles every value, newlines included, with no quoting rules.
        buf.extend_from_slice(name.as_bytes());
        buf.push(b'\n');
        buf.extend_from_slice(&(value.len() as u64).to_le_bytes());
        buf.extend_from_slice(value.as_bytes());
        buf.push(b'\n');
    }
    if buf.len() > MAX_DATAGRAM {
        log::debug!("journal record too large ({} bytes); skipped", buf.len());
        return;
    }
    match UnixDatagram::unbound().and_then(|s| s.send_to(&buf, SOCKET)) {
        Ok(_) => {}
        Err(e) => log::debug!("journal record not sent: {e}"),
    }
}
