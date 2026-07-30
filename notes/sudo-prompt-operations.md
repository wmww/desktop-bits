# Installing, verifying and recovering sudo-prompt

Design and rationale: `permission-prompt.md`. This is the operational side.

**Keep a root TTY logged in for the whole switch.** A malformed sudoers line fails open into "no rule
at all", which under this design means nobody can sudo. Do not deploy on a system with no root
password.

## Rollout order

1. Confirm the root password works from a TTY. Everything below assumes that escape hatch.
2. `cargo build --release`, then `install/install.sh install --user <human> --user ai`. It refuses to
   install unless `/usr/local`, `/usr/local/bin`, the shim and the gate are root-owned and not group-
   or other-writable, and it checks the host requirements below.
3. Run the verify list (`install/install.sh verify` covers the automatable parts; the rest is below).
4. Run the abandoned-lock recovery drill.
5. Prove the new path end to end from a human uid **and** from `ai`.
6. Only then remove the zenity/bash chain, the old `%wheel` rule, and the uids' now-pointless wheel
   membership.

## Verify on the host

Re-check the sudo items if the host ever switches to sudo-rs, which supports a subset of sudoers.

- The command-argument wildcard rule parses and matches as intended: `sudo-prompt -- id` matches,
  and both bare `sudo-prompt id` and bare `sudo-prompt --` match nothing. (`install.sh` checks all
  three with `sudo -l -U`.) Note the rule deliberately does not match an empty request: fnmatch needs
  the literal space, so sudo refuses it before the gate sees it. The gate's own empty-argv rejection
  is a second line, not the first.
- Every group member, `ai` included, can reach the gate, and the prompt names the right uid. Agent
  sudo is a supported flow, so "the agent's sudo works and is attributable" is a verification item.
- `sudo -n` for a group member is denied immediately, with no password and no gate prompt, so
  non-interactive callers fail fast instead of parking on the lock.
- `secure_path` is set, so the PATH the gate execs against is root-controlled.
- `use_pty` is on, so the approved command does not share the caller's terminal, where another
  process of the caller's uid could inject input via TIOCSTI. Check `dev.tty.legacy_tiocsti=0` as a
  second layer.
- wlbouncer still denies virtual-keyboard, input-method and virtual-pointer to every non-root uid:
  the approval guarantee assumes no untrusted uid can synthesize input to the prompt.
- wlbouncer denies `ext_session_lock_manager_v1` to every non-root uid. Default-deny, but confirm it
  is not in the base set and is not granted alongside layer shell to `ff`/`comms`/`code`. A non-root
  uid holding it can lock the session before the gate does, which both blocks all sudo and puts an
  uncoverable fake prompt on screen.
- The compositor implements `ext-session-lock-v1` (sway does) and `gtk_session_lock_is_supported()`
  returns true as root. The gate has no fallback, so this is a hard install-time requirement, and
  **gtk4-layer-shell must be ≥ 1.2.0** (`Instance` is 1.1, but the `monitor` signal is 1.2).
- A `root ALL=(ALL:ALL) ALL` style rule exists, so the interpreter path's inner sudo can run commands
  as other users. sudo never authenticates uid 0, so no NOPASSWD is needed.
- That same rule carries SETENV implicitly, because sudoers(5) implies SETENV for a rule matching
  `ALL`. The interpreter path depends on it: the shim turns `--preserve-env` into command-line
  variables for the inner sudo, and without SETENV that sudo refuses them. Confirm with `sudo -l` and
  a live `sudo -u <other> FOO=bar env`. If root's rule is ever narrowed, it needs an explicit SETENV
  tag. The gate's own rule needs none and must not be granted one.
- `ignore_dot` is at its default (on), matching the gate's own PATH resolution, which skips empty
  path elements rather than searching the current directory.

## Recovery from an abandoned session lock

A gate killed with SIGKILL (or OOM-killed, or crashed hard enough to skip its unlock) leaves the
compositor's session locked with no client. On sway that is a **plain red screen that no keystroke
dismisses**. This is the accepted cost of session-lock mode.

Drilled and confirmed working on this host's sway:

1. From the root TTY, run another lock client and dismiss it — it takes over the lock and unlocks on
   exit. `permission-prompt --surface session-lock --title recovery` then Escape does exactly this,
   and restores the desktop underneath.
2. Failing that, restart the compositor from the TTY.

`tests/gui-test.sh` does not cover this (it needs a deliberate SIGKILL and a manual check); run the
drill by hand before rollout, not during an incident.

The flock needs no equivalent recovery: it lives on the open file description, so the kernel releases
it on any exit including SIGKILL. There is no PID file and no stale-lock handling.

## Reading the decision

One escaped record per decision on stderr via `env_logger`, and best-effort to the systemd journal
(`SYSLOG_IDENTIFIER=sudo-prompt`), silently skipped when the socket is absent:

~~~
approve: uid=1006 user=ai display=wayland-1 command=/usr/bin/rm -rf '/tmp/a b'
deny(settle-cap): uid=1000 user=… display=wayland-1 command=…
~~~

sudo's own logging already records the gate invocation; this record is the decision.

## Still to check on the real host

Things that cannot be tested from an unprivileged uid in a nested compositor, so they are untested
code paths until someone runs them as root:

- The whole real-sudo integration: sudoers argv matching, `SUDO_UID`, a plain round trip, a
  `--preserve-env` round trip on both paths, an assignment round trip proving the command sees it and
  the gate never did, an interpreter round trip (`sudo -u <other> id`) with the interpreter named in
  the prompt, `-i`/`-s` smoke tests including Ctrl-C and window resize through the nested `use_pty`
  layers, and `use_pty` behaviour under a full-screen command such as `less`.
- Denial of `-e` and its abbreviations via all three routes: the shim, an explicit `sudo -e`, and a
  direct `sudo-prompt -- /usr/bin/sudo --ed …`.
- The journal record actually landing.
- The panic hook unlocking (needs a panic injected into a GTK callback).
- The generic presenter's `auto` fallback ordering on a compositor that lacks the session lock
  manager, and on one that lacks layer shell too.
