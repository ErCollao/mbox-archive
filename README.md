# Mbox Archive

Personal mbox browser (Rust/axum + SvelteKit) serving one folder-scoped archive per person.

## Layout (all gitignored, all bind-mounted into the backend container)

```
profiles/<slug>/profile.json   roster: { "name": "...", "secret": "<sha256 hex of password>" }
profiles/<slug>/portrait.jpg   optional (also .jpeg/.png/.webp) — shown on the login chooser
data/<slug>/                   drop inbox: copy new Takeout .mbox files here
archive/<slug>/merged.mbox     the accumulated archive (created by Merge)
archive/session.key            cookie signing key, auto-generated on first boot
backup/<slug>/                 gzip backups from the maintenance page
```

The app refuses to start with zero valid profiles (fail-closed), and a broken
`profile.json` only skips that one profile — it never takes others down.

## Passwords, names & photos

In-app: the avatar chip (top-left) opens `/profile` — set your display name
there, change the password, or upload a portrait (JPEG/PNG/WebP, ≤12 MB; it
shows up on the sign-in chooser, cropped in the browser to a 512px square).
Changes are written straight to `profiles/<slug>/profile.json` and take effect
immediately, no restart —
the sign-in screen and the archive corner re-read the name from disk, so a
rename never logs anyone out.

Changing the *password* is the revoke switch: session cookies carry a
fingerprint of the secret they were issued against, so every older session
dies the moment the password changes (your current one is re-issued).

Secrets are bcrypt hashes. Legacy sha256 entries are upgraded to bcrypt
automatically on next successful login.

To bootstrap a brand-new profile from the shell, seed it with a sha256 hex:

```sh
printf '%s' 'the password' | shasum -a 256 | cut -d' ' -f1
```

...then the first login turns it into bcrypt. Demo profiles from the initial
migration: passwords are `daniel` and `hajni` — change them via `/profile`.

## Adding / removing a person

```sh
mkdir -p profiles/newname data/newname
$EDITOR profiles/newname/profile.json     # name + secret
docker compose restart backend
```

Removing = delete the folder (archive/data stay on disk unless you also wipe them).
Session cookies for deleted profiles die with them — they're checked against the roster.

## Multiple accounts & aliases (Addresses)

One person can declare several identities — separate Gmail accounts merged
into one archive, or aliases that forward into it — under **your own addresses**
in `/profile` (stored as `"addresses": [...]` in `profile.json`, lowercase,
max 30). With **two or more** declared:

- every email row gets a tag listing which of your addresses it involves
  (`from`, `to`, or `cc`), and the sidebar grows an **Addresses** filter group;
- with 0 or 1, everything stays exactly as before — no tags, no group.

Mechanics worth knowing:

- Tags are **derived at read time** from already-indexed header fields. They
  are never written into the mbox or the index — adding an address relabels
  the whole archive instantly (no rescan), removing one forgets it cleanly.
- Gmail plus-aliases collapse: `hajni+news@gmail.com` counts as
  `hajni@gmail.com` unless the exact plus-address is itself declared
  (most-specific wins).
- Merging separate Takeout exports just works: drop the `.mbox` files in
  `data/<slug>/`, Merge, Re-scan. Mail that two of her accounts exchanged
  exists once per copy with an identical Message-ID — dedup collapses each
  message to a single copy, so mutual threads merge into one conversation
  (tagged with both addresses), not two duplicates.
- The search field also accepts `to:hajni@work.com`-style queries on its own.

## Running

```sh
docker compose up -d
# http://localhost:3132  → /welcome gates everything
```

The maintenance page (`⚙`) is the per-profile runbook: Bring Takeout →
Merge → Re-scan → verify → Backup, with the real folder names filled in.

## Keyboard shortcuts

Vim-style navigation in the archive view (in-app help: press `?` or the
bottom-left corner key). Keys never fire while you're typing in a field —
there, `hjkl` are just letters.

| Key | What |
|---|---|
| `h` `l` (or `←` `→`) | focus previous / next pane — the focused pane lifts with a shadow; the email pane also gets a blue frame |
| `j` `k` (or `↓` `↑`) | move within the focused pane (filters, emails, or thread messages) |
| `Enter` / `Space` | open email · apply highlighted filter · toggle full screen in the email pane |
| `gg` / `G` | jump to first / last item of the focused pane |
| `/` | jump to the search field — `Enter` runs the search and hands the keys back; `Esc` leaves the field |
| `f` | full screen the open email |
| `Esc` | leave an open email; elsewhere hides all highlights (cursors remember their spots) |
| `?` | this help |

## Testing

Automated suites (no containers involved):

```sh
cd frontend && npm test                       # vitest + @testing-library/svelte
docker run --rm -v "$PWD/backend:/usr/src/app" -w /usr/src/app rust:slim cargo test
```

Browser verification happens **only** against the disposable test environment
— never against the live app on :3132, and never with the live profiles:

```sh
./test-env/up.sh                              # fixture-only stack on :3199 (API :8199)
# login: tester / testpass — profile.json + sample.mbox are seeded from fixtures/
./test-env/down.sh
```

`test-env/` runs as its own compose project (`-p mbxtest`) with its own
volumes, ports, and profiles; its runtime dirs are gitignored. The live
`profiles/`, `data/`, `archive/` folders are user data — treat them as
production: read-only, or don't touch.

## Threat model

LAN-only. One shared password per profile, bcrypt-verified over plain HTTP
behind the home router — this is a family appliance, not a vault.
Sessions are HMAC-signed year cookies carrying `slug:expiry:fingerprint`
(`archive/session.key`); delete that file to log everyone out at once, or
change a password to evict just that one person's older sessions.
