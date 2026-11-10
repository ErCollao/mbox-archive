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

```sh
cd frontend && npm test                       # vitest + @testing-library/svelte
docker run --rm -v "$PWD/backend:/usr/src/app" -w /usr/src/app rust:slim cargo test
```

## Threat model

LAN-only. One shared password per profile, bcrypt-verified over plain HTTP
behind the home router — this is a family appliance, not a vault.
Sessions are HMAC-signed year cookies carrying `slug:expiry:fingerprint`
(`archive/session.key`); delete that file to log everyone out at once, or
change a password to evict just that one person's older sessions.
