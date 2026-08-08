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

# Points inside the fixed `/bin/echo coord` request's dialog on a 1280x720 output: the Approve
# button's centre, and the two ends of a drag across the command field. The dialog is centred at
# its natural height, so *anything* that changes which fields that request renders moves these —
# they dropped 27px when the env field stopped being drawn for a request that sets no variables.
# A positive click test runs first, so a stale coordinate fails loudly instead of silently passing.
APPROVE_X=791
APPROVE_Y=412
COMMAND_X1=438
COMMAND_Y1=300
COMMAND_X2=700
COMMAND_Y2=336

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

# Launch the gate inside the session and return once its prompt has settled.
#
# From `/`, always: the prompt shows the caller's cwd, so launching from the checkout would make the
# dialog's width — and every coordinate above — depend on where the repo happens to live.
launch() {
    rm -f "$DIR/gate.log" "$DIR/gate.status"
    local args="" a
    for a in "$@"; do args+=" $(printf '%q' "$a")"; done
    swaymsg exec "env $LAUNCH_ENV SUDO_UID=$(id -u) SUDO_GID=$(id -g) \
        SUDO_PROMPT_TEST_DISPLAY_ROOT=$DIR SUDO_PROMPT_TEST_LOCK_PATH=$DIR/lock RUST_LOG=debug \
        sh -c 'cd /; $GATE --$args >$DIR/gate.log 2>&1; echo \$? >$DIR/gate.status'" >/dev/null
}

# Wait for the prompt to be answerable, or for the run to have finished.
settled() {
    local i
    for i in $(seq 1 40); do
        [[ -f $DIR/gate.status ]] && return 0
        grep -q "surface presented" "$DIR/gate.log" 2>/dev/null && { sleep 1.2; return 0; }
        sleep 0.25
    done
    return 1
}

finished() {
    local i
    for i in $(seq 1 40); do
        [[ -f $DIR/gate.status ]] && return 0
        sleep 0.25
    done
    return 1
}

status() { cat "$DIR/gate.status" 2>/dev/null; }
gatelog() { cat "$DIR/gate.log" 2>/dev/null; }

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
wdotool mousemove $APPROVE_X $APPROVE_Y click 1
if finished && [[ $(status) == 0 ]]; then
    ok "a click on Approve after settling approves"
else
    bad "click approves" "status=$(status) — is $APPROVE_X,$APPROVE_Y still the Approve button?"
fi

launch /bin/echo coord
wdotool mousemove $APPROVE_X $APPROVE_Y mousedown 1
settled
wdotool mouseup 1; sleep 0.5
if [[ -z $(status) ]]; then
    ok "a press begun during settling does nothing when released over Approve"
else
    bad "press across settling" "status=$(status)"
fi
wdotool mousemove $APPROVE_X $APPROVE_Y click 1; finished >/dev/null

# --- selecting and copying --------------------------------------------------
if command -v wl-paste >/dev/null; then
    launch /bin/echo coord; settled
    wdotool mousemove $COMMAND_X1 $COMMAND_Y1 mousedown 1 \
        mousemove $COMMAND_X2 $COMMAND_Y2 mouseup 1
    wdotool key ctrl+c
    sleep 0.5
    # One label per field, so a single drag takes the whole command, not just the line under it.
    if [[ $(timeout 3 wl-paste -n 2>/dev/null) == "/bin/echo
coord" ]]; then
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

# --- the locks --------------------------------------------------------------
launch /bin/echo first; settled
out=$(env SUDO_UID="$(id -u)" SUDO_GID="$(id -g)" SUDO_PROMPT_TEST_DISPLAY_ROOT="$DIR" \
    SUDO_PROMPT_TEST_LOCK_PATH="$DIR/lock" "$GATE" -- /bin/echo second 2>&1)
if [[ $? == 125 ]] && grep -q "already holds" <<<"$out"; then
    ok "a concurrent request fails closed on the flock"
else
    bad "concurrent flock" "$out"
fi
wdotool key Escape; finished >/dev/null

# A second lock client cannot be covered: the gate must fail fast rather than park on the lock.
swaymsg exec "$GENERIC --surface session-lock --title 'other lock client' --body holding \
    >$DIR/other.log 2>&1" >/dev/null
sleep 2
out=$(env SUDO_UID="$(id -u)" SUDO_GID="$(id -g)" SUDO_PROMPT_TEST_DISPLAY_ROOT="$DIR" \
    SUDO_PROMPT_TEST_LOCK_PATH="$DIR/lock" "$GATE" -- /bin/echo nope 2>&1)
if [[ $? == 125 ]] && grep -q "session lock" <<<"$out"; then
    ok "the gate fails closed when another client holds the session lock"
else
    bad "held session lock" "$out"
fi
wdotool key Escape; sleep 1

# --- signals ----------------------------------------------------------------
launch /bin/echo should-not-run; settled
pkill -TERM -x sudo-prompt
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "signal 15"; then
    ok "SIGTERM denies"
else
    bad "sigterm" "status=$(status)"
fi
# The session must be left unlocked: the next lock has to succeed.
launch /bin/echo unlocked-check
if settled && grep -q "session locked" "$DIR/gate.log"; then
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

# --- hotplug ----------------------------------------------------------------
launch /bin/echo hotplug; settled
swaymsg create_output >/dev/null; sleep 2
if [[ $(grep -c "lock surface for monitor" "$DIR/gate.log") == 2 ]]; then
    ok "a hotplugged output gets its own lock surface"
else
    bad "hotplug add" "$(grep -c 'lock surface' "$DIR/gate.log") surfaces"
fi
swaymsg output HEADLESS-2 unplug >/dev/null; sleep 2
if [[ -z $(status) ]]; then
    ok "losing one output of two keeps the prompt"
else
    bad "hotplug remove" "status=$(status)"
fi
swaymsg output HEADLESS-1 unplug >/dev/null
if finished && [[ $(status) == 125 ]] && gatelog | grep -q "no outputs left"; then
    ok "losing the last output denies rather than holding the locks"
else
    bad "last output lost" "status=$(status)"
fi

echo
echo "$pass passed, $fail failed"
[[ $fail == 0 ]]
