# Decisions: radio

The decisions this repo was founded on (2026-11-10). Newer decisions go above them.

## 2026-11-12: D16. Every op is also a command, and the socket stays the contract

Each op of protocol minor 0 is a `radio` subcommand (`hello`, `register`, `room create`, `rooms`,
`post`, `read`, and `line list|set|clear` for status lines, since `status` is the install status), so
a shell script or an app in any language can use Radio by running the binary it bundles. A command
sends exactly its op's request through `radio-client` and prints the answer line as it arrived. It
adds no op, field or answer phrase, so it can't drift from `CONTRACT.md`: what it checks itself is
only what it must to build a request (argument shape, a member's kind by the daemon's own rule) and a
body too big for any request; everything else is the daemon's to answer. Exit codes split ok (0),
refused (3), failed (1) and a bad argument (2). There is no waiting or streaming command: a caller
reads with `--since` again.

## 2026-11-12: D15. The first link is direct only, and delivers files into a folder the app gets from Radio

The link between computers is built direct first: each computer binds only local addresses the user
configured, on one fixed UDP port, and dials only addresses the user configured or a join code
carried. There is no relay, no address lookup and no port mapping, so it works where the computers
already reach each other, on one network or over a VPN the user runs. Relays and finding computers
on a network by themselves come later, unchanged in design, as their own steps.

Its first use is files: an app hands its Radio a file for an app on another of the person's
computers, and the receiving Radio writes it into a folder that Radio itself made for that app,
never anywhere else. A send that can't reach its peer fails within the dial's 5 seconds; Radio holds nothing to send
again, so the sending app keeps what it needs. Handing an app's message to its handler comes after.

The fleet record, members, codes and deliveries live in `fleet.db`, the daemon its one writer, so
every change is one transaction; `fleet.toml` holds only this computer's own settings. The threat
model's § *The link* holds the rules and the numbers each step tests.

## 2026-11-12: D14. Releases are built on a maintainer's Mac, signed ad hoc, and ship inside apps

A tag's release carries one `radio` binary per Mac architecture (arm64 and x86_64), each in a
`.tar.gz` with the licence, plus a `SHA256SUMS` file. `scripts/release.sh <tag>` builds them from a
clean copy of the tag, never the working tree, with the build's folder names remapped, and refuses a
binary that still holds a folder or account name of the Mac that built it, or a private term (D3);
`--publish` uploads them with `gh` and downloads them again to compare. The repo runs no Actions, so a
maintainer runs it.

The binaries are signed ad hoc and not notarised. Radio is meant to ship inside an app, and
notarisation needs every executable in it signed with a Developer ID, the hardened runtime and a
secure timestamp, so an app re-signs its copy with its own identity anyway, and the app's ticket then
covers it. A copy downloaded on its own in a browser is quarantined, and macOS should then refuse
it, since an ad hoc binary can't be notarised; notarising the bare binary would need a Developer ID
this repo doesn't have. `.ai/PROJECT.md` § Signing has the details and how each was checked.

## 2026-11-12: D13. One README, plain first (replaces D11's second document)

The app makers' guide moved into `README.md`, and `docs/` is gone. The README reads in this order:
what Radio does, where it helps, the gap it fills, how it's used, building an app in plain steps,
then *For app developers* (bundling, installing, the socket with a worked client, errors,
versions), where it stands, and working on Radio itself. Everything above *For app developers*
stays in plain words with no code, so the plain reader still has a plain page; the gap comes before
the how, so a reader learns why Radio exists before how to use it. D11's other rules stand: the
exact rules stay in `CONTRACT.md`, and a contract change updates `CONTRACT.md`, `radio-client` and
the README's developer section together.

## 2026-11-12: D12. The name is Radio, and what talks through it are agents

The project is **Radio**, with no article, in prose as in headings. What uses it are **agents**:
the umbrella word for whatever an app calls its AI (an assistant, a chatbot, a copilot, a bot, an AI
session someone works in, a program that does work on its own), and the word the open protocols in
this space use. The README lists those names the first time it uses the word, so a reader who knows
only one of them finds it. Code strings and crate descriptions said "the Radio" at first, among them the
client's `the Radio did not answer`, which apps may match on; they changed in code after `v0.1.0`,
together with the README's developer section, which names the old words.

## 2026-11-12: D11. Two audiences, two documents

`README.md` says what Radio is, where it helps, how it's used and the gap it fills, in plain
words, for someone who will never read code; it ends with a pointer for people working on the
Radio. What an app maker needs (bundling, installing, the socket, a worked client, the errors) is
`docs/app-developers.md`, and the exact rules stay in `CONTRACT.md`. One document per audience keeps
each one short, and keeps the plain one plain. A change to the contract updates `CONTRACT.md`,
`radio-client` and the guide together. The README's comparison with other open tools is dated and
says "when we looked": re-check it before changing it.

## 2026-11-12: D10. The deliberation protocol comes in as a copy, its author field renamed

The protocol crate, `radio-panels`, was first written in another public repo. It came in as one new
commit (from that repo's `d2c2871a309734e6a6b26f1b57e9a98c41e31f86`), not with its history, because that
history and its test data used words D3 keeps out of this repo. The one such word in the protocol
itself, the field that names a dissent's author, became `member` in the type, the serialized result
and the fixtures (a few fixture sentences that used the same word too); the rendered markdown never
named the field and is unchanged, and nothing else in the protocol changed. `tests/rename.rs` holds the proof: each
fixture's digest, taken from the earlier copy with that word masked, matches this copy with `member`
masked at the recorded places, so the other bytes are the earlier copy's; the manifest is the root of
trust, so a reviewer checks it against the earlier copy once. The reference probes' own note names a
commit of the earlier reference, which still emits the old field name. A client that kept results
under the old field name maps them itself. The first tag, `v0.1.0`, carries the crate; apps pin it
exactly (D4).

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
  its folders and any `-wal`, `-shm` and `-journal` beside it, since SQLite opens those too and reopens the file
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

## 2026-11-10: D7. Radio owns each computer's one link and identity

One identity per computer, held by Radio, and the link to other computers is built here from now
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

The shared core is **Radio**: the per-computer process that carries the link between computers,
plus rooms, the deliberation protocol and seats.
