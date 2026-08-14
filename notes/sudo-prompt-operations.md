# Rolling out, verifying and recovering sudo-prompt

Design and rationale: `permission-prompt.md`. The install steps are in the repo README; this is the
operational side around them.

**Keep a root TTY logged in for the whole switch.** A malformed sudoers line fails open into "no rule
at all", which under this design means nobody can sudo. Do not deploy on a system with no root
password.

## Rollout order

Nothing in the repo installs anything, so the ordering is the operator's to hold. Two orderings are
in play and they differ:

- The README does the symlink (step 3) *before* the sudoers rule (step 4). Between those, `sudo` is
  routed to the shim with no rule for the gate. Harmless if the host still has an ordinary
  password-based `%wheel` rule to fall back on; a lockout if the only rule was a NOPASSWD entry for
  some previous gate.
- `desktop-setup.sh`'s `override_sudo` writes the sudoers line *first*, then the symlink. That is
  the safe order and the one to follow by hand.

Around that:

1. Confirm the root password works from a TTY. Everything below assumes that escape hatch.
2. Install the binaries. Nothing about `sudo` changes yet — callers still reach `/usr/bin/sudo`.
3. Add the sudoers rule, then `sudo-prompt/verify.sh`. It warns about the missing `/usr/local/bin/sudo`
   symlink at this point, which is expected. Then work the manual list below.
4. Run the abandoned-lock recovery drill.
5. Create the symlink. This is the switch. Re-run `sudo-prompt/verify.sh`.
6. Prove the new path end to end from a human uid **and** from `ai`.
7. Only then remove whatever chain this replaces and any rule that bypasses the gate. `verify.sh`
   warns about leftover `NOPASSWD` entries and about other sudoers lines naming the gate, which is
   the check that this step actually happened.

Rollback at any point after step 5 is `rm /usr/local/bin/sudo`, which takes effect immediately.

The setup script runs `root_login_setup` (which contains `override_sudo`) *after*
`root_programs_setup` (which builds and installs the binaries), so the gate always exists before
anything routes to it. Keep that ordering if either is ever moved.

## Verify on the host

`verify.sh` covers the automatable parts; everything below marked *(script)* is one of them,
and is listed here for the reasoning, not as a manual step. Re-check the sudo items if the host ever
switches to sudo-rs, which supports a subset of sudoers.

- *(script)* The command-argument wildcard rule parses and matches as intended: `sudo-prompt -- id`
  matches, and both bare `sudo-prompt id` and bare `sudo-prompt --` match nothing. Note the rule
  deliberately does not match an empty request: fnmatch needs the literal space, so sudo refuses it
  before the gate sees it. The gate's own empty-argv rejection is a second line, not the first.
- *(script)* `/usr/local`, `/usr/local/bin`, the gate and the shim are root-owned and not group- or
  other-writable, and neither binary is setuid — a group-writable directory anywhere on the gate's
  path means somebody other than root chooses what runs as root.
- *(script)* `/usr/local/bin/sudo` is a symlink to `sudo-shim` and not some other binary. Separately
  and *not* checkable from root: `/usr/local/bin` precedes `/usr/bin` on the callers' `PATH`,
  otherwise the shim is simply not in the way.
- *(script)* The rule is `%<group> ALL=(root) NOPASSWD: /usr/local/bin/sudo-prompt -- *` and nothing
  else in sudoers names the gate. One pattern covers every invocation, plain or interpreter, because
  they all start with `--`. Never a bare command path, which would permit arbitrary gate arguments.
- *(script)* `Defaults setenv` is not enabled. The rule carries no `SETENV` (implied only for a rule
  matching `ALL`) and must not: with it, the caller could hand the gate its own `PATH` and the prompt
  would name `id` while root ran somebody else's. `NOSETENV` on the rule would make that a property
  of the rule rather than of the host default.
- Every group member, `ai` included, can reach the gate, and the prompt names the right uid. Agent
  sudo is a supported flow, so "the agent's sudo works and is attributable" is a verification item.
- `sudo -n` for a group member is denied immediately, with no password and no gate prompt, so
  non-interactive callers fail fast instead of parking on the lock.
- `secure_path` is **not** a requirement. The gate never reads its inherited PATH (see
  `permission-prompt.md`), so nothing about the prompt's honesty depends on it. Still worth setting
  for sudo used outside the gate; `verify.sh` deliberately does not check it.
- *(script)* `use_pty` is on, so the approved command does not share the caller's terminal, where
  another process of the caller's uid could inject input via TIOCSTI. Check
  `dev.tty.legacy_tiocsti=0` as a second layer.
- wlbouncer still denies virtual-keyboard, input-method and virtual-pointer to every non-root uid:
  the approval guarantee assumes no untrusted uid can synthesize input to the prompt.
- wlbouncer denies `ext_session_lock_manager_v1` to every non-root uid. Default-deny, but confirm it
  is not in the base set and is not granted alongside layer shell to `ff`/`comms`/`code`. A non-root
  uid holding it can lock the session before the gate does, which both blocks all sudo and puts an
  uncoverable fake prompt on screen.
- The compositor implements `ext-session-lock-v1` (sway does) and `gtk_session_lock_is_supported()`
  returns true as root — this part needs a live run. The gate has no fallback, so it is a hard
  requirement, and *(script)* **gtk4-layer-shell must be ≥ 1.2.0** (`Instance` is 1.1, but the
  `monitor` signal is 1.2). The script also checks the gate resolves all its shared libraries, since
  an unresolvable one only shows up at the worst moment.
- *(script)* A `root ALL=(ALL:ALL) ALL` style rule exists, so the interpreter path's inner sudo can
  run commands as other users. sudo never authenticates uid 0, so no NOPASSWD is needed.
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

One escaped record per decision, best-effort to the systemd journal (`SYSLOG_IDENTIFIER=sudo-prompt`,
silently skipped when the socket is absent) and on stderr via `env_logger` — the record is `info`
and the default filter is `warn`, so stderr shows it only under `RUST_LOG=info`:

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
- Denial of `-e` and its abbreviations via the shim's own classification and via a direct
  `sudo-prompt -- /usr/bin/sudo --ed …`. `verify.sh` now covers the third route, `/usr/bin/sudoedit`
  reaching real sudo without passing the shim, by checking sudoers authorizes it for nobody.
- The journal record actually landing.
- The panic hook unlocking (needs a panic injected into a GTK callback).
- The generic presenter's `auto` fallback ordering on a compositor that lacks the session lock
  manager, and on one that lacks layer shell too.
