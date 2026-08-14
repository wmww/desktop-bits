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
tests/gui-test.sh       45 behavioural checks in a nested sway
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
virtual-keyboard, input-method and virtual-pointer to every non-root uid, so approval cannot be
forged by synthesizing input — only by tricking the human's own press. XWayland clients can inject
input only to other X clients.

The response entry does create a `GtkIMContext`, which an earlier version of this note cited as
absent. Still sound, for three reasons rather than one: that context lives on the *gate's own*
root-authenticated Wayland connection, so an attacker's uid cannot bind an input method to it; a
denied global means no non-root client has an input method to commit through in the first place;
and an IM commits *text*, never key events, so even a commit could only fill the response box.
Approval still requires a fresh physical key press or pointer press delivered by the compositor and
read in the capture phase before any widget sees it.

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
desktop until the human answers it or minimizes it; and since requests queue, a spam storm becomes a
queue of prompts, each denial costing a settle period rather than ending it — recovery from a prompt
nobody can answer is a root TTY); a SIGKILLed gate leaving the session locked with
no client; what the approved command then does, including a caller-writable `sudo ./script` changing
between approval and exec.

## Why two executables

- `permission-prompt` is a generic yes/no presenter. It never executes a command, gets no sudoers
  entry, and is **not** an authorization boundary. Its caller owns its prose and reads its exit
  status. An unprivileged caller can spoof its own request, which grants nothing. `--response` adds
  the response box (off by default, since what it prints changes this binary's output contract);
  with it, a non-empty response prints as `User response: <text>` on stderr on approval and on
  denial alike, and nothing prints on a cap, signal or error exit.
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

stderr also carries the response, when the human typed one: `User response: <text>` on its own line,
after the denial message on a denial and immediately *before* the exec on an approval, which is what
guarantees it precedes anything the command writes. The denial message keeps its own line byte for
byte — callers grep for it. Nothing prints when the box was empty, so the output of a request nobody
annotated is what it always was, and nothing prints for a settle cap, a signal or an error, none of
which was a human answering. The text is escaped through the same `Escaped::of` path as the command
before it is printed or logged: one line in, one line out, whatever is in the box. The exit status is
unchanged in every case — the response is an annotation on the answer, not a third answer.

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
`COMMAND_PATH`, `HOME=/root`, `USER`/`LOGNAME=root`, root's passwd shell, `LANG=C.UTF-8`); one
validated passthrough, `TERM`; and the request's assignments, applied last. Then the gate sets
`SUDO_UID`/`GID`/`USER`/`COMMAND` itself so provenance cannot be rewritten.

**`TERM` is the only inherited variable.** It is there because without it every interactive root
command is unusable and because no other source knows the caller's terminal. `TERMINFO`,
`TERMINFO_DIRS`, `TERMCAP` and `TERMPATH` are not: those redirect the terminfo *search*, and with
them gone every lookup lands under a root-owned directory, leaving `TERM` as only an index into
one. `cmdenv::term_ok` validates it as a terminfo entry name — leading letter, then alphanumerics
and `. _ + -`, ≤64 bytes — which is an allowlist of the shape a terminal name has rather than a
charset. Notably no `/` and no leading `.`: `TERM` is used downstream as a path component, and a
name that can hold a separator or a `..` is a traversal waiting for a search root that isn't
root-owned. Current ncurses rejects those itself (verified: it does not even `stat`), but that is
ncurses' invariant to keep, not ours. A failing value is dropped, never sanitized, and the drop is
noted in the prompt.

**The locale is root-controlled, not inherited.** `LANG`, `LANGUAGE` and `LC_*` are caller-controlled
switches on root-program *semantics* — decimal separator, collation order (so `sort` and `[a-z]`
glob ranges), `rpmatch()` yes/no patterns, date formats, gettext message text — so a root script
that parses its own output or matches a translated string can be steered by them without anything
appearing in the prompt. Unlike `TERM` they have a safe fixed answer, so the gate sets
`LANG=C.UTF-8` and forwards none of them. The file-loading half of the worry turned out to be
already closed on glibc 2.44: a locale name containing `..` or a leading `.` is refused before any
filesystem access, and an absolute-looking one is *concatenated* under root-owned
`/usr/lib/locale/` rather than honoured (`LC_ALL=/tmp/x` opens `/usr/lib/locale//tmp/x/LC_CTYPE`).
`LOCPATH`, which would genuinely redirect the load, was never a passthrough candidate. The
semantic half is the reason for the change.

**PATH is never inherited.** `env_reset` preserves the *caller's* `PATH` unless `secure_path` is
set, and the two arrive at the gate identically, so a gate reading the inherited value could not
tell them apart — it would resolve the approved command against a caller-chosen list while the
prompt named a bare `id`. Reading it at all made an external sudoers setting load-bearing for the
prompt's honesty, so the gate reads neither and always uses `cmdenv::COMMAND_PATH`. A caller who
wants a different PATH sends `PATH=…`, which arrives in argv and is shown in the `env` field
like any other assignment. `secure_path` is still worth setting for sudo used outside the gate, but
nothing here depends on it.

Assignments are applied **without filtering** — a deliberate difference from stock sudo. There is no
`env_check`/`env_delete` equivalent and `LD_PRELOAD` is not special-cased, so
`sudo --preserve-env=LD_PRELOAD cmd` works. The mitigation is disclosure, not filtering: every
assignment is rendered in the prompt's `env` field, so the field is a complete account of what the
caller *asked* to put into the root command's environment.

The field deliberately does **not** list the inherited `TERM`. It used to list the whole passthrough
set, which meant `COLORTERM`, `LANG` and `TERM` appeared on every single request — ambient shell
state nobody set on purpose, sitting three lines above the line that might say `LD_PRELOAD=`. That
trains the eye to skip the field, which costs more than the disclosure buys. So `CommandEnv` splits
`assigned` (displayed) from `inherited` (not), and the shrunken passthrough plus `term_ok` is what
pays for the silence: hiding a value means its validator is the only backstop, so the validator got
strict at the same time the list got short. In the ordinary case the `env` field is now empty.

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
ParagraphSeparator or SpaceSeparator codepoint becomes `\uNNNN` — `\UNNNNNNNN` above the BMP — (so
U+2028/U+2029, which Pango treats as mandatory breaks, and U+00A0, which fakes alignment, cannot move
text); invalid UTF-8 is escaped **per byte** rather than replaced, because a lossy conversion renders
distinct requests identically. `\xNN` is reserved for single bytes and `\uNNNN` used above ASCII, so
a stray `0x85` byte and the C1 control U+0085 do not collide. A literal backslash is doubled but does
not set the "was escaped" flag, so that flag stays meaningful.

Those spellings are bash's, not Rust's, and the widths are fixed at two and four hex digits because
that is what makes them unambiguous — bash reads *at most* that many, so `\u200b` followed by a
literal `b` cannot be misread as a five-digit escape.

### Argv on one line

Argv is displayed exactly as requested, unresolved — resolution happens at exec time against the
root-controlled PATH, and displaying a resolved path would promise an inode identity the gate cannot
hold across the approval window — and as **one shell-quoted line**, not a token per row. That is the
shape the requester typed and the shape they would paste back, and it makes the prompt and the log
record literally the same string.

Quoting carries the token boundaries, in three forms: bare when every byte is safe unquoted; `'…'`
when it is not; and `$'…'` when the escaper had to write a backslash. The third is not cosmetic — in
`'…'`, bash takes `\x0a` as five literal characters, so a token containing a newline would be shown
as a command that pastes back as a *different* argv. `permission-prompt-ui` asserts the round trip
against a real bash (`rendered_tokens_paste_back_as_the_same_argv`) rather than against our reading
of its rules, over spaces, quotes, backslashes, invalid UTF-8 and an above-BMP escape.

Two limits worth knowing. A token clipped by the length ceiling pastes as a syntax error, not as a
shorter command — that is the failure to prefer, and it is the only reason the clip is allowed to
land mid-quote. And a line long enough to wrap cannot show whether a break fell on a space or inside
a token, exactly as in a terminal; the per-row layout did not have that ambiguity, and it is the
price of the shape.

### What the dialog says, and what it leaves out

The prompt carries only what the decision needs. The gate is, top to bottom: the command, in the
accent colour and larger than anything else; the interpreter warning when there is one; one flat
line reading `user | cwd`; an `env` gutter row when the request has one; and the response box. No
icon, no heading and no subtitle — the
command *is* the question, and a sentence asking it in the gate's own words would only push the
answer further down. The button row is `[minimize] [X] [Run as root]`, and it says what the heading
used to: approve keeps its words, because it is the one control that has to state what it does,
while deny and minimize are cairo-drawn icons evoking a window's own decoration buttons. Drawn
rather than glyphs, because no font glyph for "minimize" can be relied on and because drawing both
keeps their stroke weight and colour identical — the colour is `widget.color()` read at draw time,
so the theme and the `:disabled` fade (which only deny ever takes) reach them exactly as they reach
a label. Each carries an
accessible label, which its constructor requires.

`cwd` is abbreviated with `~` against the *requesting* user's passwd home directory, never root's;
the uid and gid are not shown at all, since the name answers "was this me?" and the log record keeps
the numbers. That line is the one piece of caller data rendered without a viewport around it: it is
one short line by nature, it wraps rather than scrolls, and it is still escaped and capped
(`Field::flat`).

There is no key-binding prose (Enter and Escape on a two-button dialog are not news), no "about this
prompt" block on the gate, and no note explaining that root's own PATH/HOME/USER are set — `env`
lists what the caller *asked* for, which is the only part anyone can act on. The generic presenter does
keep a heading and its one compiled-in disclaimer as the subtitle: it has no single thing it is
about, and "this is not the sudo gate" is exactly what a reader cannot infer.

Fields are one label each rather than one label per line, and selectable, so a command can be
selected and copied in one go. That pulls two GTK behaviours in: labels are focusable, and a label
the toolkit focuses selects all of itself — so `gtk-label-select-on-focus` is turned off in `init()`,
or every prompt would come up showing a selection nobody made. Ctrl+C and Ctrl+Insert are the only
keys the input state machine lets through to the focused widget; nothing else there is focusable, so
they can copy text and cannot reach anything activatable.

The response box is one full-width `GtkEntry` above the buttons, placeholder "Response", empty by
default and ignorable. The gate always has it; the point is agents — "next time do it this other
way" plus an accept or a deny is a channel the exit status alone cannot carry — and the gate has no
options, so a flag for it would be a caller-controlled one. It is *disclosure to the requester*: the
text goes back to whoever asked, so the placeholder is a bland "Response" rather than anything that
invites a secret. It is also the one text widget in the workspace whose content is neither compiled
in nor escaped caller data; it is the human's own, typed on the trusted surface, and it is escaped
on the way *out* instead.

One `GtkEntryBuffer` is shared by every output's entry, so the text follows the reader between
screens, capped at 1024 characters — a response is a sentence, and the cap only has to keep a paste
from putting something unbounded into a log record and onto the caller's stderr. GTK does **not**
strip newlines from text pasted into a single-line entry (checked, not assumed): a pasted newline
stays in the buffer and renders as a control glyph on the one visible line. Nothing is hidden by
that, and the printed line stays one line because `Escaped::of` turns it into `\x0a` — that
guarantee is ours, not the toolkit's.

The quiet period shows as two greyed-out answer buttons and nothing else: no status line, and
nothing that appears or disappears, since a widget that stopped taking space would move the buttons
at the instant they go live, and the pointer could end up over the *other* one. The response box
and minimize are never disabled — see the input state machine for why.

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

Nothing scrolls sideways: the viewports are `PolicyType::Never` horizontally and the content wraps.
A field that scrolls horizontally can hide its tail behind nothing louder than a scrollbar, and the
tail of a command line is where the interesting argument tends to be.

Width grows with the content, up to 80 monospace columns (`text::MONO_WRAP_CHARS`), and two GTK
facts decide that:

- A `ScrolledWindow` asks for its child's *minimum* width unless `propagate-natural-width` is set,
  and the minimum of a label that wraps mid-word is one column. Without propagation the dialog was
  as narrow as its button row however many arguments it was showing, so the command wrapped at ~25
  columns in a box with empty screen either side.
- `max-content-width` is a no-op under a horizontal policy of `Never`, so it cannot be the ceiling.
  The ceiling is the label's `max-width-chars`, which bounds the *natural* width so a long line
  wraps instead of making the whole dialog that wide.

Columns rather than pixels, because the fields are monospace and a terminal's width is the shape
this text is written for. The prominent field is 1.3em, so 80 of its columns is ~920px and the
plainer fields' 80 are ~700px; both hug their content when it is shorter. Minimum width is
untouched — one column — so the shrink-on-a-cramped-output behaviour above is unchanged, verified
again at 480x360.

GTK details, each of which cost an afternoon:

- Wrapping needs `WrapMode::Char`, not `WordChar`: Pango counts `-` and `/` as word boundaries, so
  `--exclude` came out as `--` ending one line and `exclude` starting the next, which reads like a
  separator that is not in the argv. Char breaks at the column, as a terminal does.
- Pango then hyphenates the break, putting a `-` on screen that is not in the command (`--excl-` /
  `ude`). Off via an `insert-hyphens` attribute — an attribute, not markup, so it cannot introduce
  text.
- The overflow marker counts the label's *rendered* lines (`layout().line_count()`), not the
  `Escaped` lines it was built from: one logical line wraps to several, and counting the latter
  under-reports exactly when a command is long enough for the count to matter.
- The theme's scrollbar-slider minimum height is a floor under every viewport, which left a one-line
  field sitting in a three-line box. `.pp-viewport scrollbar slider { min-height: 14px }` removes it.
- Every colour is a theme named colour (`@theme_base_color`, `@borders`, `@insensitive_fg_color`,
  `@warning_color`, plus one `mix()`-derived inset shade for the viewports), so the prompt follows
  root's GTK theme in light and dark alike. The caller cannot influence which theme that is —
  `GTK_THEME` is never forwarded. The gate is `@error_color` and the generic presenter
  `@theme_selected_bg_color`, never the same colour, because those two must not be confusable
  whatever the theme does. The approve button clears `border` and `box-shadow`: the theme draws a
  button outline in its own button colour, which around a filled accent is a stray hairline.
- **The stylesheet sits above `PRIORITY_USER`, not at `PRIORITY_APPLICATION`.** GTK loads
  `~/.config/gtk-4.0/gtk.css` at `GTK_STYLE_PROVIDER_PRIORITY_USER` (800), above the 600 an
  application's own provider normally gets — and `@import`ing a theme's stylesheet from exactly
  that file is how a GTK3-era theme is applied to GTK4 at all (libadwaita apps ignore
  `gtk-theme-name`, so nothing else works). That puts a whole theme above the prompt, and themes
  open with a reset: Sweet's is `* { padding: 0 }`, which collapsed the dialog's padding, the
  viewports' and the buttons' — every field hard against the border on all four sides, the approve
  fill gone — while leaving every colour the theme does not mention. Layout is not the theme's to
  override, so `app::CSS_PRIORITY` is `PRIORITY_USER + 1`. Colour still is: `@theme_*` resolve
  through the whole cascade whatever priority we sit at. A GUI test launches the presenter with a
  `HOME` holding just that reset and checks the fields are still inset from the dialog's border,
  which is what the `geometry: dialog` line is for.
- **Only legacy colour names.** `@accent_color` and the rest of the libadwaita set exist in GTK's
  own theme and in few others; under Sweet (a GTK3-era theme, and the one on this host) the approve
  button came out with no fill at all, because GTK drops a declaration naming an undefined colour
  in silence. A unit test now holds every `@name` in the stylesheet to the GTK3-era public set or
  to one the stylesheet defines itself.
- **Which theme the gate gets** is root's GTK configuration and nothing else. The environment is
  scrubbed before GTK init, so `GTK_THEME` — how root's own desktop selects Sweet, from
  `bashrc-shared.sh` — does not reach it, and the gate came up in GTK's built-in light theme on a
  dark desktop. What does reach it is `gsettings set org.gnome.desktop.interface gtk-theme` run as
  root (the host's answer, via the settings portal on root's session bus, which `scrub()` leaves
  reachable through `XDG_RUNTIME_DIR=/run/user/0`), and failing that
  `/root/.config/gtk-4.0/settings.ini`, since `scrub()` sets `HOME=/root`. Both are root-owned,
  which is the whole requirement. `init()` logs the theme it resolved at debug level; see
  `host-sudo-setup.md`.
- A `GtkGrid` hands its spare width to *every* column a spanning child covers, so the full-width
  command field made the gutter column expand and stranded the `env` label a third of a dialog away
  from its own box. The fields are plain rows now, with a fixed-width gutter.

Hard ceilings: 4096 rendered characters per token, 512 lines and 64 KiB per field, each with an
unmissable truncation marker — scrolling does not help if GTK is laying out a megabyte of argv. The
character ceiling clips *within* a line rather than dropping it: the command is one line however
many arguments it has, and dropping it would leave the field empty.

### Input

- One lock surface per output, all with the same dialog over an opaque backdrop. Nothing is dimmed:
  a lock surface replaces the desktop rather than sitting over it.
- Settling is a **quiet period**, not a fixed window: the answer buttons stay visibly disabled until
  `SETTLE` (400ms) has passed with no key press, key release or pointer button. It starts from the
  second frame-clock tick of a surface — the frame clock of a Wayland surface is driven by the
  compositor's frame callbacks after the first commit, so a second tick means the surface is really
  being presented, and a DPMS-blanked output never gets there. It restarts whenever any surface
  presents for the first time or (re)gains keyboard focus.
- Pointer *motion* is not input: a drifting mouse, a resting touchpad finger or a VM absolute pointer
  emits motion continuously, and with no prompt timeout a motion-sensitive quiet period would mean
  sudo never works. Scrolling is likewise left alone so the fields can be read while it settles.
- The wait is **capped** at `SETTLE_CAP` (5s) from the last non-input restart, and reaching the cap
  **denies** with its own message rather than enabling the controls — enabling them would hand
  approval to whatever is generating the input. The cap restarts only on the non-input restarts, so
  a late-waking or refocused output survives while a stuck key stays bounded. This is a cap on
  settling, not on deliberation: once settled the prompt waits indefinitely.
  (The plan's prose said the cap counted "from when the current quiet period began", which
  contradicts its own explicit "never on an input event". The explicit rule is what is implemented.)
- Approval needs a fresh physical key *down* on `Return`/`KP_Enter`/`ISO_Enter`, or a pointer press
  delivered after settling. Escape denies. Held keycodes are tracked so a client-side autorepeat is
  never a fresh press, and the set is cleared on focus loss.
- **Only the two answers are gated.** The quiet period exists to stop input the human did not aim
  at this prompt from *answering* it, so `Dialog::set_settled` disables approve and deny and
  nothing else. The response box and minimize stay live from the first frame: neither decides
  anything (minimize hands the desktop back with the request still pending), and both are what
  someone who is not ready to answer reaches for first. A press that lands on one of them is let
  through by the capture gesture instead of being claimed — it still counts as input and still
  restarts the quiet period.
- The key controller runs in the capture phase, before any widget, and its rule is: track the key
  always; handle Enter and Escape itself and **stop** them, settled or not, so they keep their
  meaning while the human is typing and the entry never sees an `activate`; let Ctrl+C/Ctrl+Insert
  through to copy from the focused label; let anything else through only when the window's focus is
  inside the response entry, and stop it otherwise. Buttons set `can-focus` off, so focus can only
  ever be on a label or in the entry, and no key can reach anything activatable. There is no
  default action.
- Typing therefore cannot reach the settle cap, but the reason is no longer that the entry is
  disabled: a key press or release **delivered to the focused entry does not feed `Settle::input`
  at all** (Enter and Escape excepted, since the controller takes those wherever focus is). Without
  that, a sentence typed into the box would restart the quiet period keystroke by keystroke and
  reach the 5s cap, denying the request out from under whoever was writing it. This costs nothing:
  the entry takes focus only from a deliberate click, that click is itself input, and the entry
  cannot answer — so approval is still 400ms of quiet after the last event aimed anywhere else.
- Nothing is focused on map or on refocus (`drop_focus`), so the human clicks the entry to type; a
  refocused window un-settles but the box keeps working and the buffer keeps the text. Tab out of
  the entry moves focus to a label and typing goes back to being swallowed, which is harmless —
  Enter and Escape are global either way.

### The minimize chip

The human can shrink a live prompt to a small overlay-layer surface in the bottom-right corner of
every output, use the desktop to check whatever made them hesitate, and come back to it. The chip
shows the same shell-quoted command line — ellipsized — and the requesting user, plus an X.

What makes that safe is that the chip is **powerless**: its only two actions are deny and expand.

- Approval is reachable only on the session-lock surface, and only after a fresh quiet period.
- The prompt always *starts* full and locked. Only a human click on the lock surface minimizes it,
  and the gate has no options, so a caller cannot ask for a prompt that starts minimized. Keep it
  that way.
- Minimize is clickable during the quiet period (see the input state machine). A stray or baited
  click there costs the human a chip in the corner and a click to expand, and the expand starts a
  fresh quiet period — it can never approve, and it does not deny either.
- The chip is on the overlay layer, which non-root uids can also draw on
  (`issues/overlay-layer-spoofing.md`), so it is coverable and spoofable. Accepted by design:
  covering it hides a pending prompt, which is the accepted DoS class; a fake's text is corrected
  the moment the real lock surface shows the real argv; and a tricked click yields either a denial
  (safe) or an expand.
- The fresh quiet period on expand is load-bearing, not cosmetic: it defeats bait-a-double-click,
  since the expanding click is itself input and the new lock surface starts settling from scratch.
- A relock that fails — somebody else took the only session lock while we were minimized — is
  `could not take the session lock`, consistent with "`failed` is a denial". Reachable in practice
  only through the compositor's implicit pointer grab, which is how the GUI test provokes it.
- The gate's flock is held throughout, so other sudo requests stay behind this one.
- The chip asks for no keyboard (`KeyboardMode::None`), so its entire input surface is those two
  click targets and the keyboard belongs to the desktop while it is up. Its exclusive zone is 0, so
  it floats over the desktop rather than reshaping it.
- Ellipsizing the command is right *here* and nowhere else: the chip approves nothing and the whole
  argv is one click away. `mono_block`'s never-ellipsize rule still governs the surface where
  approval happens.

`chip: Option<ChipSpec>` on `DialogSpec` is the whole switch — there is no separate config flag. The
generic presenter passes `None` and is unchanged, and minimize is offered only in
`SurfaceMode::SessionLock`, since the other surfaces do not seize the screen and so have nothing to
get out of the way of.

### Session lock discipline

Order: flock → display selection → `lock()` → windows. `failed` is a denial, not a fallback trigger.

- **`monitor` and `locked` arrive in either order.** Observed both ways on the same sway. So windows
  are created in the `monitor` handler but only *presented* once `locked` has arrived, and "were
  there any outputs at all?" is answered by a 2s timer rather than from either signal.
- Monitor removal is detected via `GdkMonitor::invalidate`, not the window's `destroy`: the library
  unmaps and unrefs the window but we hold a strong reference, so no destroy fires. Losing the last
  output denies rather than holding both locks with nothing on screen.
- **Lock transitions only ever happen between main loops.** Unlocking inside a callback deadlocks on
  the state `RefCell` (see below), so `app::run` is a loop over *phases*: each lock epoch is one
  `run_full_phase` with its own `Inner` and its own `glib::MainLoop`, each minimized period is one
  `chip::run_phase`, and the unlock, the `ACTIVE_LOCK` teardown and the window destruction happen in
  straight-line code in `run` between them. Everything per-phase resets for free — the quiet period
  and its cap, held keys, `shown_settled`, the surfaces, the 2s zero-output timer — and each epoch
  gets a fresh `gtk4_session_lock::Instance`. Relocking in one process is not documented upstream
  but works; see `issues/gtk4-session-lock-warts.md`.
- A phase records its outcome and sets an `over` flag it never clears, so a callback that runs after
  the outcome has been taken out (window teardown fires `destroy`) cannot resurrect the phase.
- What must outlive a phase lives in `app::Ui` (the config and the response buffer) rather than in
  `Inner`, so a response typed before minimizing is still in the box when the prompt expands again.
  The chip phase is handed the config alone: it has no response box and no way to reach that one.
- The signal poll is its own 50ms timer rather than a branch of the settle timer, since the chip
  phase has no settle timer. A phase consumes the flag only while it is live, so a signal cannot be
  swallowed by an outgoing phase during a handover.
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
0600, fstat'd for root ownership, regular file and no group/other write, then an exclusive `flock`.
Safe to create without a tmpfiles.d entry because `/run` is root-owned and only root-writable. The
lock lives on the open file description, so the kernel releases it on any exit including SIGKILL —
the opposite of the session lock. SIGINT/SIGTERM/SIGHUP are never blocked and all count as denial
(SIGHUP is the normal outcome of the requesting terminal going away).

A concurrent request **queues** rather than failing: `LOCK_EX|LOCK_NB` first, and on `EWOULDBLOCK`
one `info` line ("another sudo-prompt is active; waiting for it") — below the default filter, so a
queued sudo is silent unless `RUST_LOG=info` — then a blocking `flock`. Fail-fast was never a control — a caller can retry in a loop — and it punished the
legitimate cases (two scripts racing, or the human running sudo while a prompt is up, which the
minimize feature makes routine). No timeout: the wait is before the UI's signal handlers exist, so
the waiter still has default disposition and Ctrl+C kills it at once. Everything after the flock —
capture, scrub, display selection — is done fresh when the turn comes, so a request that queued for
minutes is not stale. Wake order is unspecified, not FIFO; irrelevant at human scale. A caller
SIGKILLed while queued leaves an orphaned gate that eventually prompts for a command nobody is
waiting on; there is no clean detection, and the human denies it.

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

The GUI harness never hardcodes layout or tuned sleeps — both broke on unrelated UI changes. The
prompt logs, at debug level, `geometry: <approve|deny|minimize|prominent|response> X Y W H`
(window-relative, logged at presentation) and `controls <live|settling>` on every settle
transition; the chip logs `geometry: <chip|chip-body|chip-cancel> X Y W H` in *output* coordinates
instead, because it is a small window in a corner rather than a surface filling the output. The
script derives its click and drag targets from the geometry lines and waits on log markers
(`controls live`, `surface presented`, `minimized`, `chip presented`, `expanded`, `monitor removed`,
`session locked`/`unlocked` from a debug-logged `permission-prompt`) instead of sleeping. It also
scopes its `kill` to gates whose `/proc/PID/environ` names this session's directory: a bare
`pkill -x sudo-prompt` also kills a gate another agent is driving in a different guibox session,
which shows up as unrelated tests failing with "signal 15". Change the dialog freely; only renaming
those log lines or the behaviour itself breaks the suite. The suite's `click <name>` helper nudges
the pointer off a control and back before pressing it, because a control that becomes sensitive
under a pointer that never moved does not get the next click at all —
`issues/stale-pointer-focus-when-controls-go-live.md`.
(The old fixed sleeps flaked about one run in six on the
hotplug checks — the `sleep 2` after `create_output` could return before the second lock surface
was up, so the unplug raced the plug and could tear down the prompt's only surface.)

Nothing in the suite may reach outside its own session. The SIGTERM check used to `pkill -x
sudo-prompt`, which killed the gates of any *other* worktree running the suite at the same time and
produced random "signal 15" denials in it; it now matches this checkout's binary and argv exactly
(`pkill -x -f`, since a substring `-f` match also hits the `sh -c` wrapper that records the exit
status).

## Out of scope, deliberately

Remembered decisions and timestamps, tty fallback, any password-authenticated sudo path, PAM command
discovery, sudo plugins, pkexec, being an actual lock screen (no password, no authentication, no idle
handling), sudoedit, `-E`, `--chdir`, compact and abbreviated option forms, and filtering the
assignments a request carries.
