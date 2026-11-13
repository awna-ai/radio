# Decisions: radio

The decisions this repo was founded on (2026-11-10). Newer decisions go above them.

## 2026-11-13: D22. A computer's own link addresses live in `fleet.db`, on one fixed port

Each computer keeps the addresses its other computers reach it at: at most one IPv4 and one IPv6,
set by the person (`radio link address add`), never found by Radio. The port is fixed, UDP 7341
(unassigned in macOS's `/etc/services`), so an address is only an IP and every computer listens
in the same place. They are kept in `fleet.db`, beside this computer's other own state (the
`local` row), not in `fleet.toml` as D15 first said: the daemon is that file's one writer, a change
is one transaction, and a second settings file would add a format and a dependency for two
values. Leaving a fleet keeps them. A join code carries exactly these addresses; a code made
before any is set carries none, and the new computer's dial then fails at once with a plain
sentence instead of waiting.

## 2026-11-13: D21. The link's decoders are tried with generated inputs on every land, and fuzzed outside it

The frame decoder, the three record checks and the join-code parser take bytes from other
computers or from what a person pastes, so the gate also tries them with inputs no one chose:
random and mutated frames in random chunkings, records with one field changed or one rule broken,
mutated record JSON and mutated codes (`crates/radio-link/tests/properties.rs`). Each property runs
256 cases from a fixed seed, so the gate stays repeatable and a failure names the seed and case
that replay it; `RADIO_PROPERTY_SEED` and `RADIO_PROPERTY_CASES` run other or more cases. The
generator is a few lines of the test itself, not a property-testing library, so the crates take no
new dependency for it.

The same code is fuzzed by `cargo-fuzz` targets in `fuzz/`, a workspace of its own with its own
lockfile, never built into a release. Fuzzing needs a nightly toolchain and time, so it runs
outside the gate; the gate checks that the targets build, lint and pass their own tests on stable,
and that every package both lockfiles hold is at the same version, source and checksum, with no
package in the fuzz lockfile that has not passed an intake.

## 2026-11-13: D20. Each computer signs its own record, and apps' sections ride in it unread

Each computer in a fleet keeps one record about itself: its name, its link id, its roles (at most 8
names Radio gives no meaning), the addresses it can be reached at, and one section per app (at most
8, each a string of up to 4 KiB). Only that computer's key signs it, so no other computer can change
what it says about itself. Radio checks the signature and carries an app's section without reading
it: the app on the other computer parses it. Sections are not secret, since every computer in the
fleet holds them.

A computer keeps another's record only when that computer signed it, it names a computer on this
computer's list (pending and blocked ones included) under the name the list has for it, and its
version is newer than the one held; anything else is dropped, so a replay or an older record changes
nothing. A version is the signing time in milliseconds, or one more than the last when the clock
says less, so a computer that loses its database and joins again starts above its old records
unless its clock has gone back past them. Any
accepted computer may carry another's record, since the signature, not the sender, says whose it
is. A record with addresses replaces the ones held for that computer; one without says nothing
about them. Removing a computer drops its record, re-admitting it under a new name drops the old
one, and leaving a fleet drops them all.

An app changes its section with the `resign` op, which may also replace the computer's roles: an
empty section removes it, and a request without one leaves it, so changing roles never drops a
section. The app must be registered, and a change that changes nothing keeps the record and its
version. `computer_records` reads this computer's record and its accepted computers', each with the
asking app's section; a pending or blocked computer's record is kept but shown only once it is
accepted, so an app never acts on a computer the person hasn't let in. A newcomer takes the
records its inviter holds with the rest of the inviter's view, up to 256 KiB of them; the others
reach it when their computers next sign. Until the daemon listens (the direct-address step), a
re-signed record is kept on its own computer and sent from then on.

## 2026-11-13: D19. Computers join through one inviter, and each keeps its own list

A fleet's other computers are *computers* in Radio's code, ops and docs, not *members*, since a
member is already a room's author (D8). A newcomer dials the inviter its code names, on the join
ALPN, at the code's addresses, and sends the code, its name, its own addresses and its signed
consent to that inviter, name and addresses; the inviter checks them, consumes the code and adds the
newcomer in one transaction, answers with its own view (the fleet record, the computers it has
accepted, the keys removed), and sends every accepted computer a signed introduction carrying that
consent, so no other computer can claim to have invited it. The newcomer takes that view whole:
joining through a computer is joining what that computer knows.

Each computer keeps its own list. An introduction from a computer this one trusts adds the newcomer
as accepted; from any other it waits as pending until the person accepts it here. A block is this
computer's alone and is never sent; accepting the computer lifts it. A removal, signed by the
removed computer's inviter or the founding member (while it has not left or been removed), reaches
the removed computer too, which then leaves the fleet; a removal and a leave, the latter signed by
the leaving computer, are tombstones that win over an introduction of the same key whichever arrives
first: a removal that arrives before its introduction is held (at most 64 per signer and 1,024 in
all) until the introduction shows its signer was the inviter. Introductions and removals carry their
signer's counter, so a repeated one is dropped, a record that would change nothing is not kept, and
a computer makes, and accepts from each other computer, at most 3 introductions in 24 hours. Nothing
lifts a tombstone: a removed computer comes back only with a new key. A record a computer missed
while it was off is not sent again.

All of this is built in `radio-link` and proven between computers on loopback. The daemon starts no
endpoint until the direct-address step, so its socket gains only the ops that need none (list,
accept, block, trust); joining, removing and leaving become ops in that step, when Radio first
listens.

## 2026-11-13: D18. A fleet starts on one computer, and its recovery key leaves through a file

`fleet_init` makes the fleet record on the computer that runs it, its founding member: a random
id, a name, a version, the founder's and a recovery key's public halves, and no relays yet (D15),
signed by the founder's link key. A newer record is accepted only from the founder, which may
change the name and the release key, or from the recovery key, which may name a new founding
member or re-pin the release key, and nothing else; the recovery key itself never changes. Records
are ordered by an epoch, then a version: only the recovery key starts a new epoch, so a stolen
founder key can't sign a version so high that the recovery key could never follow it. The
recovery key's secret is written once to `~/.config/radio/recovery.key` (0600, left out of Time
Machine) rather than put in the op's answer, so it doesn't land in a terminal's or an agent's
transcript; the person moves it into a password manager and deletes the file.

Join codes are Radio's own format, `rj1.` and four dot-separated parts (the fleet, the inviter, a
256-bit secret and the addresses to dial), so no app's name is in them. Radio stores only each
secret's SHA-256 in `fleet.db`, compares it in constant time, and consumes a code in one
transaction, so two newcomers racing one code can't both use it.

## 2026-11-13: D17. The log says what happened, never what was said

`radio.log` (the daemon's stderr under launchd) gets one line per lifecycle event (start with the
version, protocol and the computer's name; bind; a failed start; stop on SIGTERM or SIGINT, after the
request in hand but never more than 2 seconds, so the installer's stop never times out on a stalled
client; an install, an upgrade, an install that kept the running version, a client removed, a failed
install or uninstall) and one per answer that is not ok, as its op and fixed phrase. A line is `<UTC time> <source> <event>` and `key=value` fields, each
text value escaped onto one line. It never holds a body, a title, a status line, a member's name or
any other text a client sent, and an op it doesn't know is `unknown`, so what apps say stays out of
it and a client can't forge extra lines; the computer's name is the one personal detail. Successful
requests write nothing (unless the answer couldn't be delivered), so a busy Radio's log stays short;
the log starts over past 1 MiB rather than rotating, because launchd holds its file open, and only
when the daemon's stderr is `radio.log` itself. The install creates the log (0600) before it starts
the daemon; the installer's own lines append and never create it, so a refusal before anything is
written leaves no log, and a first install that fails after that keeps it, with the daemon's lines
saying why. The lines are for people: they are not part of the contract and may change in any release.

## 2026-11-12: D16. Every op is also a command, and the socket stays the contract

Each op is a `radio` subcommand (minor 0's `hello`, `register`, `room create`, `rooms`, `post`, `read`,
and `line list|set|clear` for status lines, since `status` is the install status; then `id`, and each
later op with the minor that adds it), so
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
