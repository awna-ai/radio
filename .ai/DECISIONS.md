# Decisions: radio

The decisions this repo was founded on (2026-11-10). Newer decisions go above them.

## 2026-11-12: D9. Install records are checked, never trusted, and an older store comes in by name

- **Paths come from versions.** `install.json` names the installed version, its SHA-256 and its
  path, but the path is only compared with the one derived from the version; a record that does not
  check out, or whose binary is missing or changed, counts as nothing installed, so the next install
  repairs it instead of failing on every app start.
- **An older rooms store moves in only when an app names it** (`--rooms <path>`), so no app's path
  lives in the code (D3). It is read, never written; only stores at schema version 1 or 2 are taken,
  so a store that grew its own later migrations is never misread as this one's. It comes in once per
  path (the `imports` table, written in the same transaction as the rows); a room that already exists
  here keeps its title and gets the older store's messages after its own. Rows this store's rules
  refuse are counted and left out.
- **The import is the one write outside the daemon,** and it is safe: it runs under the install
  lock, in one transaction, and SQLite's lock orders it with the daemon's own writes; the daemon keeps
  no rows in memory.
- **A failed upgrade puts the store back too.** A newer daemon migrates `rooms.db` before it answers,
  so the installer snapshots the store once the older daemon has stopped and restores it if the newer
  one fails; a release that adds a store migration bumps the version.
- **App names are lowercase,** so two spellings never share one client record on a case-insensitive
  disk. A failed install records no client; the app runs the install again.
- **The source of an import** must belong to this user and be writable by no one else, and so must
  its folders and any `-wal` and `-shm` beside it, since SQLite reads those too and reopens the file
  by its path. SQLite may create `-wal` and `-shm` when it opens a WAL file; nothing writes the store
  itself.
  Its version-1 status lines read as set by the person at the computer, as this store's own second
  migration defaulted them.
- **A test can never load a real launch agent:** the real `launchctl` is used only for the account's
  own HOME; any other HOME must name a stand-in in `RADIO_LAUNCHCTL`.

## 2026-11-11: D8. Member kinds are data

An author is a kind and a name. The daemon knows one kind, `human`, the person at the computer, who is
always a member; every other kind is a word an app registers with its members, stored and compared but
never interpreted. So no app's vocabulary lives in the code (D3), and a rooms store written under
another app's fixed words keeps its rows when it moves here: the store's third migration rebuilds its
tables with the generic kind check.

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
