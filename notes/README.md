# Notes index

- `host-sudo-setup.md` — the user's desktop config repo, UID sandboxing scheme, wlbouncer policy,
  and the bash sudo-authorization chain that `sudo-prompt` replaces. Also the sudo/glibc facts the
  design depends on.
- `permission-prompt.md` — design record for the `permission-prompt`/`sudo-prompt` workspace: threat
  model, why the binaries are split, the shim's classification, the gate's two environments,
  rendering safety, the input state machine, and the session-lock discipline.
- `sudo-prompt-operations.md` — installing it, the verify list for the host, the abandoned-lock
  recovery drill, and what is still untested because it needs root.
