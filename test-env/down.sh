#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
docker compose -p mbxtest -f docker-compose.test.yml down "$@"
