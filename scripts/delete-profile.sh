#!/usr/bin/env bash
# Remove a household member's login — and optionally their mail. Pure file
# operations, meant to run INSIDE the backend container (see README → People).
#
#   delete-profile.sh hajni          # asks: remove login? then offers to
#                                    #   delete the mail too, size shown first
#   delete-profile.sh hajni --purge  # profile + all mail data in one go,
#                                    #   typed-username confirmation
#
# Afterward run 'docker compose restart backend' while the stack is up —
# old session cookies of the removed profile die with the roster rebuild.
set -euo pipefail

usage() { echo "usage: $(basename "$0") <user> [--purge]" >&2; exit 2; }

slug="${1:-}"; mode="${2:-}"
[ -n "$slug" ] || usage
[ $# -le 2 ] || usage
if [ -n "$mode" ] && [ "$mode" != "--purge" ]; then usage; fi
[[ "$slug" =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$ ]] \
    || { echo "error: user must be 1-64 chars, letters/digits/_/-, starting alphanumeric" >&2; exit 1; }

if [ -d /profiles ]; then P=/profiles; D=/data; A=/archive; B=/backup
else cd "$(dirname "$0")/.."; P=./profiles; D=./data; A=./archive; B=./backup; fi
if [ ! -d "$P/$slug" ]; then
    echo "error: no profile '$slug'. Existing:" >&2
    if ! ls "$P" 1>&2 2>/dev/null; then echo "  (none)" >&2; fi
    exit 1
fi

total_kb() {
    local t=0 d
    for d in "$@"; do
        [ -d "$d" ] && t=$((t + $(du -sk "$d" | cut -f1)))
    done
    echo "$t"
}
human() {
    awk -v k="$1" 'BEGIN{
        if (k >= 1048576)   printf "%.1fG", k/1048576;
        else if (k >= 1024) printf "%.0fM", k/1024;
        else                printf "%dK",  k }'
}

mail_kb=$(total_kb "$D/$slug" "$A/$slug" "$B/$slug")

if [ "$mode" = "--purge" ]; then
    echo "PURGE '$slug': profile and portrait + all mail data:"
    du -sh "$D/$slug" "$A/$slug" "$B/$slug" 2>/dev/null || true
    echo "This cannot be undone."
    read -rp "Type the username to confirm: " typed
    [ "${typed:-}" = "$slug" ] || { echo "confirmation did not match — nothing deleted" >&2; exit 1; }
    keep_mail=no
else
    echo "Removes '$slug's profile and portrait."
    echo "Their mail is currently $(human "$mail_kb") under $D/$slug (plus archive/ and backup/)."
    read -rp "Proceed with removing the login? [y/N] " ans
    case "${ans:-n}" in [yY]|[yY][eE][sS]) ;; *) echo "aborted" >&2; exit 1 ;; esac
    if [ "$mail_kb" = "0" ]; then
        keep_mail=no
    else
        read -rp "Also delete their mail files ($(human "$mail_kb") will be freed)? [y/N] " ans
        case "${ans:-n}" in [yY]|[yY][eE][sS]) keep_mail=no ;; *) keep_mail=yes ;; esac
    fi
fi

rm -rf "$P/$slug"
if [ "$keep_mail" = no ]; then
    rm -rf "$D/$slug" "$A/$slug" "$B/$slug"
fi

remaining=$(find "$P" -mindepth 2 -name profile.json 2>/dev/null | wc -l | tr -d ' ')
if [ "$remaining" = "0" ]; then
    echo "warning: '$slug' was the last profile — with no valid roster the backend"
    echo "refuses to start (fail-closed). Create a new one before restarting:"
    echo "  ./scripts/new-profile.sh <user>"
fi

if [ "$keep_mail" = no ] && [ "$mail_kb" != "0" ]; then
    echo "done: '$slug' removed; $(human "$mail_kb") freed."
else
    echo "done: '$slug' removed."
fi
echo "Apply it: docker compose restart backend (while the stack is up)."
