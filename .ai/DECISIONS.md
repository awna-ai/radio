# Decisions: radio

The decisions this repo was founded on (2026-11-10). Newer decisions go above them.

## 2026-11-10: D7. The Radio owns each computer's one link and identity

One identity per computer, held by the Radio, and the link to other computers is built here from now
on, so nothing is built twice.

## 2026-11-10: D6. The daemon installs itself

`radio install --app <name> --app-version <v> --handler <path>` holds the one install logic every app
calls: an exclusive install lock, newer wins, a failed upgrade rolls back to the version before it,
one record per client app, and uninstalling the last app keeps the user's data. Removing any one app
leaves the others working.

## 2026-11-10: D5. Every read goes through the socket

The rooms store is the daemon's private file. Two apps at different versions can't safely share a
file whose reader refuses any schema but its own.

## 2026-11-10: D4. Socket versioning

A `"v"` on every request, a `hello` that names what the daemon serves, additive minors, the current and
previous majors served side by side, newer-wins install, and semver tags that apps pin exactly. One
rule every client can test against (`CONTRACT.md`).

## 2026-11-10: D3. The repo names no app and holds no one's data

Apps are clients registered by name, in data. Code, docs, tests and commit messages name no app and no
person, and tests use invented people and apps. `scripts/term-check.sh` enforces it with a list kept
outside the repo.

## 2026-11-10: D2. Public, MIT, under awna-ai

The repo is `awna-ai/radio`, public and MIT, and the launch agent's label is `ai.awna.radio`. A shared
component belongs to the vendor's namespace, and a public repo lets public apps build against it.

## 2026-11-10: D1. The name

The shared core is **the Radio**: the per-computer process that carries the link between computers,
plus rooms, the deliberation protocol and seats.
