#!/bin/bash
# Verify a sudo-prompt setup against what the README describes. Reads only — this script changes
# nothing. Run as root.
#
#   verify.sh [--group NAME] [--user NAME]...
#
# --group is the sudo group named in the sudoers rule (default wheel). With no --user it checks
# every member of that group.
#
# Exit 0 if every check passed, 1 if any FAILed. WARNs do not affect the exit status: they are
# either host settings this repo does not own or states that are only wrong at the wrong moment.
set -uo pipefail

GATE=/usr/local/bin/sudo-prompt
SHIM=/usr/local/bin/sudo-shim
SUDO_LINK=/usr/local/bin/sudo
GENERIC=/usr/local/bin/permission-prompt
# Always the real sudo: a verify script must not run through the shim it is verifying.
REAL_SUDO=/usr/bin/sudo
GROUP=wheel

fails=0
warns=0
fail() { echo "FAIL: $*" >&2; fails=$((fails + 1)); return 1; }
ok()   { echo "ok:   $*"; return 0; }
warn() { echo "WARN: $*" >&2; warns=$((warns + 1)); return 0; }

usage() { sed -n '2,10p' "$0"; exit 2; }

# Every sudoers file, for the text searches below. visudo -c is what actually validates them.
sudoers_files() {
    local -a files=(/etc/sudoers)
    shopt -s nullglob
    files+=(/etc/sudoers.d/*)
    shopt -u nullglob
    printf '%s\n' "${files[@]}"
}

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
    # Only the recovery drill needs the generic presenter, so its absence is not a broken setup.
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

# /usr/local/bin/sudo is a symlink to the shim. Nothing in this repo creates or removes it, because
# it is the switch that routes real traffic — see the README.
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

    local -a files
    mapfile -t files < <(sudoers_files)

    # The approved command must not share the caller's terminal, where another process of the
    # caller's uid could inject input into it via TIOCSTI.
    if grep -qsE '^[^#]*Defaults.*\buse_pty\b' "${files[@]}"; then
        ok "use_pty is configured"
    else
        warn "use_pty not found in sudoers — set 'Defaults use_pty'"
    fi
    # Without secure_path the gate captures a caller-chosen PATH and resolves the approved command
    # against it, so the prompt could name `id` while root runs somebody else's.
    if grep -qsE '^[^#]*Defaults.*\bsecure_path=' "${files[@]}"; then
        ok "secure_path is configured"
    else
        warn "secure_path not found in sudoers — the gate's captured PATH would not be root-controlled"
    fi
    # The gate's rule carries no SETENV (it is implied only for a rule matching ALL), but a global
    # `Defaults setenv` would grant it anyway and let the caller hand the gate its own PATH.
    local setenv_lines
    setenv_lines=$(grep -hsE '^[^#]*Defaults.*\bsetenv\b' "${files[@]}" | grep -v '!setenv')
    [[ -z $setenv_lines ]] && ok "setenv is not globally enabled" \
        || warn "'Defaults setenv' is enabled — the caller can set the gate's environment; add NOSETENV to the rule"

    local tiocsti
    tiocsti=$(sysctl -n dev.tty.legacy_tiocsti 2>/dev/null || echo "unavailable")
    [[ $tiocsti == 0 ]] && ok "dev.tty.legacy_tiocsti=0" || warn "dev.tty.legacy_tiocsti=$tiocsti (want 0)"
}

# --- sudoers ----------------------------------------------------------------
# The authoritative check is the per-user argv matching in check_matching; this locates the rule so
# a human can see where it lives and whether anything else grants the gate more widely.
check_sudoers_rule() {
    if visudo -c >/dev/null 2>&1; then
        ok "the sudoers configuration parses (visudo -c)"
    else
        fail "the sudoers configuration does not parse — run 'visudo -c' for details"
    fi

    local -a files
    mapfile -t files < <(sudoers_files)
    local rule="%$GROUP ALL=(root) NOPASSWD: $GATE -- *"
    local -a found
    mapfile -t found < <(grep -lF -- "$rule" "${files[@]}" 2>/dev/null)

    if (( ${#found[@]} == 0 )); then
        warn "the expected rule text was not found verbatim in any sudoers file:"$'\n'"      $rule"
        warn "  (spelling may differ harmlessly — the per-user matching checks below are what decide)"
    else
        local f owner mode
        for f in "${found[@]}"; do
            ok "the rule is in $f"
            owner=$(stat -c %u "$f"); mode=$(stat -c %a "$f")
            if [[ $owner != 0 ]]; then
                fail "$f is owned by uid $owner, not root — sudo will ignore it"
            elif (( 0$mode & 0022 )); then
                # sudo ignores a sudoers file that is group- or other-writable, which under this
                # design fails closed into "nobody can sudo".
                fail "$f is group or other writable (mode $mode) — sudo will ignore it"
            else
                ok "$f is root-owned, mode $mode"
            fi
        done
        (( ${#found[@]} > 1 )) && warn "the rule appears in ${#found[@]} files; one of them is redundant"
    fi

    # Any other mention of the gate is a second rule that may be wider than the one above.
    local others
    others=$(grep -nsHF -- "$GATE" "${files[@]}" | grep -vF -- "$rule" | grep -v ':\s*#' | sed 's/^/      /')
    [[ -z $others ]] && ok "no other sudoers line mentions $GATE" \
        || warn "other sudoers lines mention $GATE:"$'\n'"$others"
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
    listing=$("$REAL_SUDO" -l -U "$user" 2>/dev/null) || warn "sudo -l -U $user failed; does $user exist?"
    # Positive and negative argv matching, which is the check that actually matters.
    if "$REAL_SUDO" -l -U "$user" "$GATE" -- id >/dev/null 2>&1; then
        ok "$user: '$GATE -- id' matches"
    else
        fail "$user: '$GATE -- id' does not match the rule"
    fi
    if "$REAL_SUDO" -l -U "$user" "$GATE" id >/dev/null 2>&1; then
        fail "$user: bare '$GATE id' matches — the rule is too wide"
    else
        ok "$user: bare '$GATE id' matches nothing"
    fi
    if "$REAL_SUDO" -l -U "$user" "$GATE" -- >/dev/null 2>&1; then
        fail "$user: '$GATE --' matches — the rule is too wide"
    else
        ok "$user: '$GATE --' matches nothing"
    fi

    # The gate is only the sole path to root if nothing else is passwordless for this uid. A
    # leftover rule from whatever chain this replaces would be exactly that.
    local others
    others=$(grep NOPASSWD <<<"$listing" | grep -vF -- "$GATE" | sed 's/^\s*/      /')
    [[ -z $others ]] && ok "$user: the gate is the only NOPASSWD entry" \
        || warn "$user: other NOPASSWD entries bypass the gate:"$'\n'"$others"
}

# root's own rule is what the interpreter path's inner sudo relies on, and sudoers(5) implies SETENV
# for a rule matching ALL — without it that sudo refuses command-line variables.
check_root_rule() {
    local listing
    listing=$("$REAL_SUDO" -l -U root 2>/dev/null || true)
    grep -qE '\(ALL(:ALL)?\)\s+ALL' <<<"$listing" \
        && ok "root has a rule matching ALL (implies SETENV)" \
        || warn "root has no ALL rule — the interpreter path's inner sudo will refuse VAR=value"
}

# --- main -------------------------------------------------------------------
users=()
while (( $# )); do
    case $1 in
        --group) [[ ${2:-} ]] || usage; GROUP=$2; shift 2 ;;
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
check_sudoers_rule
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
