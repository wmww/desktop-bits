# Host setup this repo slots into

The user's desktop config lives in `~/playground/personal/desktop/` (separate repo). Relevant facts,
since programs here are built against it.

## UID sandboxing

Per the author's https://loginasroot.net/diy_linux_sandboxing: separate UIDs per domain rather than
containers. `root` (personal files, runs the compositor), `ff` (browser), `code`, `comms`, `play`.

Sway runs **as root**. Other uids get shells via `login-as` → `machinectl shell $U@.host` →
`login-as-inner`, which symlinks root's wayland socket into the target user's runtime dir and sets
`PULSE_SERVER`, `MOZ_ENABLE_WAYLAND` etc. So root's runtime dir is reachable by other uids.

## wlbouncer

`LD_PRELOAD=libwlbouncer-preload.so sway`, policy in `wlbouncer.yaml` → `/usr/local/etc/`. Filters
Wayland globals per uid. Key points for us:

- Everyone gets a base set (`xdg_wm_base`, `wl_shm`, …) — so **any uid can make windows**.
- `zwlr_layer_shell_v1` is denied by default; `root` gets everything; `ff`/`comms` get it (portal
  needs it), `code` gets it.
- Virtual keyboard, input method, virtual pointer and data-control are denied to every non-root uid,
  so input spoofing against a root-owned prompt isn't available to sandboxed uids. Screencopy is
  denied in the base set but **granted to `ff` and `comms`** (they need it for
  xdg-desktop-portal-wlr), so those two uids can read the gate's prompt off the screen. Reading is
  not approving, so the guarantee holds, but the prompt is not confidential from them.
- `ext_session_lock_manager_v1` should be denied to every non-root uid (default-deny, but
  **unverified** — check it isn't in the base set). A uid holding it can lock the session, which
  blocks `sudo-prompt` entirely and puts an uncoverable surface on screen.

`sudo-prompt` covers the desktop with an `ext-session-lock-v1` surface (via `gtk4-session-lock`,
which needs gtk4-layer-shell ≥ 1.2.0) rather than an `xdg_toplevel`, and has no fallback — not even
to layer shell, which non-root uids can draw over. The generic presenter picks session lock → layer
shell → toplevel, which on this host means a toplevel for everyone but root.

## sudo chain (pre-`permission-prompt`)

1. `/usr/local/bin/sudo` (`sudo-override.sh`) shadows the real one; splits sudo's flags from the
   command with `getopt` and rewrites to `sudo FLAGS /usr/local/bin/validate-sudo CMD`. Passes
   through untouched when `EUID == 0`.
2. `/etc/sudoers`: `%wheel ALL=(ALL:ALL) NOPASSWD: /usr/local/bin/validate-sudo` — sudoers itself
   enforces that the gate is the only passwordless target.
3. `validate-sudo.sh` runs as the target user, hardcodes `WAYLAND_DISPLAY=wayland-1`,
   `XDG_RUNTIME_DIR=/run/user/0`, `GTK_THEME=Sweet`, shows a zenity yes/no, then `exec "$@"`.

Known problems (all addressed by `plans/permission-prompt.md`): the getopt splitting is fragile;
argv is joined into one ambiguous string with only `&` escaped; the prompt runs as the *target* user
so `sudo -u ff …` would have no layer shell; `sudo -s`/`-i` prompt with an empty command and then
exec nothing; hardcoded display; zenity conflates "no display" with "denied"; the caller's
environment reaches a root GTK process unscrubbed under `-E`.

Installed by `desktop-setup.sh` (`root_login_setup` → `override_sudo`, `install_wlbouncer`).

## Distro

Arch, stock `sudo` (1.9.17 as of 2026-07). Possible future switch to sudo-rs, which has no plugin
API — assume no sudo plugins either way.

## Misc facts checked 2026-07-27

- `ai` (uid 1006) is in `wheel`, so the agent uid itself has the passwordless path to the gate.
- `wheel` is `anon,code,install,ai,cal` (checked 2026-08-07), and `wheel` is the group the gate rule
  names. Everyone in it can raise a root prompt with any argv. Worth periodically asking whether
  `anon` and `cal` need it.
- `install` holds `ALL=(ALL:ALL) NOPASSWD: ALL` by design — a second uid trusted with unrestricted
  root, alongside root itself, and outside anything the gate covers. Pass it to `verify.sh` as
  `--trusted install` so it is reported rather than counted as a bypass.
- `/run/user/0` is `drwx-----x root root` — other uids can traverse in and open sockets by name,
  which is how `login-as-inner`'s symlinks work.
- Non-root runtime dirs hold **real, non-root-owned** wayland sockets (`/run/user/1006/wayland-0`,
  owned by uid 1006) next to the `wayland-root` symlink to root's socket. So "scan `/run/user/*`
  for a wayland socket" can land on a caller-controlled compositor — hence the gate looks only in
  `/run/user/0` and checks `SO_PEERCRED`.
- `pkexec` is installed: a separate, ungated path to root with a spoofable toplevel prompt.
- `/usr/bin/sudoedit` is a symlink to `sudo` and is not shadowed by `/usr/local/bin/`.

## sudo/libc facts checked 2026-07-29 (locale/terminfo items 2026-08-07)

Verified against this host's sudo 1.9 docs, glibc 2.44 and ncurses; `permission-prompt` depends on
all of them. The last two are the *reason* a check exists, not a check the gate relies on — it
validates `TERM` itself rather than trusting either library, see `permission-prompt.md`.

- **SETENV is implied for a sudoers rule matching `ALL`** (sudoers(5)). So `root ALL=(ALL:ALL) ALL`
  accepts `VAR=value` and `-E`/`--preserve-env` with no explicit tag — which is what lets the gate's
  interpreter path hand command-line variables to an inner sudo. Narrowing root's rule to specific
  commands would silently break that; a narrow rule (like the gate's own) gets no SETENV.
- **`env_reset` has a hardcoded survivor set** — TERM, PATH, HOME, MAIL, SHELL, LOGNAME, USER,
  SUDO_* — that no `env_keep`/`env_check`/`env_delete` setting removes, on top of the env_keep list.
  HOME and TERM in it are the *caller's* values. So sudo cannot be configured to hand a target a
  genuinely empty environment; a program that needs one must construct it itself.
- **glibc's `execvpe` resolves PATH from the calling process's `environ`, not from the `envp` it
  passes to the child** (checked with a test program both ways). A process that scrubbed its own
  environment therefore cannot use `execvpe` to resolve against the child's PATH — resolve manually
  and `execve`. `execvp` has the same problem in reverse: it needs `environ` mutated, which is
  unsafe once a toolkit has started threads.
- **glibc will not follow a locale name out of `/usr/lib/locale`** (traced with `strace` on 2.44). A
  name containing `..`, or starting with `.`, is refused before any filesystem access and
  `setlocale` returns NULL. An absolute-looking name is *concatenated*, not honoured:
  `LC_ALL=/tmp/x` opens `/usr/lib/locale//tmp/x/LC_CTYPE`, which stays under a root-owned directory.
  `LOCPATH` is the variable that genuinely redirects the load. So a forwarded `LANG` is not a
  file-loading hazard — the hazard is semantic (decimal separator, collation, `rpmatch`, gettext
  text silently changing what a root script does), which is why the gate sets `LANG=C.UTF-8` and
  forwards no locale variable at all.
- **ncurses rejects a `TERM` containing `/` or `..` outright** — no `openat`, no `access`, just
  `unknown terminal`. Lookup is `/usr/share/terminfo/<c>/<name>` plus `$HOME/.terminfo`, so with
  `TERMINFO`/`TERMINFO_DIRS`/`TERMCAP`/`TERMPATH` unset and `HOME=/root` every search root is
  root-owned and `TERM` is only an index into one.
