#!/usr/bin/env bash
# Disposable test environment: its own compose project, dirs, ports, and
# fixture-only profiles. Live profiles (daniel, hajni) are NEVER used for
# verification — anything needing writes happens here, on `tester`.
#
#   ./up.sh          build + start + seed            -> http://localhost:3199
#   ./down.sh        stop (data stays under test-env for reuse)
#   ./down.sh -v     stop and drop the index volume
set -euo pipefail
cd "$(dirname "$0")"

mkdir -p profiles/tester data/tester archive backup

# newest copy of the fixture archive content
if [ -f fixtures/sample.mbox ]; then
    cp fixtures/sample.mbox data/tester/sample.mbox
fi

# optional bulk demo content for screenshots (regenerate with
# python3 fixtures/make-demo-mbox.py — 400 fictional messages, all .test)
if [ -f fixtures/demo.mbox ]; then
    cp fixtures/demo.mbox data/tester/demo.mbox
fi

# seeded with the legacy sha256 form on purpose: first login upgrades it,
# which exercises that path too. Addresses are pre-declared so identity
# tagging, filtering and row chips are active from the very first scan.
if [ ! -f profiles/tester/profile.json ]; then
    hash=$(printf '%s' 'testpass' | shasum -a 256 | cut -d' ' -f1)
    printf '{\n  "name": "Tester",\n  "secret": "%s",\n  "addresses": ["tester-primary@sample.test","tester-alias@sample.test"]\n}\n' "$hash" > profiles/tester/profile.json
fi

docker compose -p mbxtest -f docker-compose.test.yml up -d --build

echo
echo "test env ready:  http://localhost:3199   login: tester / testpass"
echo "direct API:     http://localhost:8199"
echo "wipe everything: rm -rf test-env/{profiles,data,archive,backup} && ./down.sh"
