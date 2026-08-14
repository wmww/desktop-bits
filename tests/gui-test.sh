#!/bin/bash
# Behavioural tests for sudo-prompt that need a real compositor: session lock, the quiet period, the
# key/pointer state machine, hotplug, and the locks.
#
# Runs a private headless sway via `guibox` from https://github.com/wmww/agent-skills (gui-testing
# skill); point $GUIBOX at it, or install the skill at the default path below. Needs sway, grim and
# wdotool.
#
# The gate is built with the `test-seams` feature, which relaxes the euid-0 requirement and lets the
# runtime dir and lock path be overridden. That feature is never enabled in the installed binary, so
# these tests drive the real code with only those two constants moved.
#
#   tests/gui-test.sh [-k]      -k keeps the session directory on failure
set -uo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
GUIBOX=${GUIBOX:-$HOME/.claude/plugins/cache/wmww-agent-skills/gui-testing/0.1.0/guibox}
GATE=$REPO/target/test-seams/debug/sudo-prompt
GENERIC=$REPO/target/debug/permission-prompt
KEEP=${1:-}

pass=0; fail=0
ok()   { echo "PASS  $1"; pass=$((pass+1)); }
bad()  { echo "FAIL  $1: $2"; fail=$((fail+1)); }
have() { command -v "$1" >/dev/null || { echo "SKIP: $1 not installed"; exit 0; }; }

have sway; have grim; have wdotool
[[ -x $GUIBOX ]] || { echo "SKIP: no guibox at $GUIBOX (set \$GUIBOX)"; exit 0; }

echo "building..."
cargo build -p permission-prompt >/dev/null || exit 1
CARGO_TARGET_DIR="$REPO/target/test-seams" cargo build -p sudo-prompt --features test-seams \
    >/dev/null || exit 1

# A placeholder toplevel stands in for the desktop, so lock surfaces have something to cover.
DIR=$($GUIBOX start -- "$GENERIC" --surface toplevel --title placeholder --body "stands in for the desktop")
[[ -d ${DIR:-} ]] || { echo "guibox did not start"; exit 1; }
# shellcheck disable=SC1091
. "$DIR/env"
cleanup() {
    if [[ $fail -gt 0 && $KEEP == -k ]]; then
        echo "session kept at $DIR"
    else
        $GUIBOX stop "$DIR" >/dev/null 2>&1
    fi
}
trap cleanup EXIT

# Extra caller-side environment for one launch, to prove a variable is or is not forwarded.
LAUNCH_ENV=""

# Which run `launch` writes and every helper below watches. The concurrency test moves it aside to
# run a second gate while the first is still up.
watch() { GATE_LOG=$DIR/$1.log; GATE_STATUS=$DIR/$1.status; }
watch gate

# Launch the gate inside the session. From `/`, always: the prompt shows the caller's cwd, so a
# fixed cwd keeps the dialog identical however the repo happens to be checked out.
launch() {
    rm -f "$GATE_LOG" "$GATE_STATUS"
    local args="" a
    for a in "$@"; do args+=" $(printf '%q' "$a")"; done
    swaymsg exec "env $LAUNCH_ENV SUDO_UID=$(id -u) SUDO_GID=$(id -g) \
        SUDO_PROMPT_TEST_DISPLAY_ROOT=$DIR SUDO_PROMPT_TEST_LOCK_PATH=$DIR/lock RUST_LOG=debug \
        sh -c 'cd /; $GATE --$args >$GATE_LOG 2>&1; echo \$? >$GATE_STATUS'" >/dev/null
}

# Wait until the gate has logged at least $2 (default 1) lines matching $1, or the run has
# finished. Every wait in this file keys on a gate.log marker rather than a tuned sleep: a fixed
# sleep is a race against the compositor, and losing that race was the old flakiness.
wait_log() {
    local i
    for i in $(seq 1 60); do
        [[ -f $GATE_STATUS ]] && return 0
        [[ $(grep -c "$1" "$GATE_LOG" 2>/dev/null) -ge ${2:-1} ]] && return 0
        sleep 0.25
    done
    return 1
}

# Wait for the surface to be up (geometry logged), then for the controls to be answerable.
presented() { wait_log "geometry: approve"; }
settled()   { wait_log "controls live"; }
chipped()   { wait_log "chip presented"; }

# This session's gate processes only. A bare `pkill -x sudo-prompt` also kills the gate another
# agent is driving in a different guibox session on the same machine, which shows up here as
# unrelated tests failing with "signal 15".
gate_pids() {
    local p
    for p in $(pgrep -x sudo-prompt); do
        grep -qz "SUDO_PROMPT_TEST_DISPLAY_ROOT=$DIR" "/proc/$p/environ" 2>/dev/null && echo "$p"
    done
}

# The names sway currently has outputs under. Unplugging and creating them renumbers, so nothing
# below hardcodes HEADLESS-N.
outputs() { swaymsg -t get_outputs -r | grep -oE '"name": "[^"]+"' | cut -d'"' -f4; }

# Wait for a marker in another client's log (the generic presenter stands in for the desktop and
# for a rival lock client).
wait_other() {
    local i
    for i in $(seq 1 60); do
        grep -q "$2" "$1" 2>/dev/null && return 0
        sleep 0.25
    done
    return 1
}

finished() {
    local i
    for i in $(seq 1 40); do
        [[ -f $GATE_STATUS ]] && return 0
        sleep 0.25
    done
    return 1
}

status() { cat "$GATE_STATUS" 2>/dev/null; }
gatelog() { cat "$GATE_LOG" 2>/dev/null; }

# The gate logs where its widgets landed ("geometry: approve X Y W H", window-relative, and the
# lock surface fills the output at 0,0) so the tests never hardcode layout. `center <name>` and
# `left_edge <name>` turn the last such line into wdotool coordinates.
geom() { gatelog | grep -oE "geometry: $1 [0-9]+ [0-9]+ [0-9]+ [0-9]+" | tail -1 | cut -d' ' -f3-6; }
center() {
    local x y w h
    read -r x y w h <<<"$(geom "$1")" || return 1
    [[ -n ${h:-} ]] || { echo "no geometry for $1 in gate.log" >&2; return 1; }
    echo "$((x + w / 2)) $((y + h / 2))"
}
left_edge() {
    local x y w h
    read -r x y w h <<<"$(geom "$1")" || return 1
    [[ -n ${h:-} ]] || { echo "no geometry for $1 in gate.log" >&2; return 1; }
    echo "$((x + 2)) $((y + h / 2))"
}

# Click a control by name. The pointer is nudged off the target first: a control that becomes
# sensitive under a pointer that never moved does not get the next click at all — see
# issues/stale-pointer-focus-when-controls-go-live.md — and every test below is about something
# else.
click() {
    local x y w h
    read -r x y w h <<<"$(geom "$1")" || return 1
    [[ -n ${h:-} ]] || { echo "no geometry for $1 in gate.log" >&2; return 1; }
    wdotool mousemove "$((x + w / 2))" "$((y + h + 40))"
    wdotool mousemove "$((x + w / 2))" "$((y + h / 2))" click 1
}

# --- the gate presents, and either verdict works -----------------------------
launch /bin/echo denied-by-escape; settled
wdotool key Escape
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "User denied sudo :("; then
    ok "Escape denies with exit 125 and the exact denial message"
else
    bad "escape denies" "status=$(status)"
fi

launch /bin/echo approved-by-enter; settled
wdotool key Return
if finished && [[ $(status) == 0 ]] && gatelog | grep -q "^approved-by-enter$"; then
    ok "Enter approves and the command runs"
else
    bad "enter approves" "status=$(status)"
fi

# --- the quiet period -------------------------------------------------------
launch /bin/echo should-not-run
# Fired immediately, so these land before the surface is presented.
for _ in $(seq 1 20); do wdotool key Return; done
if [[ -z $(status) ]]; then
    ok "input before the surface is presented never approves"
else
    bad "early input swallowed" "status=$(status)"
fi
settled; wdotool key Return
if finished && [[ $(status) == 0 ]]; then
    ok "the first post-settle Enter approves"
else
    bad "post-settle enter" "status=$(status)"
fi

launch /bin/echo should-not-run
# An Enter held across the settling boundary is not a fresh press.
wdotool keydown Return; settled; wdotool keyup Return; sleep 0.5
if [[ -z $(status) ]]; then
    ok "an Enter held across settling does not approve"
else
    bad "held enter" "status=$(status)"
fi
wdotool key Return
if finished && [[ $(status) == 0 ]]; then
    ok "release-then-press after that does approve"
else
    bad "press after held enter" "status=$(status)"
fi

launch /bin/echo should-not-run
# Sustained input across the presentation restarts the quiet period until the cap denies.
end=$((SECONDS + 9))
while [[ $SECONDS -lt $end && -z $(status) ]]; do wdotool key a; sleep 0.1; done
if [[ $(status) == 125 ]] && gatelog | grep -q "never settled"; then
    ok "the settle cap denies rather than enabling the controls"
else
    bad "settle cap" "status=$(status)"
fi

launch /bin/echo should-not-run
# Pointer motion alone must never restart the quiet period, or a drifting mouse would stop sudo
# from ever working.
for x in $(seq 300 20 700); do wdotool mousemove $x 300; done
if settled && [[ -z $(status) ]]; then
    wdotool key Return
    finished && [[ $(status) == 0 ]] \
        && ok "pointer motion does not restart the quiet period" \
        || bad "pointer motion" "status=$(status)"
else
    bad "pointer motion" "prompt did not settle"
fi

# --- pointer approval -------------------------------------------------------
launch /bin/echo coord; settled
# shellcheck disable=SC2046
wdotool mousemove $(center approve) click 1
if finished && [[ $(status) == 0 ]]; then
    ok "a click on Approve after settling approves"
else
    bad "click approves" "status=$(status); approve at '$(geom approve)'"
fi

launch /bin/echo coord; presented
# shellcheck disable=SC2046
wdotool mousemove $(center approve) mousedown 1
settled
wdotool mouseup 1; sleep 0.5
if [[ -z $(status) ]]; then
    ok "a press begun during settling does nothing when released over Approve"
else
    bad "press across settling" "status=$(status)"
fi
# shellcheck disable=SC2046
wdotool mousemove $(center approve) click 1; finished >/dev/null

# --- selecting and copying --------------------------------------------------
if command -v wl-paste >/dev/null; then
    launch /bin/echo coord; settled
    # From just inside the command's left edge to past its right edge: the whole line.
    read -r cx cy <<<"$(left_edge prominent)"
    read -r _ _ cw _ <<<"$(geom prominent)"
    wdotool mousemove "$cx" "$cy" mousedown 1 \
        mousemove "$((cx + cw + 40))" "$cy" mouseup 1
    wdotool key ctrl+c
    sleep 0.5
    # One label per field and one line per command, so a drag takes all of it, quoted as a shell
    # command rather than as a column of tokens.
    if [[ $(timeout 3 wl-paste -n 2>/dev/null) == "/bin/echo coord" ]]; then
        ok "a drag selects the whole command and Ctrl+C copies it"
    else
        bad "select and copy" "clipboard held '$(timeout 3 wl-paste -n 2>/dev/null)'; log: $(gatelog | tail -3)"
    fi
    # Copying must not have answered the prompt.
    if [[ -z $(status) ]]; then
        ok "Ctrl+C does not answer the prompt"
    else
        bad "copy is not a verdict" "status=$(status)"
    fi
    wdotool key Escape; finished >/dev/null
fi

# --- the response box -------------------------------------------------------
# Typed text rides back to the caller on stderr. On approval it must land before anything the
# command itself writes, which is what printing it pre-exec buys.
launch /bin/echo ran-after; settled
click response
wdotool type "use -n next time"
click approve
if finished && [[ $(status) == 0 ]] \
   && [[ $(gatelog | grep -n "^User response: use -n next time$" | cut -d: -f1) -lt \
         $(gatelog | grep -n "^ran-after$" | cut -d: -f1) ]]; then
    ok "an approved response prints before the command's own output"
else
    bad "approved response" "status=$(status); $(gatelog | grep -E 'User response|ran-after')"
fi

launch /bin/echo should-not-run; settled
click response
wdotool type "not this one"
wdotool key Escape
if finished && [[ $(status) == 125 ]] \
   && [[ $(gatelog | tail -2 | head -1) == "User denied sudo :(" ]] \
   && [[ $(gatelog | tail -1) == "User response: not this one" ]]; then
    ok "Escape while typing denies, with the response on the line after the denial"
else
    bad "denied response" "status=$(status); $(gatelog | tail -3 | tr '\n' '|')"
fi

launch /bin/echo enter-while-typing; settled
click response
wdotool type "typed then enter"
wdotool key Return
if finished && [[ $(status) == 0 ]] && gatelog | grep -q "^enter-while-typing$" \
   && [[ $(gatelog | grep -c "^User response: typed then enter$") == 1 ]] \
   && [[ $(gatelog | grep -c "verdict: Approved") == 1 ]]; then
    ok "Enter reaches the prompt rather than the focused entry, and answers once"
else
    bad "enter while typing" "status=$(status); $(gatelog | grep -cE 'verdict:') verdicts"
fi

# Nothing typed: the output has to be what it was before the box existed.
launch /bin/echo empty-box; settled
click response
wdotool key Return
if finished && [[ $(status) == 0 ]] && ! gatelog | grep -q "User response:"; then
    ok "an empty box adds nothing to an approval"
else
    bad "empty box approval" "status=$(status); $(gatelog | grep 'User response')"
fi

launch /bin/echo should-not-run; settled
wdotool key Escape
if finished && [[ $(status) == 125 ]] && [[ $(gatelog | tail -1) == "User denied sudo :(" ]]; then
    ok "an empty box adds nothing to a denial"
else
    bad "empty box denial" "status=$(status); $(gatelog | tail -1)"
fi

# A pasted newline stays in the buffer — GTK does not strip it from a single-line entry — so what
# keeps the printed line to one line is the escaping, and that is what this checks.
if command -v wl-copy >/dev/null; then
    printf 'first line\nsecond line\n' | wl-copy
    launch /bin/echo pasted; settled
    click response
    wdotool key ctrl+v
    # The one place a sleep is unavoidable: a clipboard transfer is a Wayland roundtrip with no
    # marker in the log, and answering before it lands would test nothing.
    sleep 0.5
    wdotool key Return
    if finished && [[ $(status) == 0 ]] \
       && [[ $(gatelog | grep -c "^User response:") == 1 ]] \
       && [[ $(gatelog | grep "^User response:") == 'User response: first line\x0asecond line\x0a' ]]
    then
        ok "a multi-line paste still prints as one escaped line"
    else
        bad "pasted response" "status=$(status); $(gatelog | grep 'User response')"
    fi
fi

# The entry is insensitive until the controls are live, so it cannot take focus and typing at it
# is swallowed like every other key — including for the settle cap, which this must not trip.
launch /bin/echo late-typing; presented
click response
wdotool type "ghost"
if settled && [[ -z $(status) ]]; then
    click response
    wdotool type "real"
    wdotool key Return
    if finished && [[ $(status) == 0 ]] && gatelog | grep -q "^User response: real$"; then
        ok "the entry takes nothing before the controls are live"
    else
        bad "pre-settle typing" "status=$(status); $(gatelog | grep 'User response')"
    fi
else
    bad "pre-settle typing" "prompt did not settle: status=$(status)"
fi

# --- the locks --------------------------------------------------------------
# A second request queues on the flock instead of failing, and takes its turn when the first ends.
launch /bin/echo first; settled
watch second; launch /bin/echo second
if wait_log "another sudo-prompt is active"; then
    ok "a concurrent request queues on the flock"
else
    bad "concurrent flock" "$(gatelog)"
fi
watch gate; wdotool key Escape; finished >/dev/null
watch second
if settled; then
    wdotool key Escape
    if finished && [[ $(status) == 125 ]] && gatelog | grep -q "User denied sudo :("; then
        ok "the queued request presents once the first is answered"
    else
        bad "queued request verdict" "status=$(status)"
    fi
else
    bad "queued request" "never presented: $(gatelog)"
fi
watch gate

# A second lock client cannot be covered: the gate must fail fast rather than park on the lock.
swaymsg exec "env RUST_LOG=debug $GENERIC --surface session-lock --title 'other lock client' \
    --body holding >$DIR/other.log 2>&1" >/dev/null
for _ in $(seq 1 40); do
    grep -q "session locked" "$DIR/other.log" 2>/dev/null && break
    sleep 0.25
done
out=$(env SUDO_UID="$(id -u)" SUDO_GID="$(id -g)" SUDO_PROMPT_TEST_DISPLAY_ROOT="$DIR" \
    SUDO_PROMPT_TEST_LOCK_PATH="$DIR/lock" "$GATE" -- /bin/echo nope 2>&1)
if [[ $? == 125 ]] && grep -q "session lock" <<<"$out"; then
    ok "the gate fails closed when another client holds the session lock"
else
    bad "held session lock" "$out"
fi
# Dismiss the other client once it will take the Escape, and wait for the lock to be free again.
for _ in $(seq 1 40); do
    grep -q "controls live" "$DIR/other.log" 2>/dev/null && break
    sleep 0.25
done
wdotool key Escape
for _ in $(seq 1 40); do
    grep -q "session unlocked" "$DIR/other.log" 2>/dev/null && break
    sleep 0.25
done

# --- signals ----------------------------------------------------------------
launch /bin/echo should-not-run; settled
kill -TERM $(gate_pids)
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "signal 15"; then
    ok "SIGTERM denies"
else
    bad "sigterm" "status=$(status)"
fi
# The session must be left unlocked: the next lock has to succeed.
launch /bin/echo unlocked-check
if settled && grep -q "session locked" "$GATE_LOG"; then
    ok "every exit path leaves the session unlocked"
else
    bad "unlock discipline" "the next lock did not succeed"
fi
wdotool key Escape; finished >/dev/null

# --- what the approved command inherits -------------------------------------
# A caller-side locale and a second terminal variable, neither of which may reach root.
LAUNCH_ENV="LANG=tr_TR.UTF-8 LC_ALL=tr_TR.UTF-8 LC_NUMERIC=de_DE.UTF-8 LANGUAGE=de \
    COLORTERM=truecolor TERM=xterm-256color"
launch /bin/sh -c 'ls /proc/self/fd; env'; settled
wdotool key Return; finished
LAUNCH_ENV=""
if gatelog | grep -qE "WAYLAND_(SOCKET|DISPLAY)|XDG_RUNTIME_DIR"; then
    bad "command environment" "a display variable leaked to the approved command"
else
    ok "no display variables reach the approved command"
fi
# The caller's locale silently changes number formatting, collation and gettext output inside
# whatever root runs, so the command gets a root-controlled one instead.
if gatelog | grep -qx "LANG=C.UTF-8" && ! gatelog | grep -qE "^(LANGUAGE|LC_[A-Z]+)="; then
    ok "the command's locale is root-controlled"
else
    bad "command locale" "$(gatelog | grep -E '^(LANG|LANGUAGE|LC_)' | tr '\n' ' ')"
fi
# TERM is the one inherited variable, and the only one of its family.
if gatelog | grep -qx "TERM=xterm-256color" && ! gatelog | grep -qE "^(COLORTERM|TERMINFO|TERMCAP)="
then
    ok "TERM is forwarded and nothing else in its family is"
else
    bad "command TERM" "$(gatelog | grep -E '^(TERM|COLORTERM)' | tr '\n' ' ')"
fi
# /bin/sh's own fds plus the dirfd ls opens; the Wayland fd must not be among them.
if [[ $(gatelog | grep -cE "^[0-9]+$") -le 4 ]]; then
    ok "the approved command inherits no extra file descriptors"
else
    bad "command fds" "$(gatelog | grep -E '^[0-9]+$' | tr '\n' ' ')"
fi

# --- the generic presenter's opt-in box -------------------------------------
# Layer shell rather than a toplevel: it covers the output at 0,0, so the geometry lines are
# screen coordinates here exactly as they are under the lock.
launch_generic() {
    rm -f "$GATE_LOG" "$GATE_STATUS"
    local args="" a
    for a in "$@"; do args+=" $(printf '%q' "$a")"; done
    swaymsg exec "env $LAUNCH_ENV sh -c '$GENERIC --surface layer --verbose$args >$GATE_LOG 2>&1; \
        echo \$? >$GATE_STATUS'" >/dev/null
}

watch generic
launch_generic --title no-box --body "the default is no entry"
if settled && ! gatelog | grep -q "geometry: response"; then
    ok "permission-prompt has no response box without the flag"
else
    bad "generic default" "$(gatelog | grep geometry: | tr '\n' '|')"
fi
wdotool key Escape; finished >/dev/null

launch_generic --response --title box --body "type something"
settled
click response
wdotool type "allowed with a note"
wdotool key Return
if finished && [[ $(status) == 0 ]] && [[ $(gatelog | tail -1) == "User response: allowed with a note" ]]
then
    ok "permission-prompt --response carries the text out on approval"
else
    bad "generic approve" "status=$(status); $(gatelog | tail -1)"
fi

launch_generic --response --title box --body "type something"
settled
click response
wdotool type "denied with a note"
wdotool key Escape
if finished && [[ $(status) == 1 ]] && [[ $(gatelog | tail -1) == "User response: denied with a note" ]]
then
    ok "permission-prompt --response carries the text out on denial too"
else
    bad "generic deny" "status=$(status); $(gatelog | tail -1)"
fi

# --- a user stylesheet cannot flatten the prompt -----------------------------
# GTK loads ~/.config/gtk-4.0/gtk.css above an application's own provider, and @importing a theme
# from exactly there is how a GTK3-era theme gets applied to GTK4 at all. Themes open with a reset
# — Sweet's is `* { padding: 0 }` — which used to leave every field hard against the border.
mkdir -p "$DIR/themed-home/.config/gtk-4.0"
echo '* { padding: 0; }' >"$DIR/themed-home/.config/gtk-4.0/gtk.css"
LAUNCH_ENV="HOME=$DIR/themed-home"
launch_generic --title inset --body "the theme does not own the layout"
settled
read -r dx dy _ _ <<<"$(geom dialog)"
read -r px py _ _ <<<"$(geom prominent)"
if [[ -n ${py:-} && -n ${dy:-} ]] && (( px - dx >= 10 && py - dy >= 10 )); then
    ok "a user stylesheet's padding reset does not reach the dialog"
else
    bad "themed padding" "dialog $dx,$dy vs prominent ${px:-?},${py:-?}"
fi
wdotool key Escape; finished >/dev/null
LAUNCH_ENV=""
watch gate

# --- minimize and the chip --------------------------------------------------
# Minimizing hands the desktop back: the lock goes, a chip appears in the corner of the output, and
# the gate keeps its flock and its pending decision.
launch /bin/echo minimize-me; settled
# shellcheck disable=SC2046
wdotool mousemove $(center minimize) click 1
if chipped && gatelog | grep -q "session unlocked" && [[ -z $(status) ]]; then
    ok "minimize unlocks the session and puts a chip on the output"
else
    bad "minimize" "status=$(status)"
fi

# The desktop is not merely visible but usable: a layer client mapped now takes the keyboard, which
# under a lock surface it could not. The chip asks for no keyboard at all, so the Escape below can
# only reach the client that did.
rm -f "$DIR/desktop.log"
swaymsg exec "env RUST_LOG=debug $GENERIC --surface layer --title 'desktop check' \
    --body 'the desktop is usable again' >$DIR/desktop.log 2>&1" >/dev/null
if wait_other "$DIR/desktop.log" "controls live"; then
    wdotool key Escape
    if wait_other "$DIR/desktop.log" "verdict: Denied" && [[ -z $(status) ]]; then
        ok "the desktop takes the keyboard back while the prompt is minimized"
    else
        bad "minimized desktop input" "gate status=$(status)"
    fi
else
    bad "minimized desktop input" "the layer client never settled"
fi

# Anywhere but the X expands, and the prompt that comes back is a fresh lock with a fresh quiet
# period — which is what makes baiting a double-click useless.
# shellcheck disable=SC2046
wdotool mousemove $(center chip-body) click 1
if wait_log "controls live" 2 && gatelog | grep -q "expanded" \
    && [[ $(grep -c "session locked" "$DIR/gate.log") == 2 ]] \
    && [[ $(grep -c "controls settling" "$DIR/gate.log") == 2 ]]; then
    ok "a click on the chip body relocks the session and settles again"
else
    bad "chip expand" "status=$(status); log: $(gatelog | tail -3)"
fi
wdotool key Escape
if finished && [[ $(status) == 125 ]]; then
    ok "the expanded prompt answers as it did before"
else
    bad "expanded prompt answers" "status=$(status)"
fi

launch /bin/echo chip-denies; settled
# shellcheck disable=SC2046
wdotool mousemove $(center minimize) click 1; chipped
# shellcheck disable=SC2046
wdotool mousemove $(center chip-cancel) click 1
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "User denied sudo :(" \
    && ! gatelog | grep -q "expanded"; then
    ok "the chip's X denies rather than expanding"
else
    bad "chip deny" "status=$(status)"
fi

# Someone else takes the only session lock while the prompt is minimized. The press is held across
# the theft so the release still reaches the chip — the compositor keeps delivering to the surface a
# press began on — and the relock then has to fail closed.
launch /bin/echo relock-fails; settled
# shellcheck disable=SC2046
wdotool mousemove $(center minimize) click 1; chipped
read -r bx by <<<"$(center chip-body)"
wdotool mousemove "$bx" "$by" mousedown 1
rm -f "$DIR/rival.log"
swaymsg exec "env RUST_LOG=debug $GENERIC --surface session-lock --title 'other lock client' \
    --body holding >$DIR/rival.log 2>&1" >/dev/null
wait_other "$DIR/rival.log" "session locked"
wdotool mouseup 1
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "could not take the session lock"; then
    ok "expanding onto a session lock somebody else took fails closed"
else
    bad "relock failure" "status=$(status); log: $(gatelog | tail -3)"
fi
wait_other "$DIR/rival.log" "controls live"
wdotool key Escape
wait_other "$DIR/rival.log" "session unlocked"

# Minimize is dead while the prompt settles, exactly like the other two controls.
launch /bin/echo settle-check; presented
# shellcheck disable=SC2046
wdotool mousemove $(center minimize) click 1
if settled && [[ -z $(status) ]] && ! gatelog | grep -q "minimized"; then
    ok "minimize does nothing during the quiet period"
else
    bad "minimize while settling" "status=$(status)"
fi
wdotool key Escape; finished >/dev/null

# --- hotplug ----------------------------------------------------------------
launch /bin/echo hotplug; settled
swaymsg create_output >/dev/null
if wait_log "lock surface for monitor" 2; then
    ok "a hotplugged output gets its own lock surface"
else
    bad "hotplug add" "$(grep -c 'lock surface' "$GATE_LOG") surfaces"
fi
# Wait for the gate to see the new surface before unplugging it, or the unplug races the plug.
wait_log "surface presented" 2
swaymsg output HEADLESS-2 unplug >/dev/null
wait_log "monitor removed"; sleep 0.5
if [[ -z $(status) ]]; then
    ok "losing one output of two keeps the prompt"
else
    bad "hotplug remove" "status=$(status); log: $(gatelog | tail -3)"
fi
swaymsg output HEADLESS-1 unplug >/dev/null
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "no outputs left"; then
    ok "losing the last output denies rather than holding the locks"
else
    bad "last output lost" "status=$(status)"
fi

# --- outputs while minimized ------------------------------------------------
# The section above left the session with no outputs at all.
swaymsg create_output >/dev/null
for _ in $(seq 1 40); do [[ -n $(outputs) ]] && break; sleep 0.25; done

launch /bin/echo chip-hotplug; settled
# shellcheck disable=SC2046
wdotool mousemove $(center minimize) click 1; chipped
swaymsg create_output >/dev/null
if wait_log "chip presented" 2; then
    ok "a hotplugged output gets its own chip"
else
    bad "chip hotplug add" "$(grep -c 'chip presented' "$DIR/gate.log") chips"
fi
first=$(outputs | head -1)
swaymsg output "$(outputs | tail -1)" unplug >/dev/null
wait_log "monitor removed"; sleep 0.5
if [[ -z $(status) ]]; then
    ok "losing one output of two keeps the chip"
else
    bad "chip hotplug remove" "status=$(status); log: $(gatelog | tail -3)"
fi
swaymsg output "$first" unplug >/dev/null
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "no outputs left"; then
    ok "losing the last output while minimized denies rather than waiting"
else
    bad "last output lost while minimized" "status=$(status)"
fi

echo
echo "$pass passed, $fail failed"
[[ $fail == 0 ]]
