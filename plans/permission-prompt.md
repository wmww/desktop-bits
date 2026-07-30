# Plan: permission prompt

Replace the zenity/bash sudo authorization prompt in ~/playground/personal/desktop/ with a Rust
GTK4 gate that presents on a session-lock surface. See notes/host-sudo-setup.md for the host
integration.

## Threat model

Trusted: the kernel; root, including the compositor, which runs as root; /usr/bin/sudo and its
sudoers evaluation; root-owned binaries and every directory on their path; and the human reading the
prompt.

Adversary: a compromised non-root uid. Members of the sudo-prompt group can invoke the gate with
any argv they like. The group holds the human's login uid(s) and the agent uid `ai` (agent sudo is
an intended use case, see Authority model), so a request may originate from semi-autonomous code
the human did not type; the requesting uid is a first-class field in the prompt for that reason.
Every uid can create ordinary windows, because wlbouncer grants `xdg_wm_base` to all. It denies
virtual-keyboard, input-method and virtual-pointer protocols to every non-root uid (see
notes/host-sudo-setup.md) and the gate creates no IM context, so approval cannot be forged by
synthesizing input — only by tricking the human's own press. XWayland clients can inject input only
to other X clients, never to the gate's Wayland surfaces.

Goal: no command runs as root unless a human saw its argv on a root-owned surface and approved it.
The gate presents on a session-lock surface (`ext-session-lock-v1`), which the compositor renders
above every other client and routes all input to, so the layer-shell overlay spoofing of
issues/overlay-layer-spoofing.md cannot reach it. What replaces that concern: any uid able to bind
`ext_session_lock_manager_v1` gets the same exclusivity. It cannot cover a live prompt — only one
lock exists at a time, so it must take the lock *first*, and then the gate fails closed and never
prompts — and there is no secret to phish, since the gate takes no password. But a fake prompt on
screen plus a gate that cannot run is bad enough: wlbouncer must deny that global to every non-root
uid (see the verify list). The gate, not sudoers, decides which command runs; sudoers only decides
who may raise a prompt.

**This design requires a root password and a usable console/TTY login.** The gate is the only path
through sudo — there is no password fallback rule and no TTY fallback in the gate. If the GUI is
broken or the compositor is gone, recovery is: log in as root and edit sudoers. Do not deploy this
on a system with no root password.

Accepted, out of scope for this pass:

- A compromised root, and pkexec (see issues/pkexec-root-prompt.md).
- Denial of service, deliberately: there is no prompt timeout once the prompt is answerable (only a
  cap on *settling*, which denies — see UI behavior). A stuck or spammed prompt blocks all sudo, and
  because the gate holds the session lock it also hides the desktop and takes all input until
  dismissed. That includes an unattended agent request sitting on the gate lock: the prompt is on
  screen and Escape dismisses it, so a stuck prompt is impossible to miss. Recovery is a root TTY.
  Wayland disconnect, compositor exit, and any internal error are denials.
- A gate killed with SIGKILL, or crashing hard enough to skip its unlock, leaves the compositor's
  session locked with no client — a blank or placeholder screen that no keystroke dismisses. This is
  the cost of session-lock mode and it is accepted; recovery is the root TTY this design already
  requires (see Session lock mode). Every survivable exit path unlocks first.
- A request raised while another client already holds the session lock. Only one lock exists at a
  time, so the gate's lock request fails and it exits 125 without prompting: sudo does not work
  while the screen is locked, and such a request fails fast instead of parking on the gate lock.
  That is a change from the layer-shell design, where the request waited invisibly, and it is the
  better direction — nobody is there to answer it.
- Visual spoofing by uids that hold `zwlr_layer_shell_v1` (issues/overlay-layer-spoofing.md). It no
  longer reaches the gate, which is session-lock only, but it still reaches the generic presenter in
  layer mode, and the generic presenter is not an authorization boundary anyway.
- An Enter held down from before the prompt cannot approve by itself: a key held across focus
  produces no fresh key down, and even if a backend ever delivered repeats for one, repeats are
  input — they restart the settle period and hit the cap, a denial, rather than approving (see UI
  behavior). So approval takes a fresh physical press, and that one press is enough. An easy Enter
  is deliberate UX.
- What the approved command then does, including a caller-writable `sudo ./script` changing between
  approval and exec. That ambiguity is inherent to sudo and is unchanged here.
- A stale `wayland-N` socket from a dead compositor blocking display selection (see Display
  selection); accepted because recovery is already the TTY.

## Decision

Use two executables sharing Rust UI code but not a security contract:

- permission-prompt is a generic yes/no presenter. It never executes a command, receives no
  passwordless sudo permission, and is not an authorization boundary. Its caller owns its prose and
  interprets its exit status.
- sudo-prompt is the sole sudo gate. It has a fixed security presentation and accepts only
  environment assignments and an argv after --. It has no UI, surface, display, timing, lock,
  theme, or config options.

This split is required. A sudoers-approved binary with arbitrary prompt options lets a requester
disable the settle delay or pick a weaker surface, or narrate a privileged action as something
harmless. The generic
presenter cannot safely be that executable.

## Authority model

sudo-prompt is the sole passwordless sudo target, and the only sudo target at all. A permitted user
may ask it to run any command, but only after the fixed root-owned UI approves it. Thus sudoers
constrains the entry point, not the eventual command: after approval it runs as root. Direct
invocation of the gate must be equivalent to the shim path and must still prompt.

The shim at /usr/local/bin/sudo is convenience, not security. Calling /usr/bin/sudo directly grants
nothing, because no sudoers rule permits anything but the gate. sudoedit has no supported path: the
unshadowed /usr/bin/sudoedit is permitted by no rule and fails, `sudo -e`/`--edit` and its
abbreviations pass through the shim to real sudo and are denied the same way, the gate rejects a
COMMAND whose basename is `sudoedit`, and the gate's interpreter whitelist (see Interpreter
requests) refuses to hand any edit flag to an inner sudo. Edit files as root instead.

Do not overstate that as containment. Every one of those checks is keyed on a *name* in the argv the
human is shown, and an approved root command can reach an editor however it likes — `sudo env sudo
--ed FILE`, `sudo sh -c '…'`, `sudo ./mysudo -e FILE` through a caller-owned symlink to
/usr/bin/sudo (sudo picks edit mode from the flag, not argv[0]), or simply `sudo vim FILE`. Nothing
can prevent that, because root editing files is not a privilege escalation. The purpose of these
checks is that the *shim's own* output is never a sudoedit request and that a directly-invoked gate
request has a shape the human can read — not that root cannot edit a file.

The generic executable receives no sudoers entry. An unprivileged caller can spoof its own generic
request, which grants nothing. A future privileged service that needs generic confirmation must
define its own authenticated request protocol and must not treat arbitrary invocation of
permission-prompt as authorization.

Use a dedicated, deliberately small sudoers group over %wheel: the human's login uid(s) plus the
agent uid `ai`. Membership is the whole security boundary: every member can raise a root prompt at
any moment, and the only defence left is the human reading it.

`ai` is in the group deliberately — an agent asking for root and a human approving it on the
prompt is a supported flow, and it is the same flow the human's own sudo takes. Consequences to
accept: an agent request blocks the gate lock until someone answers it, and the prompt's uid field
is the only thing separating "I typed this" from "the agent typed this", so it must be prominent
and never adjacent to caller-controlled text. Anything that must fail fast instead of waiting for a
human — batch scripts, timers, an agent that cannot block — uses `sudo -n`, which passes through to
real sudo and is denied immediately (see the shim's flag classification).

## Components

Use a workspace with a small shared UI crate and three binaries:

~~~
permission-prompt-ui/       # GTK + surface-mode primitives; internal workspace API
permission-prompt/          # generic yes/no presenter
sudo-prompt/                # fixed sudo gate
sudo-shim/                  # unprivileged /usr/local/bin/sudo dispatcher
~~~

sudo-shim stays a separate binary rather than a second personality of sudo-prompt selected by
argv[0]. It runs as the untrusted caller; the gate runs as root under a sudoers rule. That is the
same boundary the plan draws between permission-prompt and sudo-prompt, and collapsing it would
give the sudoers-named binary a second contract. The shim links no UI crate, takes no dependencies,
and gets no sudoers entry.

sudo-prompt constructs its presentation in code. Do not expose a reusable PromptOptions parser
to it. Test seams belong behind cfg(test) or a test-only Cargo feature not enabled in the installed
binary; no production test display, environment override, or alternate lock path.

The UI crate supports four surface modes — `SessionLock`, `Layer`, `Toplevel`, and `Auto`, where
Auto tries them in exactly that order: session lock if the compositor offers
`ext-session-lock-v1`, else layer shell if it offers `zwlr_layer_shell_v1`, else an xdg toplevel.
The gate compiles in `SessionLock` and has no way to select another; a missing session lock
protocol is an error for it, with no downgrade to layer shell (see Session lock mode). The generic
binary defaults to `Auto` and can be pinned with `--surface=auto|session-lock|layer|toplevel`. This
is the surface type only; it is unrelated to which Wayland socket is chosen, which the gate decides
separately and the generic binary inherits from its own session. Auto matters on this host because
wlbouncer denies `zwlr_layer_shell_v1` to most uids — and must deny `ext_session_lock_manager_v1`
to all of them — so for `play`, `ai` and anything else outside the `ff`/`comms`/`code` set, Auto
falls all the way through to a toplevel.

Use gtk4, gtk4-layer-shell, gtk4-session-lock, log, env_logger, and Rust/libc filesystem APIs. Use
clap only for the generic executable; the gate's tiny CLI parser and the shim should be manual.
gtk4-session-lock binds the same C library as gtk4-layer-shell and needs it at ≥ 1.1.0, so it is
one extra system dependency version, not a new one.

## Sudo shim and sudoers

/usr/local/bin/sudo is the sudo-shim binary. It is not a security boundary — it runs as the caller,
and anything it gets wrong the caller could have done by invoking /usr/bin/sudo directly — so it is
Rust for engineering reasons only: byte-exact argv handling with `env::args_os()`, environment
lookup, and one exec beat threading strings and temp files through shell, and the classification
becomes a pure function returning PassThrough | Gate that unit tests can drive directly.

Classification, a pure function over argv and the shim's own environment:

1. euid 0, or empty argv → PassThrough: `exec /usr/bin/sudo "$@"`.
2. Scan leading tokens until the command token:
   - `--` → end of options; consume it and stop scanning.
   - `NAME=value` with a valid NAME (`[A-Za-z_][A-Za-z0-9_]*`) → collect an assignment.
   - `--preserve-env=LIST` → expand, never forward. For each comma-separated name: if the name is
     valid and set in the shim's environment, collect an assignment `NAME=value` carrying its
     value; unset and invalid names are silently skipped, matching sudo. The flag itself is
     dropped, and the assignments it produced land wherever a caller-written assignment would (see
     steps 4 and 5). So the gate never sees `--preserve-env` in any spelling and its interpreter
     whitelist does not need to know the flag exists.
   - whitelist interpreter flags: `-i`, `-s`, `-u USER`, `-g GROUP`, `--user=USER`,
     `--group=GROUP`, `--user USER`, `--group GROUP` → collect them; the request will run through
     real sudo as the interpreter (step 5). `-u`/`-g`/`--user`/`--group` in the separated form
     consume the following token as their argument.
   - any other token starting with `-` → PassThrough with the raw argv. That includes sudo's
     unambiguous long-option abbreviations (`--us=ff` for `--user=ff`, `--ed` for `--edit`);
     matching is on exact spellings only, because modelling getopt_long's abbreviation rules would
     mean tracking sudo's whole option table.
   - anything else → command token; stop scanning. The remaining argv is never re-interpreted.
3. No command token → Gate iff `-s` or `-i` was collected, else PassThrough (real sudo prints its
   own usage error).
4. Gate, plain request: exec
   `/usr/bin/sudo /usr/local/bin/sudo-prompt -- [NAME=value ...] COMMAND ARGS...`. Assignments are
   deduplicated by name, last occurrence winning, and are applied by the gate itself after
   approval (see the gate contract) — there is no `env(1)` in the chain. Using env would mean the
   prompt's command field named `/usr/bin/env` instead of the real command, and would break on a
   command token containing `=` or starting with `-`.
5. Gate, interpreter request (whitelist flags were collected): exec
   `/usr/bin/sudo /usr/local/bin/sudo-prompt -- /usr/bin/sudo RAWARGV...`, where RAWARGV is the
   original argv from the first *collected* token on — flag or assignment — byte-for-byte, with
   exactly one substitution: each `--preserve-env=LIST` token is replaced **in place** by the
   `NAME=value` assignments it expanded to in step 2 (by zero tokens if nothing in LIST was set).
   No other token is altered and no order is rewritten. Starting RAWARGV at the first collected
   token rather than the first flag matters: `sudo FOO=bar -u ff cmd` would otherwise drop
   `FOO=bar` entirely.

   The gate's own assignment list is therefore always empty on this path: every assignment, whether
   the caller wrote it or `--preserve-env` produced it, is a command-line variable for the inner
   sudo, which is the only place it can be and still survive that sudo's env_reset. Three reasons
   this beats forwarding the flag: the displayed argv is exactly what runs, the gate's whitelist
   stays a short list of option spellings that already includes `NAME=value`, and nothing has to be
   applied twice in two different environments. It works because root's own rule matches ALL, and
   sudoers(5) implies SETENV for a rule matching ALL, so the inner sudo accepts command-line
   variables — see the verify list, and do not narrow root's rule without adding SETENV. The
   substitution happens where the flag stood, in the options region, which is where sudo(8)
   documents `VAR=value` as belonging; a caller who writes an assignment *before* the flags gets an
   argv the inner sudo mis-parses, failing exactly as stock sudo would have failed.

Step 2's pass-through of unknown flags is safe by construction: informational flags (`-l`, `-ll`,
`-V`, `-v`, `-k`, `-K`, `-h`, `--help`, `--version`, `--list[=…]`, `--validate`,
`--reset-timestamp`, `--remove-timestamp`, and clusters of the short forms) work normally against
real sudo, while anything carrying a command is denied because no sudoers rule permits it. No-op
invocations never raise a prompt.

Environment values never become the gate's environment. The shim never passes -E and the gate's
sudoers rule grants no SETENV, so the caller's environment cannot reach the gate's own environment
at all. That is structural, not a check we perform: the dynamic loader acts on LD_PRELOAD and
friends before any gate code could scrub them. Assignments and expanded `--preserve-env` values
travel as argv data and are applied to the *command's* environment at exec, after approval, never to
the gate's.

They are, however, applied without filtering, which is a deliberate and load-bearing difference
from stock sudo. sudoers(5) allows command-line variables only under a SETENV tag (implied for a
rule matching ALL, which is why the interpreter path's inner sudo takes them, but not granted on the
gate's own narrow rule); the gate applies them itself instead, and unlike the env_check/env_delete
path it does not drop loader-dangerous names. `sudo --preserve-env=LD_PRELOAD cmd` therefore works
and runs `cmd` as root with the caller's LD_PRELOAD. The mitigation is disclosure, not filtering:
every name and value is rendered in the prompt's own Environment field before anything can act on
it (see Command rendering), so approving a request that carries LD_PRELOAD is a decision the human
made. A `PATH=` assignment is part of that: it overrides the captured secure_path for command
resolution and is displayed like any other.

Every shim path exec()s in place, so exit status and signals pass through untouched, and argv is
forwarded as raw bytes with no re-quoting or lossy string conversion. If a shim exec itself fails,
print the errno and exit 125.

Known and documented behavior differences:

- The command's environment is constructed, not inherited (see the gate's Environment section), so
  variables stock sudo would have preserved are gone unless the request names them: `sudo gui-app`
  no longer inherits DISPLAY/XAUTHORITY, and HOME is root's rather than the caller's. Name them
  (`--preserve-env=DISPLAY,XAUTHORITY`, or an explicit `HOME=…`) and they work and are displayed.
- `--preserve-env` values appear in the prompt, in sudo's command logging, and in
  /proc/PID/cmdline for the life of the prompt. The caller chose those names explicitly; never
  preserve secret-bearing variables. `--preserve-env=SUDO_*` is rejected by the gate rather than
  silently overridden.
- Beyond plain commands, the shim supports `-u`/`-g` (including the `--user=`/`--group=` and
  `--user `/`--group ` spellings) and `-i`/`-s` by routing through /usr/bin/sudo as the
  interpreter, shown as such in the prompt. Unsupported: `-e`/`--edit`/sudoedit (no path),
  `-E`/bare `--preserve-env`, `-n`, `-b`, `--chdir`, compact forms like `-uff`, long-option
  abbreviations like `--us=ff`, and any other flag. These pass through to real sudo, which denies
  command execution. `--chdir` is excluded deliberately: it would require a CWD=* tag in root's own
  sudoers rule.
- In `-u` commands, SUDO_*, HOME and SHELL describe root's inner sudo invocation — exactly what
  stock sudo produces when root runs `sudo -u`. `--preserve-env` combined with interpreter flags
  becomes plain command-line variables for the inner sudo (step 5), so it is handled once, in the
  place that actually determines the command's environment.
- Mixed informational+command invocations that stock sudo allows (`sudo -k cmd`) now fail.
- A command that itself exits 125 is indistinguishable from gate denial/error; stderr
  disambiguates (see the gate contract).

Install and validate a command-argument constrained sudoers drop-in:

~~~
# /etc/sudoers.d/sudo-prompt — the human's login uid(s) and ai, nobody else.
%sudo-prompt-users ALL=(root) NOPASSWD: /usr/local/bin/sudo-prompt -- *
~~~

One pattern, because every gate invocation — plain or interpreter — starts with `--`. sudoers joins
the user's arguments with spaces and fnmatches the pattern against the result, and `*` matches
spaces, so `--` followed by at least one argument matches. Note the pattern does *not* match a bare
`sudo-prompt --`: fnmatch needs the literal space, so an empty request is refused by sudo before
the gate sees it. The gate's own empty-argv rejection is therefore a second line, not the first —
keep it anyway, since direct invocation is a supported path and the rule may change. Validate with
visudo -cf on the target version, and test that `sudo-prompt -- id` matches while bare
`sudo-prompt id` and bare `sudo-prompt --` do not. A
malformed line here fails open into "no rule at all", which under this design means nobody can
sudo, so check it before logging out. Do not use a bare command path, which permits arbitrary gate
arguments. SETENV is not needed and must not be granted.

The rule's safety depends on path integrity: the installer must verify that /usr/local,
/usr/local/bin, the shim, and the gate are root-owned and not group- or other-writable, and refuse
to install otherwise.

## sudo-prompt contract

The complete production interface is:

~~~
sudo-prompt -- [NAME=value ...] COMMAND [ARGS...]
~~~

Leading tokens matching `[A-Za-z_][A-Za-z0-9_]*=` are environment assignments for the command; the
first token that does not match is COMMAND and nothing after it is re-interpreted. That is sudo's
own rule, so `./a=b` is a command and not a malformed assignment. Assignments are deduplicated by
name, last occurrence winning.

Reject a missing delimiter, an empty argv, an empty COMMAND, an argv consisting only of
assignments, an assignment whose NAME begins with `SUDO_`, a COMMAND whose basename is `sudoedit`,
and any token before --. The binary never has --title, --body, --detail, --icon, --display,
--runtime-dir, --gtk-theme, --settle, --dim, --lock-file, --env-file, --surface, --preserve-env,
or --verbose.

Exit status: on approval the gate exec()s, so the command's own status or signal is the result.
Denial exits 125 with exactly `User denied sudo :(` on stderr. Any operational error exits 125
with a specific message naming the failed check.

Compile conservative fixed values: heading, backdrop appearance, settle duration and settle cap, the
passthrough variable list, lock path /run/sudo-prompt.lock, and display root /run/user/0. The
release binary requires euid 0; nested-compositor tests exercise the UI crate or a test-only build.

### Environment

Two environments matter and neither is inherited wholesale: the gate's own, which must be
root-controlled so nothing the caller said can steer GTK, and the approved command's, which must
contain nothing the human was not shown.

The gate's own environment is whatever sudo's env_reset policy produced, which still carries
caller-influenced survivors such as HOME, TERM, DISPLAY, LANG, and LS_COLORS. Before GTK
initialization or threads:

1. Capture what is needed later: `PATH` (sudo set it from secure_path), the provenance variables
   `SUDO_UID`/`SUDO_GID`, and the passthrough candidates listed below. Not the whole environment.
2. Clear the live environment.
3. Set only root-controlled GTK prerequisites: HOME=/root, a fixed system PATH, and known system
   data directories. Set the trusted display variable, and XDG_RUNTIME_DIR=/run/user/0, only after
   display selection has validated that directory. Set XDG_RUNTIME_DIR explicitly rather than
   leaving it unset: GLib otherwise falls back to a cache-directory runtime path, and GTK, dconf
   and the cursor cache all land somewhere unintended.
4. Initialize GTK and take the session lock from this clean state.

Never configure root GTK from caller GTK_THEME, GTK_MODULES, GIO_MODULE_DIR, XDG_DATA_DIRS, runtime,
or display variables.

#### The command's environment is constructed, not inherited

sudo cannot hand us a clean environment, so do not ask it to. env_reset keeps everything the
env_keep/env_check lists name (LANG, LC_*, DISPLAY, XAUTHORITY, LS_COLORS, … under a stock Arch
sudoers) *plus* a hardcoded TERM/PATH/HOME/MAIL/SHELL/LOGNAME/USER set that no sudoers setting
removes, and HOME and TERM in that set are the caller's values. A per-command
`Defaults!<Cmnd_Alias> !env_keep, !env_check` would empty the lists — the boolean form disables a
list parameter, and per-command Defaults need a Cmnd_Alias because they may not carry arguments —
but not that hardcoded set, and it would leave the gate depending on sudoers for a property it can
guarantee itself. So the gate ignores its inherited environment as a source and builds the command's
environment from exactly three things:

1. A root-controlled base: `PATH` = the captured secure_path, `HOME=/root`, `USER=root`,
   `LOGNAME=root`, `SHELL` = root's passwd shell. None of it is caller data.
2. A short passthrough list — the only variables copied out of the inherited environment: `TERM`,
   `COLORTERM`, `LANG`, `LANGUAGE`, `LC_*`. Each is validated (bounded length, conservative
   charset) and a value failing validation is dropped, never sanitized. These are caller data, so
   each survivor is displayed in the prompt's Environment field exactly like an assignment, and a
   drop is noted there too. TERM is on the list because without it every interactive root command
   is unusable; its loader-dangerous relatives TERMINFO, TERMCAP and TERMPATH are not.
3. The request's assignments, applied last so they override both.

Then the gate sets `SUDO_UID`, `SUDO_GID`, `SUDO_USER` and `SUDO_COMMAND` itself, after the
assignments, so provenance cannot be rewritten by the request; requests naming `SUDO_*` are rejected
outright rather than displayed as an assignment that will not take effect.

On the interpreter path this is the environment the *inner* sudo starts from, not the one the final
command gets: that sudo runs its own env_reset over it, so a passthrough variable the host's
env_keep does not name may be dropped before the command sees it, and it rewrites HOME, SHELL and
SUDO_* to describe root's invocation. Both are the safe direction — the field can only over-promise
about what survives, never under-promise — and neither is something the gate should try to defeat.

Everything else the caller had is gone, which is a deliberate, documented departure from stock sudo:
it is what makes the Environment field a *complete* account of the caller-controlled data entering
the root command rather than an almost-complete one. Absent a `PATH=` assignment the command
resolves against the captured secure_path.

Build this environment as an owned `Vec<CString>` and pass it to `execve` (see Command rendering and
execution). Never write it into the live `environ`.

Parse SUDO_UID as a nonzero numeric uid and resolve its name through passwd. Reject missing or
inconsistent provenance rather than trusting SUDO_USER.

### Display selection

Ignore caller XDG_RUNTIME_DIR and WAYLAND_DISPLAY; there is no --auto-display.

Before GTK initialization, inspect only /run/user/0:

1. Require a root-owned directory that is not group/other writable.
2. Enumerate immediate wayland-N entries, where N is a decimal number. Reject symlinks and require
   a root-owned Unix socket with safe parent ownership and permissions.
3. Take the lowest N, connect, and confirm via SO_PEERCRED that the peer is uid 0. Any root-owned
   compositor there is acceptable. Do not probe other displays: a stale socket from a dead
   compositor fails the gate until removed from a TTY, which is accepted deliberately.
4. Prefer keeping the validated connection: clear FD_CLOEXEC on the connected fd and set
   WAYLAND_SOCKET=<fd>, so libwayland uses it directly and no second lookup happens. If GTK
   interop makes that awkward, fall back to setting WAYLAND_DISPLAY=<name>; that is safe only
   because /run/user/0 is drwx-----x root:root so no non-root uid can replace the entry between
   check and connect. Write that dependency down next to the code either way.
5. After connecting, require `ext-session-lock-v1` — `gtk4_session_lock::is_supported()`, which
   costs a Wayland roundtrip on first call, so do it once and keep the answer. Missing session lock
   is an error; the gate does not fall back to layer shell or a toplevel.

The connection must not survive the approval exec. Step 4 deliberately clears FD_CLOEXEC and
libwayland does not necessarily restore it, so the approved command would otherwise inherit an open,
root-authenticated connection to root's compositor — and wlbouncer filters globals per uid at
connect time, so an inherited root connection carries virtual keyboard, virtual pointer, screencopy,
layer shell and the session lock manager into a command that may be running as `ff` or `play`. That
is exactly the capability
set the sandbox policy denies them. Immediately before the exec, re-set FD_CLOEXEC on the Wayland fd
(and close the display outright when GTK allows it), then walk /proc/self/fd and close or
CLOEXEC everything above fd 2 — GLib, dconf and GIO hold fds of their own, so "assert nothing above
2 is open" is not a property that will hold; closing them is. Do not rely on the interpreter path's
inner sudo doing `closefrom` for you; it does, but only there, and only until someone grants
closefrom_override.

On failure, exit 125 with a specific message, without an xdg-toplevel fallback.

### Command rendering and execution

Treat every argv byte as untrusted display data, and the cwd and the assignments likewise. Render
one shell-quoted token per argument in a monospace view. Escape unambiguously, and flag that
escaping occurred:

- C0/C1 control characters.
- Bytes that are not valid UTF-8 — argv is arbitrary bytes, and a lossy conversion is not acceptable
  here because it renders distinct requests identically. Escape each offending byte as `\xNN`; the
  same goes for overlong encodings and encoded surrogates.
- Unicode format characters (Cf: bidi controls, zero-width, joiners) and unassigned codepoints.
- Characters that move text around even with markup off: the line/paragraph separators U+2028 and
  U+2029, which Pango treats as mandatory breaks and which would otherwise let a caller inject line
  breaks into a single-line field, and non-ASCII spaces (Zs, notably U+00A0) which can fake
  alignment.

Leave ordinary printable non-ASCII (café.txt) readable so legitimate paths stay legible and the
escape flag stays meaningful. Never log raw terminal control bytes, join argv into a shell-looking
string, or invoke a shell.

The cwd is read with getcwd at startup, before the environment is touched. If it fails — a deleted
or unreachable directory — render the field as an escaped trusted "(unavailable)" and carry on; a
caller whose cwd vanished should still be able to run sudo.

The command's environment gets its own field, one `NAME=value` per line with the same escaping,
never merged into the command field, always shown. It lists every caller-controlled entry — the
request's assignments and the surviving passthrough variables — and notes any passthrough variable
dropped for failing validation. The root-controlled base (PATH from secure_path, HOME, USER,
LOGNAME, SHELL, and the gate's own SUDO_*) is not caller data and is summarised as a single trusted
line rather than enumerated. Because the environment is constructed rather than inherited, this
field is a complete account of what the caller put into the root command's environment, and it is
the only thing standing between an approving human and a root command carrying the caller's
LD_PRELOAD. On the interpreter path it carries no assignments at all: they are command-line
variables for the inner sudo and appear inside the displayed argv, which is what actually happens to
them.

Display argv exactly as requested. Do not resolve COMMAND and do not display a resolved path:
resolution happens at exec time against the captured root PATH (or a displayed `PATH=` assignment),
so `ls` is the normal `ls`. Displaying a resolved path would promise an inode identity the gate
cannot hold across the approval window, and a relative path means the user is approving whatever
that path holds at exec time — the same contract stock sudo offers.

#### Rendering safety

Escaping bytes is not enough on its own: GTK will happily interpret caller text as Pango markup or
as a mnemonic, which would let a request restyle the dialog, hide text with a matching foreground
colour, or forge the trusted heading inside the trusted surface. Make that structurally impossible
rather than remembering not to do it:

- Binaries never touch `gtk::Label` (or any other text widget) directly. The UI crate owns every
  widget that can display text and is the only place those APIs are called.
- Untrusted bytes are carried in an `Untrusted` newtype from the moment they are read out of argv.
  It implements neither `Display` nor `Deref<Target = str>`, so it cannot be interpolated into a
  format string, concatenated with trusted prose, or passed anywhere expecting `&str`. The single
  way to get it on screen is one UI-crate function that escapes it and calls `set_text`.
- That function, and every other label constructor in the crate, sets `use_markup = false` and
  `use_underline = false` explicitly, and sets accessible descriptions from the same escaped text.
  No `set_markup` call, no `<property name="use-markup">` in any builder XML.
- Nothing the human must read may live in a popup, menu or tooltip: `ext-session-lock-v1` allows one
  surface per output, so GTK popups silently do not appear in session-lock mode. Everything is in
  the one window, which the scrolling overflow-marked viewports below already assume.
- Trusted fields — heading, uid/name, interpreter, verdict, buttons — are compiled-in constants
  rendered by their own functions, never concatenated with `Untrusted` content and never adjacent
  to it in a way that lets caller text appear to be part of them.
- A test walks the workspace sources and fails on `set_markup`, `use_markup`, `set_use_underline`,
  and `markup=` outside the one audited module. It is a backstop for the encapsulation, not the
  mechanism.

Any field whose content the caller controls — argv, cwd, environment, and in the generic binary
the body and details — lives in a scrolling viewport with a bounded maximum height. The trusted
fields and the buttons sit outside every viewport and are always visible, so no amount of caller
text can push them off screen or move them. When a viewport has content beyond its visible area,
say so: a permanently visible (non-overlay) scrollbar plus an explicit "N more lines" marker, so
"there is more to read" is never something the human has to discover. Very large requests still get
a hard ceiling on total rendered bytes and token count — enough that no realistic command hits it —
with an unmissable truncation marker and the full text in the log record; scrolling does not help
if GTK is busy laying out a megabyte of argv.

The dialog must fit any output it lands on, including a small one with root's text-scaling turned
up. Constrain the whole dialog to the output's size and let the viewports be what shrinks — down to
nothing if it comes to that, since a scrollable field two lines tall is still readable while a
clipped button is not. The trusted fields and buttons keep their natural size and are the last thing
to give.

#### Interpreter requests

When COMMAND's basename is `sudo`, the request runs a second sudo as root, so the gate — not the
shim — decides what that inner sudo may be asked to do. Scan the tokens after COMMAND with a
whitelist and deny, without prompting, on anything unrecognised:

- exactly `-i`, `-s`, `-u USER`, `-g GROUP`, `--user=USER`, `--group=GROUP`, `--user USER`,
  `--group GROUP`; valid `NAME=value` assignments; and `--`, which ends the scan.
- the first token that is none of those and does not start with `-` is the inner command; stop.
- anything else starting with `-` is a denial. That includes `--preserve-env` in every spelling: the
  shim expands it into assignments and never forwards the flag, so a request carrying it did not
  come from the shim.

A blacklist does not work here. Direct invocation of the gate is a supported, sudoers-permitted
path, and sudo accepts unambiguous long-option abbreviations, so a scan looking for `-e`/`--edit`
misses `--ed`, `--edi` and `--e` — all of which mean `--edit`, and which land on an editor running
as root via sudoers' `editor` default. The whitelist denies those and every future option we have
not thought about, at the cost of denying spellings the shim never produces anyway. It bounds what
the *shim's* generated requests can be and keeps a directly-invoked request readable; it does not
and cannot stop an approved root command from reaching an editor by some other route (see the
authority model).

Mark the request as an interpreter request with fixed compiled-in prose in a trusted field, and pick
the prose from the whitelist scan's own result, not from the request text. When `-i`/`-s` is present
the interactive-shell warning always applies — combined with the `-u`/`-g` wording when both appear,
since `sudo -u ff -i` is a shell as another user and still one that never prompts again:

- `-u`/`-g` with an inner command: "this runs another sudo, as root, to run a command as another
  user".
- `-i` or `-s` with no further tokens: "this grants an interactive root shell — it will not prompt
  again for anything it then runs". That is a much larger grant than one command and the prompt has
  to say so, since inside that shell euid is 0, the shim passes straight through, and root's own
  sudoers rule needs no approval.
- `-i` or `-s` *with* trailing tokens: additionally "the arguments below are joined and interpreted
  by a shell, not run as a command". sudo joins them into one `-c` string, so shell metacharacters
  in them are code. The tokens are still rendered one shell-quoted token per line like any other
  argv, and that rendering would otherwise imply they are argv words.

Leave the path itself in the command field with the rest of the argv, where it is escaped like any
other caller data. The trusted field must not be built out of the request. Do not claim to resolve
the eventual target.

There is exactly one execution path, and it is `execve` with an explicit environment and a
command path the gate resolved itself. After approval:

1. Build the final environment as described under Environment, including SUDO_COMMAND set to the raw
   space-joined command line without the assignments (stock sudo's format, not the escaped display
   form), as an owned `Vec<CString>`.
2. Resolve COMMAND: if it contains a `/`, use it verbatim. Otherwise search the PATH *from that
   final environment* — the `PATH=` assignment if the request carried one, else secure_path —
   trying each candidate and continuing past ENOENT/ENOTDIR, remembering an EACCES to report if
   nothing else works. Skip empty path elements rather than treating them as the current directory,
   matching sudo's `ignore_dot` default. There is no ENOEXEC fallback to a shell: a file with no
   shebang is an error, because this design never invokes a shell.
3. Unlock the session and wait for the compositor to acknowledge it (see Session lock mode) —
   before, not after, the connection is torn down.
4. Re-arm FD_CLOEXEC on the Wayland fd, close everything above fd 2, and `execve`.

Do not use `execvp`/`execvpe` and do not write the final environment into `environ`. `execvp`
resolves against the live `environ`, so it would require rewriting `environ` at approval time —
after GTK init, with GLib worker threads running, where `clearenv`/`setenv` race any concurrent
`getenv`. `execvpe` avoids that but resolves PATH from the *caller's* `environ` and not from the
`envp` it passes on (verified against this host's glibc), which with a scrubbed gate environment
would silently resolve the command against the gate's own hardcoded PATH and ignore both secure_path
and any `PATH=` assignment. Resolving in the gate and calling `execve` is the only combination that
gets the displayed environment and the documented resolution behaviour at once. The source lint
should flag `set_var`/`remove_var`/`clearenv` outside the pre-GTK setup module.

Execution failure is an error (exit 125 with the errno), not approval.

## UI behavior

The gate pins the UI crate's `SessionLock` surface mode: `ext-session-lock-v1` surfaces covering
every output, never a layer surface and never an xdg toplevel, with no fallback.

- Exactly one window per output, handed to the compositor with `assign_window_to_monitor` before it
  is presented. Every window carries an identical dialog instance over an opaque compiled-in
  backdrop — there is no pointer/focus output selection and no reparenting. Nothing is dimmed,
  because a lock surface replaces the desktop rather than sitting over it; that is also why the
  backdrop is opaque rather than translucent.
- The compositor routes all input to lock surfaces, so there is no exclusive-keyboard request to
  make and no input region to set. Whichever window the compositor focuses drives the shared
  decision, as do the buttons on any of them. There are no focusable GTK widgets, no default action,
  and no IM context, so nothing but the state machine below can turn a keystroke into an activation.
- Settling is a quiet period, not a fixed window. Ignore all input and visibly disable the controls
  until the settle duration has passed with no key press, key release or pointer button event; each
  of those restarts the timer. Start it from the first frame callback — the moment a surface is
  actually presented, not when it was created — and restart it whenever any surface presents for
  the first time (a hotplugged or late-waking output must never show an already-live prompt) or
  (re)gains keyboard focus. A fixed window from mapping is not enough: a fast typist mid-sentence
  hits Enter well inside a second and approves something they never read; if the outputs are
  DPMS-blanked the whole window can elapse while nothing is visible, so the very keypress that
  wakes the display approves an invisible prompt; and a prompt whose focus the compositor moved
  away and back must not be live the instant it returns.
- Two deliberate details in that rule. Pointer *motion* does not restart the timer, only buttons: a
  drifting mouse, a resting touchpad finger or a VM absolute pointer emits motion continuously, and
  with no prompt timeout a motion-sensitive quiet period would mean a prompt that can never settle
  and therefore a system where sudo never works. And the settling wait is capped at a small multiple
  of the settle duration, counted from when the current quiet period began; on reaching the cap the
  gate **denies** and exits 125 with its own message, rather than enabling the controls. The cap
  restarts only with the non-input restarts (focus regain, a newly presenting surface), never on an
  input event — that is what lets a prompt on an output that wakes late, or one the compositor
  refocused, survive to be answered, while a stuck key stays bounded. Enabling the controls at the
  cap would hand approval to whatever is generating the input; denying fails closed, and costs a
  genuine fast typist nothing but a retry. This is a cap on *settling*, not on deliberation: once
  settled the prompt waits indefinitely for an answer.
- Approval requires a physical key press: the first real key *down* — never a synthesized
  autorepeat — on Enter (`Return`, `KP_Enter`, `ISO_Enter`) while settled approves, and `Escape`
  denies. wl_keyboard delivers only physical press/release; repeats are synthesized client-side (in
  GTK's Wayland backend, with no is-repeat flag on events), so telling a fresh down from a repeat is
  the implementor's problem — held-keycode tracking with focus-leave resets is the expected shape,
  but the contract is the sentences above. Input delivered before settling ends is swallowed at the
  window level and never reaches a control, and pointer approval requires a press *delivered after*
  settling ends: a press that began before settling and is held across the boundary does nothing,
  even if its release lands on Approve.
- Handle hotplug through the instance's `monitor` signal, which fires for every monitor present when
  the lock is taken and for each one that appears while it is held: assign a fresh window to each,
  and drop the window for a monitor that goes away. If no monitors remain, deny and exit; likewise
  deny if there were none to begin with, rather than holding the session lock and the gate's flock
  with nothing on screen.
- Keep the fixed trusted heading, requesting uid/name, requested command, command environment, and
  cwd as separate fields. Caller-controlled text never occupies the trusted verdict field, and the
  uid/name field never sits where caller text could be read as part of it — with `ai` in the group,
  that field is what distinguishes an agent's request from the human's own.

### Session lock mode

Use gtk4-session-lock: `is_supported()` during display selection, then `Instance::new()`, `lock()`,
one window per monitor from the `monitor` signal, and `unlock()` when the decision is made. Keep the
instance alive for the whole run.

- **This is not a lock screen.** The gate takes no password and authenticates nobody; it uses the
  protocol purely for exclusivity — a surface no other client can cover, that receives all input.
  Either verdict unlocks. The prompt must therefore not look like a lock screen: anyone who walks up
  can press Escape and land on the unlocked desktop, and the session is no more protected while the
  prompt is up than it was a moment before. Nothing here replaces or interacts with the real lock.
- Order: flock, then display selection, then `lock()`, then windows. Present nothing before the
  compositor confirms with `locked` — a lock surface exists only inside the lock.
- `failed` is a denial, not a fallback trigger: log it and exit 125. Its normal cause is another
  client already holding the lock, i.e. the screen is locked. Same for the compositor withdrawing
  the lock under us.
- Every survivable exit path unlocks *and waits for the compositor to process the unlock* (a
  roundtrip, not just a flush) before tearing anything down: approval before the exec, denial, the
  settle cap, the SIGINT/SIGTERM/SIGHUP handlers, and every operational error after `lock()`
  succeeded. Dropping the connection while locked is precisely how the compositor learns a lock
  client died, and it keeps the session locked with nothing on it. The exec path is the sharp edge —
  it closes every fd by design, so the unlock must have completed, not merely been queued.
- Install a panic hook that unlocks and roundtrips, and do not build with `panic = "abort"`. A panic
  in a GTK callback would otherwise strand the session.
- What remains: SIGKILL, the OOM killer, a real crash. Those leave the session locked with no
  client, and no in-process design fixes that. Recovery is the root TTY this design already requires
  — from there restart the compositor, or run another lock client to take over and unlock. Establish
  which of those works on this host's sway *before* rollout and write it into the recovery docs. The
  same risk is the reason to keep the code running between `lock()` and `unlock()` small.
- No downgrade to layer shell when the protocol is missing. A silent downgrade would hand back the
  exact spoofing exposure session lock was chosen to remove, at the one moment nobody is watching
  for it. Compositor support is a static property of the host, checked at install.

Acquire the flock before taking the session lock. /run is tmpfs, so the file will not exist after a
boot: open it with O_CREAT|O_NOFOLLOW|O_CLOEXEC and mode 0600, then fstat and require root
ownership, a regular file, and no group/other write. Creating it is safe without an installer or a
tmpfiles.d entry because /run itself is root-owned and only root-writable, so no other uid can win
the race or plant a symlink. Take a non-blocking exclusive flock. A concurrent request fails closed;
do not queue prompts, accept an alternate lock path, or continue without it. flock lives on the open
file description, so the kernel releases it on any exit including SIGKILL — no PID file or
stale-lock recovery is needed (unlike the session lock, which is exactly the opposite). Do not block
or ignore SIGINT, SIGTERM or SIGHUP; treat all three as denial (SIGHUP is the normal outcome of the
requesting terminal going away while the prompt is up), and keep the fd CLOEXEC so an approved
long-running command does not hold the flock.

Log one escaped record per decision — requesting uid/name, display identity, rendered command, and
approve/deny/error — to stderr via env_logger, and additionally best-effort to the systemd journal
(datagram to /run/systemd/journal/socket), silently skipped when the socket is absent. One line,
no chatter. sudo's own logging already records the gate invocation; the gate's record is the
decision.

## Generic permission-prompt

The generic binary may accept title, body, repeatable details, icon, normal session-display
selection, and `--surface=auto|session-lock|layer|toplevel`, default auto: session lock, then layer
shell, then an xdg toplevel. It exits 0 approved, 1 denied, 3 operational error (it is not a sudo
wrapper, so no exit-status collision exists); it has no -- ARGV execution mode and no sudoers rule.

In practice auto lands on a toplevel for most uids, because wlbouncer denies both the session lock
manager and layer shell to them — the ordering matters for the uids that do hold those globals, and
today that is root. All the session-lock rules above apply to it unchanged, and two of them bite
harder here: it must unlock on every exit path, and a crash strands the session locked. So it gets
the same panic hook and the same signal handling as the gate, and a caller that would rather have a
plain window than that failure mode should pass `--surface=toplevel`. Being unprivileged, it is also
the binary most likely to be run where it cannot lock at all; `failed` there falls through to the
next mode rather than exiting, since it is not a security boundary and has nothing to fail closed
about.

It must visually quarantine caller prose and escape unsafe text through the same `Untrusted` path
and the same scrolling, overflow-marked viewports as the gate, but it must not claim to be a
trusted sudo/root prompt, and should be visually distinct from the gate — and, in session-lock mode,
from a lock screen. It can link the same Rust UI code, but the sudo gate must not delegate to it
over argv, environment, socket, or config file.

## Verify on the target host

Check before relying on any of it, and check the sudo items again if the host switches to sudo-rs,
which supports a subset of sudoers:

- The command-argument wildcard rule parses and matches as intended: `sudo-prompt -- id` matches,
  and both bare `sudo-prompt id` and bare `sudo-prompt --` match nothing.
- Every group member, `ai` included, can reach the gate, and the resulting prompt names the right
  uid. Agent sudo is a supported flow, so "the agent's sudo works and is attributable" is a
  verification item, not an accident.
- `sudo -n` for a group member is denied immediately without a password prompt or a gate prompt,
  so non-interactive callers fail fast instead of parking on the lock.
- secure_path is set, so the captured PATH the gate execs against is root-controlled.
- use_pty is on, so the approved command does not share the caller's terminal, where another
  process of the caller's uid could inject input into it via TIOCSTI. Check
  dev.tty.legacy_tiocsti=0 as a second layer.
- wlbouncer policy still denies virtual-keyboard, input-method and virtual-pointer globals to every
  non-root uid: the approval guarantee assumes no untrusted uid can synthesize input to the
  prompt.
- wlbouncer denies `ext_session_lock_manager_v1` to every non-root uid — it is denied by default,
  but confirm it is not in the base set and is not granted alongside layer shell to
  `ff`/`comms`/`code`. A non-root uid holding it can lock the session before the gate does, which
  both blocks all sudo and puts an uncoverable fake prompt on screen.
- The compositor implements `ext-session-lock-v1` (sway does) and
  `gtk4_session_lock::is_supported()` returns true as root on this host. The gate has no fallback,
  so this is a hard install-time requirement, and gtk4-layer-shell must be ≥ 1.1.0 for the binding
  to exist at all.
- Recovery from an abandoned lock is known and written down: kill a locked-but-clientless session
  from the root TTY by restarting the compositor, or by running another lock client that takes over
  and unlocks. Test it deliberately (SIGKILL the gate mid-prompt) before rollout, not during an
  incident.
- SUDO_UID is set by sudo and reflects the real caller.
- A `root ALL=(ALL:ALL) ALL` style rule exists, so the interpreter path's inner sudo (running as
  uid 0) can run commands as other users. sudo never authenticates uid 0, so no NOPASSWD is needed
  there.
- That same rule carries SETENV implicitly, because sudoers(5) implies SETENV for a rule matching
  ALL. The interpreter path depends on it: the shim turns `--preserve-env` into command-line
  variables for the inner sudo, and without SETENV that sudo refuses them. Confirm with `sudo -l`
  and a live `sudo -u <other> FOO=bar env`. If root's rule is ever narrowed to specific commands,
  it needs an explicit SETENV tag. The gate's own rule needs no SETENV and must not be granted one.
- `ignore_dot` is at its default (on), matching the gate's own PATH resolution, which skips empty
  path elements rather than searching the current directory.

## Milestones

1. **Build the fixed gate.** Display selection and fd hygiene, the constructed command environment
   and assignments, safe non-blocking flock, the session lock with per-monitor dialogs and its
   unlock-on-every-path discipline, quiet-period settling and the key state machine, hotplug, the
   `Untrusted` rendering path with scrolling overflow-marked fields, the interpreter whitelist, the
   single resolve-and-execve path, exit statuses, and logging.
2. **Integrate sudo.** sudo-shim binary, narrow sudoers group/drop-in, installer path-integrity
   checks, recovery docs, and the verification list above.
3. **Add the generic presenter.** Keep it unprivileged and execution-free.

## Testing and rollout

- Unit: argv/cwd byte escaping (printable non-ASCII left intact; invalid UTF-8, lone surrogates and
  overlong encodings escaped per byte rather than replaced; U+2028/U+2029 and U+00A0 escaped),
  gate CLI parsing and rejection (missing --, empty argv, empty COMMAND, assignments only, `./a=b`
  treated as a command and not an assignment, `SUDO_*` assignment, sudoedit basename, tokens before
  --), the interpreter whitelist (each accepted spelling; denial of `--ed`/`--edi`/`--e`, `-e`,
  clusters containing `e`, `--preserve-env` in every spelling, and every unrecognised flag),
  environment construction (base values, passthrough validation and dropping, assignment override
  order, SUDO_* applied last and unspoofable, nothing else from the inherited environment present),
  command resolution (absolute, relative, PATH search order, `PATH=` assignment honoured, empty path
  elements skipped, ENOENT vs EACCES reporting, no shell fallback), lock validation, SUDO_UID
  provenance, and display candidate ordering.
- Shim: unit-test classification over euid 0, empty argv, informational flags singly and in
  clusters, plain commands, VAR=value assignments, --preserve-env (set, unset, invalid, and
  duplicate names), whitelist interpreter flags (-u USER, --user=USER, --user USER, -g GROUP,
  --group=GROUP, --group GROUP, -i, -s; -u with a missing argument; interpreter flags combined with
  assignments and --preserve-env, including an assignment *before* the first flag, which must
  survive in RAWARGV), compact forms like -uff, long-option abbreviations like --us=ff, -E and
  other unsupported flags, --, and non-UTF-8 argv. Specifically for the --preserve-env expansion:
  on the plain path the names become gate assignments; on the interpreter path they are substituted
  in place inside RAWARGV, the flag never appears in the gate argv, an expansion to zero names
  removes the token entirely, and the resulting argv is one the gate's own whitelist accepts —
  drive the real gate parser with the shim's output so the two lists cannot drift apart again.
  Separately, with the exec target pointed at a fake sudo, check the constructed gate argv
  byte-for-byte and that exit status and signals pass through.
- Confirm the command's environment contains only the constructed base, the displayed passthrough
  survivors, the displayed assignments and the gate's SUDO_*: approve `/usr/bin/env` with a caller
  environment stuffed with DISPLAY, LS_COLORS, XAUTHORITY, HOME and a bogus LC_ALL, and diff the
  result against the expectation. The source lint additionally fails on `set_var`/`remove_var`/
  `clearenv` outside the pre-GTK setup module.
- Rendering safety: the source lint over the workspace; a request whose argv contains Pango markup,
  a fake heading, `_` mnemonics, and a colour span renders as literal text; a request with hundreds
  of tokens and one multi-megabyte token leaves the heading, uid field and buttons in place, scrolls
  rather than grows, shows the overflow marker, and truncates visibly at the ceiling.
- Disposable real-sudo integration: sudoers argv matching (valid argv, bare invocation, empty
  argv), SUDO_UID, plain command round trip, a --preserve-env round trip on both paths (plain: the
  value shows in the prompt's environment field and reaches the command; interpreter: it shows as a
  command-line variable in the displayed argv, the inner sudo accepts it, and it reaches the final
  command), an assignment round trip proving the command sees it and the gate
  never did, an interpreter round trip (`sudo -u <other> id`) with the interpreter identified in
  the prompt, denial of -e and its abbreviations (via the shim, via an explicit `sudo -e`, and via
  direct `sudo-prompt -- /usr/bin/sudo --ed …`), -i/-s smoke tests including Ctrl-C and window
  resize through the nested use_pty layers, denial/error exit 125 with the right stderr messages,
  and use_pty behavior (Ctrl-C and window resize through a full-screen command such as less).
- Approved commands inherit nothing they shouldn't: approve `/usr/bin/ls -l /proc/self/fd` on both
  the plain and interpreter paths and confirm only 0/1/2 (plus the pty) are open — specifically no
  Wayland fd — and that WAYLAND_SOCKET/WAYLAND_DISPLAY are absent from the command's environment.
- GUI test-only nested compositor: backdrop+dialog on every output; early Enter, Escape, and button
  input being ignored; key/button input during settling restarting the timer while pointer motion
  does not; the settle cap denying rather than enabling the controls under continuous key input;
  the *first* post-settle Enter press approving (not the second), from any surface, and Escape
  denying; a pointer press begun during settling and released over Approve after settling not
  activating; a synthesized autorepeat run never approving; an Enter held across surface mapping
  producing no press event and no approval, with release-then-press then approving; focus
  leave/enter with the key down not producing an approval and restarting the quiet period; a newly
  presenting surface (hotplug add, late-waking output) restarting the quiet period, and the cap
  restarting on focus regain so a prompt whose focus came back survives to be answered; a small
  output and a large text scale still showing heading, uid field and buttons; hotplug add/remove,
  last-monitor loss and zero monitors at startup; concurrent flock failure;
  SIGINT/SIGTERM/SIGHUP denying and releasing both locks; wrong display ownership; stale-socket
  failure.
- Session lock, in the nested compositor: the lock is taken before anything is presented and the
  session is left *unlocked* after approve, deny, settle-cap denial, each of the three signals, and
  an induced operational error — check the compositor's state, not just the exit code, and check it
  after an approved exec rather than only after a denial, since that is the path that closes the
  connection. `lock()` failing because another client holds the lock exits 125 without prompting;
  a missing `ext-session-lock-v1` is an error for the gate with no layer-shell fallback; a panic
  injected into a GTK callback still unlocks; SIGKILL mid-prompt leaves the session locked and the
  documented recovery gets out of it. Surface modes: auto choosing session lock, then layer shell
  with the lock manager absent, then a toplevel with neither, and each `--surface=` pinning holding.
- Journal record present when systemd is running, absent and harmless otherwise.
- Rollout sequencing, in order: confirm the root password works from a TTY; install binaries and
  run the verify list, including the abandoned-lock recovery drill; create the group with the human
  uid(s) and `ai`; ship the rule as a validated /etc/sudoers.d drop-in; keep a root TTY logged in
  for the whole switch; prove the new path end to end from both a human uid and `ai`; only then
  remove the zenity/bash chain, the old %wheel rule, and the uids' now-pointless wheel membership.

## Out of scope

- Remembered decisions/timestamps, tty fallback, and any password-authenticated sudo path.
- PAM command discovery, sudo plugins, or replacing sudo uid transition.
- pkexec, which is a separate ungated root path with a normal toplevel prompt.
- Overlay-layer spoofing by layer-shell-holding uids (issues/overlay-layer-spoofing.md) and the
  wlbouncer per-layer filter that would fix it. Session-lock mode takes the gate out of its reach,
  but the layer is still spoofable for anything else that uses it.
- Being an actual lock screen. The gate takes the session lock for exclusivity only: no password, no
  authentication, no idle handling, no interaction with the real locker beyond failing when one is
  already running. Do not grow it into one.
- sudoedit/-e (no supported path), full-environment preservation (-E), --chdir, compact option forms
  like -uff, and long-option abbreviations: deliberately unsupported rather than deferred features.
- Filtering the assignments a request carries. There is no env_check/env_delete equivalent and
  LD_PRELOAD is not special-cased; the prompt's environment field is the mitigation, and a human
  who approves a request carrying a loader variable has approved exactly what it says.
