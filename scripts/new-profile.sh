#!/usr/bin/env bash
# Create a household member's profile. Pure file operations — meant to run
# INSIDE the backend container, where /profiles, /data, /archive, /backup
# are the bind-mounted folders. See README → People for the one-liner.
# Running it from the repo root on the host works too (relative fallback).
set -euo pipefail

usage() { echo "usage: $(basename "$0") <user> [\"Display name\"]" >&2; exit 2; }

slug="${1:-}"; name="${2:-}"
[ -n "$slug" ] || usage
[[ "$slug" =~ ^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$ ]] \
    || { echo "error: user must be 1-64 chars, letters/digits/_/-, starting alphanumeric" >&2; exit 1; }

if [ -z "$name" ]; then
    name="$(printf '%s' "${slug:0:1}" | tr '[:lower:]' '[:upper:]')${slug:1}"
fi

if [ -d /profiles ]; then
    P=/profiles; D=/data
else
    cd "$(dirname "$0")/.."; P=./profiles; D=./data
fi
[ -f "$P/$slug/profile.json" ] \
    && { echo "error: $slug already has a profile — refusing to overwrite a secret" >&2; exit 1; }

read -rsp "Password for $slug (min 6 chars): " pass1; echo
read -rsp "Repeat it: " pass2; echo
[ "$pass1" = "$pass2" ] || { echo "error: passwords do not match" >&2; exit 1; }
[ "${#pass1}" -ge 6 ] || { echo "error: password needs at least 6 characters" >&2; exit 1; }

if command -v sha256sum >/dev/null 2>&1; then
    hash=$(printf '%s' "$pass1" | sha256sum | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    hash=$(printf '%s' "$pass1" | shasum -a 256 | cut -d' ' -f1)
else
    echo "error: neither sha256sum nor shasum found" >&2; exit 1
fi

esc_name=$(printf '%s' "$name" | sed 's/\\/\\\\/g; s/"/\\"/g')
umask 077
mkdir -p "$P/$slug" "$D/$slug"
printf '{\n  "name": "%s",\n  "secret": "%s"\n}\n' "$esc_name" "$hash" \
    > "$P/$slug/profile.json"

echo "profile created: $name <$slug>"
echo "  $P/$slug/profile.json   (sha256 seed — upgraded to bcrypt on first login)"
echo "  $D/$slug/               (drop Takeout .mbox files here)"
echo "Next: docker compose restart backend (while the stack is up) —"
echo "then $slug can sign in and set name, portrait and addresses under /profile."
