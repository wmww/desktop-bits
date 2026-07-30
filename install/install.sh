#!/bin/bash
# Install and verify the sudo-prompt chain. Run as root.
#
#   install.sh install [--from DIR] [--user NAME]...   install binaries, group and sudoers rule
#   install.sh verify   [--user NAME]...               run the checks, change nothing
#
# A malformed sudoers line fails open into "no rule at all", which under this design means nobody
# can sudo. Keep a root TTY logged in for the whole switch.
set -euo pipefail

GATE=/usr/local/bin/sudo-prompt
SHIM=/usr/local/bin/sudo
GENERIC=/usr/local/bin/permission-prompt
GROUP=sudo-prompt-users
DROPIN=/etc/sudoers.d/sudo-prompt
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

fail() { echo "FAIL: $*" >&2; exit 1; }
ok()   { echo "ok:   $*"; }
warn() { echo "WARN: $*" >&2; }

# --- path integrity ---------------------------------------------------------
# The sudoers rule's safety depends on it: a group-writable directory anywhere on the gate's path
# means somebody other than root chooses what runs as root.
check_path_integrity() {
    local p
    for p in /usr/local /usr/local/bin "$@"; do
        [[ -e $p ]] || fail "$p does not exist"
        local owner mode
        owner=$(stat -c %u "$p")
        mode=$(stat -c %a "$p")
        [[ $owner == 0 ]] || fail "$p is owned by uid $owner, not root"
        # Group or other write bit set anywhere.
        (( 0$mode & 0022 )) && fail "$p is group or other writable (mode $mode)"
        ok "$p is root-owned and not group/other writable (mode $mode)"
    done
}

# --- host requirements ------------------------------------------------------
check_host() {
    # The gate has no fallback, so compositor support is a hard install-time requirement, and the
    # `monitor` signal it uses for per-output lock surfaces needs gtk4-layer-shell >= 1.2.0.
    if pkg-config --atleast-version=1.2.0 gtk4-layer-shell-0 2>/dev/null; then
        ok "gtk4-layer-shell $(pkg-config --modversion gtk4-layer-shell-0) >= 1.2.0"
    else
        fail "gtk4-layer-shell >= 1.2.0 is required (session lock bindings)"
    fi

    if [[ -d /run/user/0 ]]; then
        local mode owner
        mode=$(stat -c %a /run/user/0); owner=$(stat -c %u /run/user/0)
        [[ $owner == 0 ]] || fail "/run/user/0 is owned by uid $owner"
        (( 0$mode & 0022 )) && fail "/run/user/0 is group or other writable (mode $mode)"
        ok "/run/user/0 is root-owned, mode $mode"
        ls /run/user/0/wayland-* >/dev/null 2>&1 \
            && ok "compositor socket present: $(ls /run/user/0/wayland-* | tr '\n' ' ')" \
            || warn "no wayland-N socket in /run/user/0 — the gate will fail until the compositor runs"
    else
        warn "/run/user/0 does not exist yet (root's compositor is not running)"
    fi

    # The approved command must not share the caller's terminal, where another process of the
    # caller's uid could inject input into it via TIOCSTI.
    if sudo -V 2>/dev/null | grep -q .; then :; fi
    if grep -qsE '^[^#]*Defaults.*\buse_pty\b' /etc/sudoers /etc/sudoers.d/* 2>/dev/null; then
        ok "use_pty is configured"
    else
        warn "use_pty not found in sudoers — set 'Defaults use_pty'"
    fi
    if grep -qsE '^[^#]*Defaults.*\bsecure_path=' /etc/sudoers /etc/sudoers.d/* 2>/dev/null; then
        ok "secure_path is configured"
    else
        warn "secure_path not found in sudoers — the gate's captured PATH would not be root-controlled"
    fi
    local tiocsti
    tiocsti=$(sysctl -n dev.tty.legacy_tiocsti 2>/dev/null || echo "unavailable")
    [[ $tiocsti == 0 ]] && ok "dev.tty.legacy_tiocsti=0" || warn "dev.tty.legacy_tiocsti=$tiocsti (want 0)"
}

# --- sudoers ----------------------------------------------------------------
install_dropin() {
    local tmp
    tmp=$(mktemp)
    cp "$HERE/sudoers.d-sudo-prompt" "$tmp"
    chmod 0440 "$tmp"
    visudo -cf "$tmp" >/dev/null || fail "the sudoers drop-in does not parse on this sudo version"
    ok "drop-in parses (visudo -cf)"
    install -o root -g root -m 0440 "$tmp" "$DROPIN"
    rm -f "$tmp"
    ok "installed $DROPIN"
}

# The rule must match a real request and neither of the two shapes that would widen it.
check_matching() {
    local user=$1
    sudo -l -U "$user" >/dev/null 2>&1 || warn "sudo -l -U $user failed; is $user in $GROUP?"
    local listing
    listing=$(sudo -l -U "$user" 2>/dev/null || true)
    grep -qF -- "$GATE -- *" <<<"$listing" \
        && ok "$user: the command-argument pattern is present" \
        || warn "$user: '$GATE -- *' not in sudo -l output"
    # Positive and negative argv matching, which is the check that actually matters.
    if sudo -l -U "$user" "$GATE" -- id >/dev/null 2>&1; then
        ok "$user: '$GATE -- id' matches"
    else
        fail "$user: '$GATE -- id' does not match the rule"
    fi
    if sudo -l -U "$user" "$GATE" id >/dev/null 2>&1; then
        fail "$user: bare '$GATE id' matches — the rule is too wide"
    else
        ok "$user: bare '$GATE id' matches nothing"
    fi
    if sudo -l -U "$user" "$GATE" -- >/dev/null 2>&1; then
        fail "$user: '$GATE --' matches — the rule is too wide"
    else
        ok "$user: '$GATE --' matches nothing"
    fi
}

# root's own rule is what the interpreter path's inner sudo relies on, and sudoers(5) implies SETENV
# for a rule matching ALL — without it that sudo refuses command-line variables.
check_root_rule() {
    local listing
    listing=$(sudo -l -U root 2>/dev/null || true)
    grep -qE '\(ALL(:ALL)?\)\s+ALL' <<<"$listing" \
        && ok "root has a rule matching ALL (implies SETENV)" \
        || warn "root has no ALL rule — the interpreter path's inner sudo will refuse VAR=value"
}

usage() { sed -n '2,9p' "$0"; exit 2; }

# --- main -------------------------------------------------------------------
cmd=${1:-}; shift || usage
from=target/release
users=()
while (( $# )); do
    case $1 in
        --from) from=$2; shift 2 ;;
        --user) users+=("$2"); shift 2 ;;
        *) usage ;;
    esac
done

[[ $(id -u) == 0 ]] || fail "run as root"

case $cmd in
install)
    for b in sudo-prompt sudo-shim permission-prompt; do
        [[ -x $from/$b ]] || fail "$from/$b not built (cargo build --release)"
    done
    check_path_integrity
    check_host
    install -o root -g root -m 0755 "$from/sudo-prompt" "$GATE"
    install -o root -g root -m 0755 "$from/sudo-shim" "$SHIM"
    install -o root -g root -m 0755 "$from/permission-prompt" "$GENERIC"
    ok "installed $GATE, $SHIM, $GENERIC"
    check_path_integrity "$GATE" "$SHIM"
    getent group "$GROUP" >/dev/null || groupadd "$GROUP"
    ok "group $GROUP exists"
    for u in "${users[@]}"; do
        gpasswd -a "$u" "$GROUP" >/dev/null
        ok "added $u to $GROUP"
    done
    install_dropin
    check_root_rule
    for u in "${users[@]}"; do check_matching "$u"; done
    echo
    echo "Now prove the path end to end from every member uid before removing the old chain."
    echo "See notes/sudo-prompt-operations.md for the rest of the rollout and the recovery drill."
    ;;
verify)
    check_path_integrity "$GATE" "$SHIM"
    check_host
    [[ -f $DROPIN ]] && { visudo -cf "$DROPIN" >/dev/null && ok "$DROPIN parses"; } || warn "$DROPIN missing"
    check_root_rule
    if [[ ${#users[@]} -eq 0 ]]; then
        mapfile -t users < <(getent group "$GROUP" | cut -d: -f4 | tr ',' '\n' | grep -v '^$' || true)
    fi
    for u in "${users[@]}"; do check_matching "$u"; done
    ;;
*) usage ;;
esac
