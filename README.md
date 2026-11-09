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

## Passwords

`secret` is the lowercase hex sha256 of the password, nothing more:

```sh
printf '%s' 'the password' | shasum -a 256 | cut -d' ' -f1
```

Edit `profiles/<slug>/profile.json` and restart the backend to apply.

Demo profiles from the initial migration: passwords are `daniel` and `hajni` — change them.

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

## Threat model

LAN-only. One shared password per profile, verified against a saltless sha256 over
plain HTTP behind the home router — this is a family appliance, not a vault.
Sessions are HMAC-signed year cookies (`archive/session.key`); delete that file to
log everyone out at once.
