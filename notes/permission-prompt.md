# permission-prompt / sudo-prompt

Replaces the zenity/bash sudo authorization chain described in `host-sudo-setup.md`. Built; the
install steps are in the repo README, and `sudo-prompt-operations.md` has the rollout order, the
manual verification items and the recovery drill.

## What exists

~~~
permission-prompt-ui/   GTK + surface-mode primitives; internal workspace API
sudo-prompt/            the sole sudo gate (lib + bin, so tests can drive its parsers)
sudo-prompt/verify.sh   read-only check of a deployed setup (setup itself is manual, see README)
sudo-shim/              /usr/local/bin/sudo, an unprivileged dispatcher (lib + bin)
permission-prompt/      generic yes/no presenter, unprivileged, execution-free
tests/gui-test.sh       19 behavioural checks in a nested sway
~~~

## Goal and threat model

No command runs as root unless a human saw its argv on a root-owned surface and approved it.

Trusted: the kernel; root, including the compositor, which runs as root; `/usr/bin/sudo` and its
sudoers evaluation; root-owned binaries and every directory on their path; the human reading the
prompt.

Adversary: a compromised non-root uid. Members of the sudo group can invoke the gate with any argv
they like, and on this host that group is `wheel`, holding the human's login uid(s), the sandboxed
`code` uid **and the agent uid `ai`** — so a request may originate from semi-autonomous code the
human did not type. The requesting uid is a first-class trusted field for that reason. Note `wheel`
is a conventional group: `usermod -aG wheel` now grants gate access as a side effect.

Every uid can make ordinary windows (wlbouncer grants `xdg_wm_base` to all). It denies
virtual-keyboard, input-method and virtual-pointer to every non-root uid, and the gate creates no IM
context, so approval cannot be forged by synthesizing input — only by tricking the human's own
press. XWayland clients can inject input only to other X clients.

The gate presents on `ext-session-lock-v1`, which the compositor renders above every other client
and routes all input to, so the layer-shell spoofing of `issues/overlay-layer-spoofing.md` cannot
reach it. What replaces that concern: any uid able to bind `ext_session_lock_manager_v1` gets the
same exclusivity. It cannot cover a live prompt — only one lock exists, so it must lock *first*, and
then the gate fails closed — and there is no secret to phish, since the gate takes no password. But
a fake prompt plus a gate that cannot run is bad enough: wlbouncer must deny that global to every
non-root uid.

**This requires a root password and a usable console/TTY login.** The gate is the only path through
sudo: no password fallback rule, no TTY fallback in the gate. If the GUI is broken or the compositor
is gone, recovery is: log in as root and edit sudoers.

Accepted, out of scope: a compromised root; pkexec (`issues/pkexec-root-prompt.md`); denial of
service (a stuck or spammed prompt blocks all sudo and, because it holds the session lock, hides the
desktop until dismissed — recovery is a root TTY); a SIGKILLed gate leaving the session locked with
no client; what the approved command then does, including a caller-writable `sudo ./script` changing
between approval and exec.

## Why two executables

- `permission-prompt` is a generic yes/no presenter. It never executes a command, gets no sudoers
  entry, and is **not** an authorization boundary. Its caller owns its prose and reads its exit
  status. An unprivileged caller can spoof its own request, which grants nothing.
- `sudo-prompt` is the sole sudo gate, with a fixed presentation and no UI, surface, display,
  timing, lock, theme or config options at all.

The split is required: a sudoers-approved binary with prompt options would let a requester disable
the settle delay, pick a weaker surface, or narrate a privileged action as something harmless.

`sudo-shim` stays a third binary rather than a second personality of the gate selected by `argv[0]`:
it runs as the untrusted caller, the gate runs as root under a sudoers rule, and collapsing that
would give the sudoers-named binary a second contract.

## Authority model

`sudo-prompt` is the only sudo target at all. A permitted user may ask it to run any command, but
only after the fixed root-owned UI approves it — sudoers constrains the entry point, not the
eventual command. Direct invocation of the gate is a supported path and still prompts.

The shim is convenience, not security: anything it gets wrong the caller could have done by calling
`/usr/bin/sudo` directly.

sudoedit has no supported path: the unshadowed `/usr/bin/sudoedit` is permitted by no rule,
`sudo -e` and its abbreviations pass through the shim to real sudo and are denied, the gate rejects a
COMMAND whose basename is `sudoedit`, and the gate's interpreter whitelist refuses to hand any edit
flag to an inner sudo. **Do not overstate that as containment** — every check is keyed on a name in
the argv the human is shown, and an approved root command can reach an editor however it likes
(`sudo sh -c …`, `sudo vim FILE`, a caller-owned symlink to `/usr/bin/sudo`). Root editing files is
not a privilege escalation. The purpose is that the *shim's own* output is never a sudoedit request
and that a directly-invoked request has a shape the human can read.

`ai` is in the group deliberately: an agent asking for root and a human approving it on the prompt is
a supported flow. Consequences: an agent request blocks the gate lock until someone answers, and the
uid field is the only thing separating "I typed this" from "the agent typed this". Anything that must
fail fast uses `sudo -n`, which passes through to real sudo and is denied immediately.

## The shim's classification

A pure function over argv and the shim's own environment (`sudo-shim/src/classify.rs`), returning
`PassThrough` or `Gate(argv)`:

1. euid 0, or empty argv → PassThrough.
2. Scan leading tokens: `--` ends options; `NAME=value` is collected; `--preserve-env=LIST` is
   *expanded and never forwarded*; the whitelist `-i -s -u -g --user --group --user= --group=` is
   collected; any other `-`-leading token → PassThrough with the raw argv; anything else is the
   command token.
3. No command token → Gate iff `-s`/`-i` was collected, else PassThrough.
4. Plain: `sudo sudo-prompt -- [NAME=value ...] COMMAND ARGS...`. No `env(1)` in the chain — that
   would put `/usr/bin/env` in the prompt's command field and break on a command token containing
   `=` or starting with `-`.
5. Interpreter: `sudo sudo-prompt -- /usr/bin/sudo RAWARGV...`, RAWARGV being the original argv from
   the first *collected* token on, byte for byte, with each `--preserve-env=LIST` replaced **in
   place** by the assignments it expanded to. Starting at the first collected token matters:
   `sudo FOO=bar -u ff cmd` would otherwise drop `FOO=bar`.

On the interpreter path the gate's own assignment list is therefore always empty: every assignment
becomes a command-line variable for the inner sudo, the only place it can be and still survive that
sudo's `env_reset`. That works because root's rule matches `ALL` and sudoers(5) implies SETENV for
such a rule — do not narrow root's rule without adding SETENV.

Matching is on exact spellings only. Modelling getopt_long's abbreviation rules would mean tracking
sudo's whole option table, so `--us=ff`, `--ed` and `-uff` pass through to real sudo, which denies
them for lack of a rule.

Deliberately unsupported, all failing through real sudo: `-e`/`--edit`/sudoedit, `-E`/bare
`--preserve-env`, `-n`, `-b`, `--chdir` (would need a `CWD=*` tag in root's rule), compact option
forms, long-option abbreviations, and mixed informational+command invocations like `sudo -k cmd`.

## The gate

Interface, complete: `sudo-prompt -- [NAME=value ...] COMMAND [ARGS...]`. Leading tokens matching
`[A-Za-z_][A-Za-z0-9_]*=` are assignments, the first token that does not match is COMMAND, and
nothing after it is re-interpreted — sudo's own rule, so `./a=b` is a command. Rejected: a missing
delimiter, empty argv, empty COMMAND, assignments only, a `SUDO_*` assignment, a `sudoedit`
basename, and any token before `--`.

Exit status: approval exec()s, so the command's own status or signal is the result. Denial exits 125
with exactly `User denied sudo :(`. Every operational error exits 125 with a message naming the
failed check. (A command that itself exits 125 is indistinguishable; stderr disambiguates.)

Order of operations: cwd → euid check → parse → flock → capture and scrub the environment →
provenance → display selection → GTK init → build the command environment → prompt → unlock →
resolve → fd hygiene → `execve`.

### Two environments, neither inherited

`env_reset` has a hardcoded survivor set (TERM/PATH/HOME/MAIL/SHELL/LOGNAME/USER/SUDO_*) that no
sudoers setting removes, and HOME and TERM in it are the *caller's* values. So sudo cannot hand
either environment over clean and the gate builds both itself.

The **gate's own**: capture `SUDO_UID`/`SUDO_GID`/`SUDO_USER` and the passthrough candidates,
`clearenv`, then set only `HOME=/root`, a fixed PATH, system data dirs and
`LANG=C.UTF-8`; `XDG_RUNTIME_DIR` and `WAYLAND_SOCKET` come later, after display selection validated
the directory. `XDG_RUNTIME_DIR` is set explicitly rather than left unset, because GLib otherwise
falls back to a cache-directory runtime path and GTK, dconf and the cursor cache land somewhere
unintended. Never configure root's GTK from caller `GTK_THEME`, `GTK_MODULES`, `GIO_MODULE_DIR`,
`XDG_DATA_DIRS`, runtime or display variables. `envsetup.rs` is the only module allowed to touch
`environ`, and a source lint enforces it.

The **command's** is constructed from exactly three things: a root-controlled base (a fixed
`COMMAND_PATH`, `HOME=/root`, `USER`/`LOGNAME=root`, root's passwd shell); a short validated
passthrough list
(`TERM`, `COLORTERM`, `LANG`, `LANGUAGE`, `LC_*` — a value failing a bounded-length, conservative
charset check is dropped, never sanitized); and the request's assignments, applied last. Then the
gate sets `SUDO_UID`/`GID`/`USER`/`COMMAND` itself so provenance cannot be rewritten. `TERM` is on
the list because without it every interactive root command is unusable; `TERMINFO`, `TERMCAP` and
`TERMPATH` are not.

**PATH is never inherited.** `env_reset` preserves the *caller's* `PATH` unless `secure_path` is
set, and the two arrive at the gate identically, so a gate reading the inherited value could not
tell them apart — it would resolve the approved command against a caller-chosen list while the
prompt named a bare `id`. Reading it at all made an external sudoers setting load-bearing for the
prompt's honesty, so the gate reads neither and always uses `cmdenv::COMMAND_PATH`. A caller who
wants a different PATH sends `PATH=…`, which arrives in argv and is shown in the Environment field
like any other assignment. `secure_path` is still worth setting for sudo used outside the gate, but
nothing here depends on it.

Assignments are applied **without filtering** — a deliberate difference from stock sudo. There is no
`env_check`/`env_delete` equivalent and `LD_PRELOAD` is not special-cased, so
`sudo --preserve-env=LD_PRELOAD cmd` works. The mitigation is disclosure, not filtering: every
caller-controlled name and value is rendered in the prompt's `env` field, and because the
environment is constructed rather than inherited that field is a *complete* account of what the
caller put into the root command's environment.

Consequences to remember: `sudo gui-app` no longer inherits DISPLAY/XAUTHORITY, and HOME is root's.
Name them (`--preserve-env=DISPLAY,XAUTHORITY`) and they work and are displayed. On the interpreter
path the inner sudo runs its own `env_reset` over this environment, so the field can over-promise
about what survives but never under-promise.

### Display selection

Caller `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY` are ignored — non-root runtime dirs on this host hold
real, non-root-owned wayland sockets, so scanning `/run/user/*` could land on a caller-controlled
compositor. The gate inspects only `/run/user/0`: root-owned directory, not group/other writable;
`wayland-N` entries only; not a symlink, a root-owned socket; lowest N; connect; confirm uid 0 via
`SO_PEERCRED`. It does not probe other displays — a stale socket from a dead compositor fails the
gate until removed from a TTY, accepted because recovery is already the TTY.

The validated fd is kept: FD_CLOEXEC is cleared and `WAYLAND_SOCKET=<fd>` set, so libwayland uses it
directly and no second lookup happens. That is safe only because `/run/user/0` is
`drwx-----x root:root`. Immediately before the exec, FD_CLOEXEC is re-armed on that fd and set on
everything above fd 2 — otherwise the approved command inherits a root-authenticated connection to
root's compositor, and wlbouncer filters globals per uid *at connect time*, so it would carry virtual
keyboard, virtual pointer, screencopy, layer shell and the session lock manager into a command that
may be running as `ff` or `play`. Do not rely on the inner sudo's `closefrom`: it only exists on that
path, and only until someone grants `closefrom_override`.

### Rendering safety

Escaping bytes is not enough on its own — GTK would happily read caller text as Pango markup or as a
mnemonic. The mechanism, not a habit:

- Untrusted bytes live in an `Untrusted` newtype from the moment they leave argv. It implements
  neither `Display` nor `Deref<Target = str>`, so it cannot be interpolated, concatenated with
  trusted prose, or passed anywhere expecting `&str`. The only way onto the screen is `Escaped`.
- `Escaped` is built only by `Escaped::of`/`shell_token` (escaping untrusted bytes) or from
  `&'static str` literals and numbers — the type is the evidence that text is safe.
- `permission-prompt-ui/src/text.rs` is the only module in the workspace that constructs or writes
  to a text widget, and everything there sets `use-markup` and `use-underline` off explicitly.
- `permission-prompt-ui/tests/source_lint.rs` fails on the markup/underline APIs, on text-widget
  constructors outside `text.rs`, and on `set_var`/`remove_var`/`clearenv` outside `envsetup.rs`.

Escaping: printable ASCII and ordinary printable non-ASCII (`café.txt`) stay readable; C0 controls
and DEL become `\xNN`; every other Control, Format, Surrogate, PrivateUse, Unassigned, LineSeparator,
ParagraphSeparator or SpaceSeparator codepoint becomes `\u{NNNN}` (so U+2028/U+2029, which Pango
treats as mandatory breaks, and U+00A0, which fakes alignment, cannot move text); invalid UTF-8 is
escaped **per byte** rather than replaced, because a lossy conversion renders distinct requests
identically. `\xNN` is reserved for single bytes and `\u{...}` used above ASCII, so a stray `0x85`
byte and the C1 control U+0085 do not collide. A literal backslash is doubled but does not set the
"was escaped" flag, so that flag stays meaningful.

Argv is displayed exactly as requested, one shell-quoted token per line, unresolved: resolution
happens at exec time against the captured root PATH, and displaying a resolved path would promise an
inode identity the gate cannot hold across the approval window.

### What the dialog says, and what it leaves out

The prompt carries only what the decision needs. No field is labelled unless the label says
something the reader could not infer: the gate is a heading, a trusted subtitle naming the
requester, an unlabelled command box, and `in` / `env` gutter rows. There is no key-binding prose
(Enter and Escape on a two-button dialog are not news), no "about this prompt" block on the gate,
and no note explaining that root's own PATH/HOME/USER are set — `env` lists what the *caller* added,
which is the only part anyone can act on. The generic presenter keeps its one compiled-in
disclaimer, as the trusted subtitle, because "this is not the sudo gate" is exactly what a reader
cannot infer.

The one status line that stays is "controls unlock in a moment", which answers the question a
greyed-out button raises. It is faded to opacity 0 rather than hidden once settled: a widget that
stops taking space would move the buttons at the instant they go live, and the pointer could end up
over the *other* one.

### Layout under pressure

An `ext-session-lock-v1` surface must commit exactly the size the compositor configured, so a dialog
whose *minimum* exceeds a small output is a protocol error — which kills the client and strands the
session locked. That is not a theoretical risk; it happened during development at 480x360.

So: caller-controlled fields live in bounded scrolling viewports with `min-content-height` 0 and a
permanently visible (non-overlay) scrollbar, plus an "N more" marker floating over the bottom of the
viewport. Their natural height is their content, capped (120px, or 360px for the one field per
dialog marked `expanding()` — the command for the gate, the message for the generic presenter). The
dialog is `valign: Center`, so a scrolled window allocates it its natural height on a roomy output
and squeezes it towards its minimum on a cramped one, where the viewports — minimum height zero —
are what gives. Verified: at 1280x720 the dialog hugs its content; at 480x360 it shrinks and the
buttons stay put; at 320x240 the outer scroller takes over, which is the documented last resort
where keys still answer the prompt.

Two GTK details that cost an afternoon:

- A scrolled window counts a horizontal scrollbar in the height it requests only under
  `PolicyType::Always`; under `Automatic` an appearing bar silently eats the field's last line and
  starts it scrolling vertically too. `reserve_hscrollbar_room` flips the horizontal policy to
  `Always` exactly while the content overflows sideways. It settles, because the extra height does
  not change how wide the content is.
- The theme's scrollbar-slider minimum height is a floor under every viewport, which left a one-line
  field sitting in a three-line box. `.pp-viewport scrollbar slider { min-height: 14px }` removes it.

Hard ceilings: 4096 rendered characters per token, 512 lines and 64 KiB per field, each with an
unmissable truncation marker — scrolling does not help if GTK is laying out a megabyte of argv.

### Input

- One lock surface per output, all with the same dialog over an opaque backdrop. Nothing is dimmed:
  a lock surface replaces the desktop rather than sitting over it.
- Settling is a **quiet period**, not a fixed window: the controls stay visibly disabled until
  `SETTLE` (1s) has passed with no key press, key release or pointer button. It starts from the
  second frame-clock tick of a surface — the frame clock of a Wayland surface is driven by the
  compositor's frame callbacks after the first commit, so a second tick means the surface is really
  being presented, and a DPMS-blanked output never gets there. It restarts whenever any surface
  presents for the first time or (re)gains keyboard focus.
- Pointer *motion* is not input: a drifting mouse, a resting touchpad finger or a VM absolute pointer
  emits motion continuously, and with no prompt timeout a motion-sensitive quiet period would mean
  sudo never works. Scrolling is likewise left alone so the fields can be read while it settles.
- The wait is **capped** at 5×`SETTLE` from the last non-input restart, and reaching the cap
  **denies** with its own message rather than enabling the controls — enabling them would hand
  approval to whatever is generating the input. The cap restarts only on the non-input restarts, so
  a late-waking or refocused output survives while a stuck key stays bounded. This is a cap on
  settling, not on deliberation: once settled the prompt waits indefinitely.
  (The plan's prose said the cap counted "from when the current quiet period began", which
  contradicts its own explicit "never on an input event". The explicit rule is what is implemented.)
- Approval needs a fresh physical key *down* on `Return`/`KP_Enter`/`ISO_Enter`, or a pointer press
  delivered after settling. Escape denies. Held keycodes are tracked so a client-side autorepeat is
  never a fresh press, and the set is cleared on focus loss. There are no focusable widgets, no
  default action and no IM context.

### Session lock discipline

Order: flock → display selection → `lock()` → windows. `failed` is a denial, not a fallback trigger.

- **`monitor` and `locked` arrive in either order.** Observed both ways on the same sway. So windows
  are created in the `monitor` handler but only *presented* once `locked` has arrived, and "were
  there any outputs at all?" is answered by a 2s timer rather than from either signal.
- Monitor removal is detected via `GdkMonitor::invalidate`, not the window's `destroy`: the library
  unmaps and unrefs the window but we hold a strong reference, so no destroy fires. Losing the last
  output denies rather than holding both locks with nothing on screen.
- Every survivable exit path unlocks *and* waits for the compositor to process it before tearing
  anything down — approval before the exec, denial, the settle cap, the three signals, and every
  operational error after `lock()`. The unlock happens once in `app::run`, outside every callback:
  doing it inside one deadlocks on the state `RefCell`, because the roundtrip makes GTK emit signals
  whose handlers borrow the same state. (That was a real crash during development.)
- A panic hook unlocks and roundtrips; do not build with `panic = "abort"`.
- What remains: SIGKILL, the OOM killer, a real crash. Those leave the session locked with no client
  — sway shows a plain red screen. Recovery is drilled and documented in
  `sudo-prompt-operations.md`.

The flock is separate and simpler: `/run/sudo-prompt.lock` opened `O_CREAT|O_NOFOLLOW|O_CLOEXEC`
0600, fstat'd for root ownership, regular file and no group/other write, then a non-blocking
exclusive `flock`. Safe to create without a tmpfiles.d entry because `/run` is root-owned and only
root-writable. The lock lives on the open file description, so the kernel releases it on any exit
including SIGKILL — the opposite of the session lock. SIGINT/SIGTERM/SIGHUP are never blocked and all
count as denial (SIGHUP is the normal outcome of the requesting terminal going away).

## Host requirements worth remembering

- **gtk4-layer-shell ≥ 1.2.0**, not 1.1.0 as the plan said: `Instance` appeared in 1.1 but the
  `monitor` signal the per-output lock surfaces rely on is 1.2. Installed here: 1.3.0.
- gtk4 ≥ 4.12 (`CssProvider::load_from_string`) and ≥ 4.10 (accessible properties). Installed: 4.22.
- glib 0.22 no longer wraps `g_unix_signal_add`, so signals go through a `sigaction` handler that
  does one relaxed atomic store, read by the settle timer.

## Test seams

Two Cargo features, neither enabled in an installed binary:

- `sudo-prompt/test-seams` relaxes the euid-0 requirement and lets `SUDO_PROMPT_TEST_DISPLAY_ROOT`
  and `SUDO_PROMPT_TEST_LOCK_PATH` move those two constants, so the real gate can be driven in a
  nested compositor as an ordinary user. Build it into a separate target dir
  (`CARGO_TARGET_DIR=target/test-seams`) so it cannot be mistaken for the real binary.
- `sudo-shim/test-exec-override` lets `SUDO_SHIM_REAL_SUDO` point the exec at a fake sudo, for
  byte-exact argv and status/signal passthrough checks. Harmless in principle — the shim runs as the
  caller, who could invoke anything directly — but still off by default.

`tests/gui-test.sh` runs the behavioural checks; the rest are `cargo test --workspace` (plus
`cargo test -p sudo-shim --features test-exec-override`). `sudo-shim/tests/gate_agreement.rs` drives
the *real* gate parser and interpreter whitelist with the shim's own output, so the two lists cannot
drift apart.

## Out of scope, deliberately

Remembered decisions and timestamps, tty fallback, any password-authenticated sudo path, PAM command
discovery, sudo plugins, pkexec, being an actual lock screen (no password, no authentication, no idle
handling), sudoedit, `-E`, `--chdir`, compact and abbreviated option forms, and filtering the
assignments a request carries.
