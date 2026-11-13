#!/usr/bin/env bash
# Remove a household member's login — and optionally their mail.
#
#   ./scripts/delete-profile.sh hajni            # asks: remove login; then asks
#                                                #   again whether to also delete
#                                                #   data/, archive/ and backup/,
#                                                #   showing the size it frees
#   ./scripts/delete-profile.sh hajni --purge    # no questions asked (beyond the
#                                                #   typed-username safety check):
#                                                #   wipes profile + all mail
#
# Restarting the backend is required for old session cookies to die (the
# tenant roster is built at boot), so the script restarts whenever the stack
# is running. Re-adding someone later = ./scripts/new-profile.sh same-slug
# (an un-deleted archive will be indexed again on next boot).
set -euo pipefail

cd "$(dirname "$0")/.."

usage() { echo "usage: $(basename "$0") <user> [--purge]" >&2; exit 2; }

slug="${1:-}"; mode="${2:-}"
[ -n "$slug" ] || usage
[ $# -le 2 ] || usage
if [ -n "$mode" ] && [ "$mode" != "--purge" ]; then usage; fi
[[ "$slug" =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$ ]] \
    || { echo "error: user must be 1-64 chars, letters/digits/_/-, starting alphanumeric" >&2; exit 1; }

if [ ! -d "profiles/$slug" ]; then
    echo "error: no profile '$slug'. Existing:" >&2
    if ! ls profiles 1>&2 2>/dev/null; then echo "  (none)" >&2; fi
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

mail_kb=$(total_kb "data/$slug" "archive/$slug" "backup/$slug")

if [ "$mode" = "--purge" ]; then
    echo "PURGE '$slug': profile and portrait + all mail data:"
    du -sh "data/$slug" "archive/$slug" "backup/$slug" 2>/dev/null || true
    echo "This cannot be undone."
    read -rp "Type the username to confirm: " typed
    [ "${typed:-}" = "$slug" ] || { echo "confirmation did not match — nothing deleted" >&2; exit 1; }
    keep_mail=no
    wipe_login=yes
else
    echo "Removes '$slug's profile and portrait."
    echo "Their mail is currently $(human "$mail_kb") under data/$slug, archive/$slug, backup/$slug."
    read -rp "Proceed with removing the login? [y/N] " ans
    case "${ans:-n}" in [yY]|[yY][eE][sS]) wipe_login=yes ;; *) echo "aborted" >&2; exit 1 ;; esac
    if [ "$mail_kb" = "0" ]; then
        # nothing under data/archive/backup worth keeping — tidy up the folders
        keep_mail=no
    else
        read -rp "Also delete their mail files ($(human "$mail_kb") will be freed)? [y/N] " ans
        case "${ans:-n}" in [yY]|[yY][eE][sS]) keep_mail=no ;; *) keep_mail=yes ;; esac
    fi
fi

if [ "$wipe_login" = yes ]; then
    rm -rf "profiles/$slug"
fi
if [ "$keep_mail" = no ]; then
    rm -rf "data/$slug" "archive/$slug" "backup/$slug"
fi

if [ "$keep_mail" = yes ]; then
    echo "mail kept on disk: data/$slug, archive/$slug, backup/$slug — delete-profile.sh again anytime, or wipe by hand."
fi

remaining=$(find profiles -mindepth 2 -name profile.json 2>/dev/null | wc -l | tr -d ' ')
if [ "$remaining" = "0" ]; then
    echo "warning: '$slug' was the last profile — with no valid roster the backend"
    echo "refuses to start (fail-closed). Create a new one before restarting:"
    echo "  ./scripts/new-profile.sh <user>"
fi

if docker compose ps 2>/dev/null | grep -q backend; then
    docker compose restart backend
    echo "backend restarted — $slug's old session cookies are now dead."
else
    echo "stack not running — removal takes effect on the next 'docker compose up -d'."
fi
if [ "$keep_mail" = no ] && [ "$mail_kb" != "0" ]; then
    echo "done: '$slug' removed; $(human "$mail_kb") freed."
else
    echo "done: '$slug' removed."
fi
