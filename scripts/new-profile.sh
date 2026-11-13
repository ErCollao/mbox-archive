#!/usr/bin/env bash
# Create a household member's profile without hand-editing JSON.
#
#   ./scripts/new-profile.sh hajni "Hajni"
#
# Prompts for the password twice, seeds profiles/<user>/profile.json with a
# sha256 secret (the first successful login upgrades it to bcrypt), creates
# the folders, and restarts the backend if the stack is already running.
# Slug and password rules mirror the backend (handlers/account.rs).
set -euo pipefail

cd "$(dirname "$0")/.."

usage() { echo "usage: $(basename "$0") <user> [\"Display name\"]" >&2; exit 2; }

slug="${1:-}"; name="${2:-}"
[ -n "$slug" ] || usage
[[ "$slug" =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$ ]] \
    || { echo "error: user must be 1-64 chars, letters/digits/_/-, starting alphanumeric" >&2; exit 1; }

if [ -z "$name" ]; then
    name="$(printf '%s' "${slug:0:1}" | tr '[:lower:]' '[:upper:]')${slug:1}"
fi

if [ -f "profiles/$slug/profile.json" ]; then
    echo "error: profiles/$slug/profile.json already exists — refusing to overwrite a secret" >&2
    exit 1
fi

read -rsp "Password for $slug (min 6 chars): " pass1; echo
read -rsp "Repeat it: " pass2; echo
[ "$pass1" = "$pass2" ] || { echo "error: passwords do not match" >&2; exit 1; }
[ "${#pass1}" -ge 6 ] || { echo "error: password needs at least 6 characters" >&2; exit 1; }

if command -v sha256sum >/dev/null 2>&1; then
    hash=$(printf '%s' "$pass1" | sha256sum | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    hash=$(printf '%s' "$pass1" | shasum -a 256 | cut -d' ' -f1)
else
    echo "error: neither sha256sum nor shasum found on this host" >&2; exit 1
fi

esc_name=$(printf '%s' "$name" | sed 's/\\/\\\\/g; s/"/\\"/g')

umask 077
mkdir -p "profiles/$slug" "data/$slug"
printf '{\n  "name": "%s",\n  "secret": "%s"\n}\n' "$esc_name" "$hash" \
    > "profiles/$slug/profile.json"

echo "profile created: $name <$slug>"
echo "  profiles/$slug/profile.json   (sha256 seed — upgraded to bcrypt on first login)"
echo "  data/$slug/                   (drop Takeout .mbox files here)"

if docker compose ps 2>/dev/null | grep -q backend; then
    docker compose restart backend
    echo "backend restarted — $slug can now sign in."
else
    echo "stack not running — it will pick the profile up on the next 'docker compose up -d'."
fi
echo "Next: have $slug sign in, then change the password anytime via /profile."
