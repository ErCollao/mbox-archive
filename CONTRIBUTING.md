# 🛠️ Contributing

Developer and operator internals for [Mbox Archive](README.md). The README
is the user manual; this file is the workshop tour. Everything here assumes
the repo folder is your working directory.

## Repo layout

```
backend/          Rust/axum — single binary, all state on bind-mounted disk
  src/main.rs         startup: roster → tenants → routes (build_app, split out for tests)
  src/auth.rs         bcrypt/legacy secrets, HMAC session cookies, session.key, portrait sniff
  src/state.rs        Root / Tenant / seed_tenant / BusyGuard, index volume paths
  src/model.rs        wire DTOs (Serialize/Deserialize)
  src/identity.rs     address folding, row tags, to:/from: term peeling
  src/index.rs        mboxshell wrapper: parsing, binary index, body search
  src/html.rs         strip_scripts (fixed-point loop), render sandbox helpers
  src/handlers/       account.rs (auth + profile settings), archive.rs (scan/merge/search/status/backup + prune_old_backups)
  src/api_tests.rs    42 integration tests against the full router in-process
frontend/         SvelteKit + adapter-node
  src/routes/+page.svelte            the archive app (panes, facets, keynav, iframes)
  src/routes/welcome/+page.svelte    chooser + password stage (?p=<slug> in URL)
  src/routes/maintenance/            per-profile runbook (⚙)
  src/routes/profile/                settings (name, password, portrait, addresses)
  src/routes/profiles/[slug]/portrait/  pre-login portrait route
  src/routes/api/                    same-origin proxy to the backend (rejects dot-segments)
scripts/          new-profile.sh / delete-profile.sh — pure file ops, run inside the container
test-env/         disposable stack (own compose project, ports, fixtures) — see Testing
.github/img/      README screenshots, numbered in reading order
```

## Runtime data layout

All gitignored, all bind-mounted into the backend container (overridable
via `PROFILES_DIR`/`DATA_DIR`/`ARCHIVE_DIR`/`BACKUP_DIR`):

```
profiles/<slug>/profile.json   roster: { "name", "secret", "addresses"? } — bcrypt after first login
profiles/<slug>/portrait.jpg   optional (also .jpeg/.png/.webp)
data/<slug>/                   drop inbox: Takeout .mbox files land here
archive/<slug>/merged.mbox     the accumulated archive (Merge output)
archive/session.key            cookie signing key, auto-created on first boot
backup/<slug>/                 verified gzip snapshots (retention: BACKUP_KEEP)
```

The backend **refuses to start with zero valid profiles** (fail-closed — an
archive that boots anonymous is an archive with no doors, and a silent-empty
start looks like data loss). One broken `profile.json` only skips that
profile; it never takes others down.

## How the pieces fit

- **Auth.** Login verifies against a fresh disk read of the roster (password
  changes apply without restart). Sessions are HMAC-signed, year-long cookies
  carrying `slug:expiry:secret-fingerprint`; deleting `archive/session.key`
  logs everyone out, changing a password evicts that person's older sessions
  (the current device gets a re-issued cookie). Legacy sha256 secrets upgrade
  to bcrypt transparently on first successful login — which is why
  `new-profile.sh` seeds sha256 (host shell has no bcrypt; the container
  script can't either — the server does the upgrade).
- **Tenants.** The in-memory map is built at boot from the roster. A profile
  added or removed on disk only takes effect after `docker compose restart
  backend` — the scripts say so in their final line.
- **Merge.** Streams every `data/<slug>/*.mbox` (sorted by mtime) plus the
  existing `archive/merged.mbox` into one output, deduping by Message-ID —
  newest copy wins — writing to a temp file and renaming atomically.
  Merge never deletes: the drop folder is untouched (tidy it whenever; the
  runbook reminds you). Log line: `Merge complete: kept N messages,
  skipped M older duplicates`.
- **Scan/serve.** Without `merged.mbox` the boot scan indexes only the
  newest single mbox (it warns loudly). Re-scan rebuilds the in-memory index
  (~500 bytes/message; 300k messages ≈ seconds).
- **Backup.** `POST /api/backup`: gzip to temp with progress → full
  decompression verify → atomic rename → `prune_old_backups` keeps the newest
  `BACKUP_KEEP` `mbox-archive-*.mbox.gz` (default 10, `0` disables). The
  pattern + mtime sort means unrelated files in `backup/` are never touched
  (there is an integration test for exactly that).
- **XSS posture.** Email HTML is served with a `sandbox` CSP and rendered in
  a scriptless iframe; `strip_scripts` loops to a fixed point (split-marker
  reassembly bypass). Attachment previews downgrade markup to downloads.

## Testing

No containers involved for the unit/integration suites:

```sh
cd frontend && npm test                                   # vitest + @testing-library/svelte
docker run --rm -v "$PWD/backend:/usr/src/app" -v mbx-target:/usr/src/app/target \
  -w /usr/src/app rust:1.99-slim cargo test               # 42 tests, full router in-process
docker run --rm -v "$PWD/backend:/usr/src/app" -v mbx-target:/usr/src/app/target \
  -w /usr/src/app rust:1.99-slim cargo clippy --all-targets -- -D warnings
```

CI (`.github/workflows/ci.yml`) runs clippy + cargo test on `rust:1.99-slim`
and vitest + build on node 26, pinned to `ubuntu-26.04` runners. Keep all
three green; clippy is enforced with `-D warnings`.

**Never verify against the live stack.** `test-env/up.sh` starts the whole
app as its own compose project (`-p mbxtest`) on :3199 (direct API :8199)
with a fixture-only tenant (`tester` / `testpass`) and its own volumes;
`down.sh -v` drops everything including the index volume. Live `profiles/`,
`data/`, `archive/` are user data: read-only, ideally untouched.

The fixtures include a deterministic 400-message fictional corpus
(`fixtures/make-demo-mbox.py` → `demo.mbox`, seeded RNG, reserved `.test`
addresses, Gmail-realistic categories so the default landing view is
populated). Regenerate it if you touch the generator, and keep any body
line from starting with `From ` (that would parse as a message separator —
there is a count check: envelopes must equal subjects).

## Screenshots

`.github/img/`, numbered in README reading order (`01-hero.gif` …). Capture
against the test env. Videos: record QuickTime, convert palette-style —

```sh
ffmpeg -i in.mov -vf "fps=12,scale=1280:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse" out.gif
```

The `.mov` sources stay local (gitignored).

## Conventions

- Dependencies and toolchain pinned (rust 1.99, node 26, debian 13) in
  Dockerfiles and CI alike; change pins deliberately, not by `latest`.
- New API endpoints are a security event: extend the threat model section in
  the README and add api_tests coverage in the same PR.
- Commit messages are single-line, dense, honest about what changed and what
  was verified.
