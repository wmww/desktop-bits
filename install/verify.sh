#!/bin/bash
# Verify a sudo-prompt installation. Reads only — this script changes nothing. Run as root.
#
#   verify.sh [--user NAME]...
#
# With no --user it checks every member of the sudo-prompt-users group. Installation itself is
# documented in README.md; nothing here installs, and nothing here needs to be run to install.
#
# Exit 0 if every check passed, 1 if any FAILed. WARNs do not affect the exit status: they are
# either host settings this repo does not own or states that are only wrong at the wrong moment.
set -uo pipefail

GATE=/usr/local/bin/sudo-prompt
SHIM=/usr/local/bin/sudo-shim
SUDO_LINK=/usr/local/bin/sudo
GENERIC=/usr/local/bin/permission-prompt
GROUP=sudo-prompt-users
DROPIN=/etc/sudoers.d/sudo-prompt
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

fails=0
warns=0
fail() { echo "FAIL: $*" >&2; fails=$((fails + 1)); return 1; }
ok()   { echo "ok:   $*"; return 0; }
warn() { echo "WARN: $*" >&2; warns=$((warns + 1)); return 0; }

usage() { sed -n '2,8p' "$0"; exit 2; }

# --- path integrity ---------------------------------------------------------
# The sudoers rule's safety depends on it: a group-writable directory anywhere on the gate's path,
# or a non-root owner, means somebody other than root chooses what runs as root.
check_path_integrity() {
    local p owner mode
    for p in "$@"; do
        [[ -e $p ]] || { fail "$p does not exist"; continue; }
        owner=$(stat -c %u "$p")
        mode=$(stat -c %a "$p")
        [[ $owner == 0 ]] || { fail "$p is owned by uid $owner, not root"; continue; }
        # Group or other write bit set.
        if (( 0$mode & 0022 )); then
            fail "$p is group or other writable (mode $mode)"
            continue
        fi
        ok "$p is root-owned and not group/other writable (mode $mode)"
    done
}

# The design puts privilege in the sudoers rule, never in a file mode. A setuid bit on either
# binary would be a second, unreviewed way to reach root.
check_not_setuid() {
    local p mode
    for p in "$@"; do
        [[ -e $p ]] || continue
        mode=$(stat -c %a "$p")
        if (( 0$mode & 06000 )); then
            fail "$p is setuid/setgid (mode $mode)"
        else
            ok "$p is not setuid/setgid"
        fi
    done
}

check_binaries() {
    [[ -x $GATE ]] && ok "$GATE is executable" || fail "$GATE is missing or not executable"
    [[ -x $SHIM ]] && ok "$SHIM is executable" || fail "$SHIM is missing or not executable"
    # Only the recovery drill needs the generic presenter, so its absence is not a broken install.
    [[ -x $GENERIC ]] && ok "$GENERIC is executable" \
        || warn "$GENERIC is missing — the abandoned-lock recovery drill needs it"

    # A library the gate cannot resolve at runtime means every sudo fails at the worst moment.
    if [[ -x $GATE ]] && command -v ldd >/dev/null; then
        local missing
        missing=$(ldd "$GATE" 2>/dev/null | grep 'not found' | awk '{print $1}' | tr '\n' ' ')
        [[ -z $missing ]] && ok "$GATE resolves all its shared libraries" \
            || fail "$GATE cannot resolve: $missing"
    fi
}

# /usr/local/bin/sudo is a symlink to the shim, created by hand (see README.md). Nothing in this
# repo creates or removes it, because it is the switch that routes real traffic.
check_sudo_link() {
    if [[ ! -e $SUDO_LINK && ! -L $SUDO_LINK ]]; then
        warn "$SUDO_LINK does not exist — callers still reach the real sudo directly, so the gate is not in the path yet"
        return
    fi
    if [[ ! -L $SUDO_LINK ]]; then
        fail "$SUDO_LINK is not a symlink — expected a symlink to $SHIM"
        return
    fi
    local owner target
    # Deliberately not -L: we want the link's own owner. Symlink modes are always 0777 on Linux and
    # are ignored, so only the containing directory's mode matters and that is checked above.
    owner=$(stat -c %u "$SUDO_LINK")
    [[ $owner == 0 ]] || fail "$SUDO_LINK is owned by uid $owner, not root"
    target=$(readlink -f "$SUDO_LINK")
    [[ $target == "$SHIM" ]] && ok "$SUDO_LINK -> $SHIM" \
        || fail "$SUDO_LINK resolves to $target, not $SHIM"
}

# --- host requirements ------------------------------------------------------
check_host() {
    # The gate has no fallback, so compositor support is a hard requirement, and the `monitor`
    # signal it uses for per-output lock surfaces needs gtk4-layer-shell >= 1.2.0.
    if ! command -v pkg-config >/dev/null; then
        warn "pkg-config not installed — cannot check the gtk4-layer-shell version (want >= 1.2.0)"
    elif pkg-config --atleast-version=1.2.0 gtk4-layer-shell-0 2>/dev/null; then
        ok "gtk4-layer-shell $(pkg-config --modversion gtk4-layer-shell-0) >= 1.2.0"
    else
        fail "gtk4-layer-shell >= 1.2.0 is required (session lock bindings)"
    fi

    if [[ -d /run/user/0 ]]; then
        local mode owner
        mode=$(stat -c %a /run/user/0); owner=$(stat -c %u /run/user/0)
        [[ $owner == 0 ]] || fail "/run/user/0 is owned by uid $owner"
        if (( 0$mode & 0022 )); then
            fail "/run/user/0 is group or other writable (mode $mode)"
        else
            ok "/run/user/0 is root-owned, mode $mode"
        fi
        ls /run/user/0/wayland-* >/dev/null 2>&1 \
            && ok "compositor socket present: $(ls /run/user/0/wayland-* | tr '\n' ' ')" \
            || warn "no wayland-N socket in /run/user/0 — the gate will fail until the compositor runs"
    else
        warn "/run/user/0 does not exist yet (root's compositor is not running)"
    fi

    # The approved command must not share the caller's terminal, where another process of the
    # caller's uid could inject input into it via TIOCSTI.
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
check_dropin() {
    if [[ ! -f $DROPIN ]]; then
        fail "$DROPIN is missing"
        return
    fi
    local owner mode
    owner=$(stat -c %u "$DROPIN"); mode=$(stat -c %a "$DROPIN")
    [[ $owner == 0 ]] || fail "$DROPIN is owned by uid $owner, not root"
    # sudo ignores a drop-in that is group- or other-writable, which fails closed into "no rule".
    if (( 0$mode & 0022 )); then
        fail "$DROPIN is group or other writable (mode $mode) — sudo will ignore it"
    else
        ok "$DROPIN is root-owned, mode $mode"
    fi
    visudo -cf "$DROPIN" >/dev/null 2>&1 && ok "$DROPIN parses (visudo -cf)" \
                                         || fail "$DROPIN does not parse on this sudo version"

    # Installation is by hand, so the installed rule can drift from the one in this repo.
    local src=$HERE/sudoers.d-sudo-prompt delta
    if [[ -f $src ]]; then
        delta=$(diff <(grep -vE '^\s*(#|$)' "$src") <(grep -vE '^\s*(#|$)' "$DROPIN"))
        if [[ -z $delta ]]; then
            ok "$DROPIN matches install/sudoers.d-sudo-prompt (comments aside)"
        else
            fail "$DROPIN differs from install/sudoers.d-sudo-prompt:"$'\n'"$delta"
        fi
    fi
}

check_group() {
    local members
    if ! getent group "$GROUP" >/dev/null; then
        fail "group $GROUP does not exist"
        return
    fi
    members=$(getent group "$GROUP" | cut -d: -f4)
    [[ -n $members ]] && ok "group $GROUP exists, members: $members" \
                      || warn "group $GROUP exists but has no members"
}

# The rule must match a real request and neither of the two shapes that would widen it.
check_matching() {
    local user=$1 listing
    listing=$(sudo -l -U "$user" 2>/dev/null) || warn "sudo -l -U $user failed; does $user exist?"
    grep -qF -- "$GATE -- *" <<<"$listing" \
        && ok "$user: the command-argument pattern is present" \
        || warn "$user: '$GATE -- *' not in sudo -l output; is $user in $GROUP?"
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

    # The gate is only the sole path to root if nothing else is passwordless for this uid. A
    # leftover %wheel rule from the chain this replaces would be exactly that.
    local others
    others=$(grep NOPASSWD <<<"$listing" | grep -vF -- "$GATE" | sed 's/^\s*/      /')
    [[ -z $others ]] && ok "$user: the gate is the only NOPASSWD entry" \
        || warn "$user: other NOPASSWD entries bypass the gate:"$'\n'"$others"
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

# --- main -------------------------------------------------------------------
users=()
while (( $# )); do
    case $1 in
        --user) [[ ${2:-} ]] || usage; users+=("$2"); shift 2 ;;
        -h|--help) usage ;;
        *) usage ;;
    esac
done

[[ $(id -u) == 0 ]] || { echo "FAIL: run as root" >&2; exit 1; }

check_path_integrity /usr/local /usr/local/bin "$GATE" "$SHIM"
check_not_setuid "$GATE" "$SHIM"
check_binaries
check_sudo_link
check_host
check_dropin
check_group
check_root_rule

if (( ${#users[@]} == 0 )); then
    mapfile -t users < <(getent group "$GROUP" | cut -d: -f4 | tr ',' '\n' | grep -v '^$')
fi
for u in "${users[@]}"; do check_matching "$u"; done

echo
if (( fails )); then
    echo "$fails check(s) FAILED, $warns warning(s)" >&2
    exit 1
fi
echo "all checks passed, $warns warning(s)"
