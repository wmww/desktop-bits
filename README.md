Wayland desktop environment bits and bobs.

- `sudo-prompt` — the sole sudo authorization gate: presents a fixed prompt on an
  `ext-session-lock-v1` surface and execs the approved command as root.
- `sudo-shim` — `/usr/local/bin/sudo`, an unprivileged dispatcher that rewrites requests into gate
  invocations and passes everything else through to the real sudo.
- `permission-prompt` — a generic yes/no presenter. Not an authorization boundary.
- `permission-prompt-ui` — the shared GTK4 code: escaping, surface modes, the input state machine.

`cargo build --release`, then `install/install.sh install --user <you>`. See
`notes/permission-prompt.md` for the design and `notes/sudo-prompt-operations.md` for rollout,
verification and recovery.
