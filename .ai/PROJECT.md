# radio: one daemon per computer for rooms and messages

Radio is a small daemon, one per computer, that several apps share. It keeps rooms (shared
conversations between the person at the computer and the agents their apps run), carries the
deliberation protocol, and holds the computer's one link to other computers (partly built: README
*Where it stands*). Apps talk to it over a local socket and never read its files. The repo is
public, so it names no app and holds no one's data (`.ai/DECISIONS.md` D3).

## Who reads what

| You are | Read |
| --- | --- |
| Anyone, new to it | `README.md` up to *Building an app that uses Radio*: what it is and the gap it fills, in plain words |
| Making an app that uses it | `README.md` → *For app developers*, then `CONTRACT.md` |
| Working on Radio | this file, then `.ai/DECISIONS.md`, `.ai/SECURITY.md` and `CONTRACT.md` |

## What's where

| Path | What | Test |
| --- | --- | --- |
| `CONTRACT.md` | The socket contract: framing, versions, members, the ops of each major | `crates/radio-client/tests/`, `contract/v1/` |
| `contract/v1/` | One fixture per op of major 1 but the four file ops (`CONTRACT.md`), replayed against a real daemon and through the client's types | `crates/radio/src/contract_tests.rs`, `crates/radio-client/tests/fixtures.rs` |
| `crates/radio-client/` | The client side of the contract: its numbers, its types, one request function | `crates/radio-client/tests/` |
| `crates/radio-rooms/` | The rooms store (SQLite, the daemon its one writer): rooms, messages, status lines, registered members, the one-time import of an older store | `crates/radio-rooms/src/store_tests.rs` |
| `crates/radio-panels/` | The deliberation protocol: posts and their blocks, rounds, distances, the result and its rendering; no I/O. The daemon serves it as the deliberation ops (`crates/radio/src/deliberation.rs`) | `crates/radio-panels/tests/` (the 66 fixtures, the reference probes, `rename.rs`) |
| `crates/radio-link/` | The link to other computers: this computer's key (`node.key`), the one endpoint builder, admission at the handshake and on each stream, framing, and the messages two computers exchange (`Hello`, an app's message); the fleet record (signed, with its recovery key and the rule for a newer one), join codes and `fleet.db`, which holds them; `radio serve` starts the endpoint once `fleet.db` exists, and logs `link off` when it can't. `src/sim.rs` is the membership simulator: in-process and deterministic, n members that sleep, wake, join, are removed and leave, with each member's messages and bytes and how fast a change spreads; the membership layer sits behind its `Node` trait. `src/topic.rs` is the fleet's gossip topic: what it carries (a membership record, or a computer's signed announcement of its record's version and hash, never the record itself) and the check each message passes before it is applied; a fetched record is applied only when it matches the newest announcement `fleet.db` holds; nothing sends on the topic yet | `crates/radio-link/src/*_tests.rs`, `crates/radio-link/tests/` (the endpoint, the transport's pins, one builder, the wire on a loopback pair, a hostile peer, the simulator with a stand-in layer). The 171,000-member run is outside the gate: `cargo test -p radio-link --release --locked --test sim -- --ignored --nocapture`; so are the gossip runs: `cargo test -p radio-link --release --locked --test gossip_sim -- --ignored --nocapture` |
| `crates/radio/` | The daemon: `radio serve` binds the socket and serves the ops; `radio install`, `uninstall` and `status` (`src/install.rs`); one command per op (`src/cli.rs`, D16) | `crates/radio/src/` tests, `crates/radio/tests/install.rs` (a stand-in `launchctl` that runs the real daemon), `crates/radio/tests/cli.rs` (each command against its fixture, and the README's Python example, on a real daemon) |
| `scripts/ci-local.sh` | Every check, run before anything lands on `main` | itself |
| `scripts/term-check.sh` | Refuses a file, path or commit message that names a private term | `tests/term_check_test.sh` |
| `scripts/release.sh` | Builds a tag's release (a binary per Mac architecture, a checksum file) and, with `--publish`, uploads it (D14) | `tests/release_test.sh` (stand-ins for the toolchain and `gh`) |
| `.ai/SECURITY.md` | The threat model | read before touching a trust boundary |

## Paths on a computer

| What | Where |
| --- | --- |
| Socket | `~/.local/state/radio/radio.sock` (folder 0700) |
| State | `~/.local/state/radio/` (0700): `rooms.db`, `install.json`, `install.lock`, `clients/<app>.json`, `radio.log`, and `fleet.db` from the first fleet op and `status.json` once the link starts, all 0600 |
| Config | `~/.config/radio/` (0700): `node.key`, 0600, made by the first `id` or fleet op; `recovery.key`, 0600, written once by `fleet_init` for the person to move into a password manager and delete |
| Binaries | `~/.local/libexec/radio/<version>/radio` |
| Launch agent | `~/Library/LaunchAgents/ai.awna.radio.plist` |

## Installing

`radio install --app <name> --app-version <version> --handler <path> [--rooms <path>]` is the one
install logic every app calls with the `radio` binary it bundles (D6, D9):

1. It takes `install.lock` (an exclusive lock; a second install waits up to 120 s).
2. Nothing installed (or a record or binary that no longer checks out): it copies itself to
   `~/.local/libexec/radio/<version>/radio`, writes the launch agent, starts it and waits up to 10 s
   for `hello` to answer with its version. If it never does, nothing is left installed.
3. The same version or a newer one installed: it installs nothing, and restarts the daemon once if
   it does not answer.
4. An older one installed: it copies itself beside it, restarts the agent on the new copy, and on
   any failure puts the older copy and the rooms store as they were. One previous version is kept;
   older ones are deleted. A copy older than a newer install never replaces it, even when that
   install's record or binary is missing or damaged, and never starts on a rooms store newer than
   itself.
5. It records the app (a lowercase name) in `clients/<app>.json` with its handler, the only program
   the daemon will ever run for that app.
6. `--rooms` names an older rooms store to bring in, once per path.

Exit codes and what each case prints are in the README, *For app developers* → *Install it*. `radio uninstall --app
<name>` removes a client; the last one out stops the daemon and removes the agent, the binaries,
`install.json`, `install.lock`, and the socket and its lock (only when no daemon still holds that lock,
and the socket only when it is one),
and keeps the rooms, the log and all other data. An uninstall of an app that was never a client
writes nothing. `radio status` prints the installed version, the clients and whether the socket answers.

Tests never touch the real launch agent: `launchctl` is `/bin/launchctl` only when HOME is the
account's own home directory; any other HOME needs `RADIO_LAUNCHCTL` naming a stand-in.

## Signing

The installer copies the binary byte for byte into a new file that is then renamed into place,
never written over (macOS caches a binary's signature per file, so code changed in place can be
killed), and carries no extended attribute over. Each point below says how it was checked
(2026-11-12, macOS 27.2).

- **Ad hoc is enough for a copy that is not quarantined.** The linker signs arm64 builds ad hoc;
  `scripts/release.sh` signs both architectures ad hoc as `ai.awna.radio`. `spctl --assess --type
  execute` rejects an ad hoc build, yet the same build with no quarantine attribute runs from a
  shell (both run here).
- **What gets quarantined.** A browser download does, and `tar` carries the archive's quarantine
  onto what it unpacks; `curl` sets none (both run here). A release archive fetched with `curl`, or a
  build from source, runs. A quarantined ad hoc binary should be refused: Gatekeeper looks for a
  notarisation ticket when quarantined code first runs, and notarisation refuses ad hoc signatures
  (Apple, "Notarizing macOS software before distribution"). That is a reading of the docs, not run
  here, since it opens a dialog.
- **Inside a notarised app.** Notarisation needs every executable signed with a Developer ID, the
  hardened runtime and a secure timestamp (the same page), so an app re-signs its bundled `radio`
  with its own identity before submitting. The app's ticket then covers it: Apple's advice is to
  notarise and staple only the outermost container, whose ticket covers everything in it (Apple
  developer forums, thread 689337). The installer's copy is byte-identical, so its code hash is the
  one the ticket names. Not run: no Developer ID build exists yet, and the daemon's needs under the
  hardened runtime (it links only system libraries and uses no JIT) are unchecked.
- **A bare binary can't carry a ticket.** "Although tickets are created for standalone binaries,
  it's not currently possible to staple tickets to them", and Gatekeeper finds the ticket online
  (Apple, "Customizing the notarization workflow"). A quarantined command-line tool double-clicked
  in the Finder is refused however well it is signed or notarised, because the Finder hands it to
  Terminal as a document (thread 689337, and thread 706379, "Tool Blocked by Gatekeeper").
  Notarising the release binaries would need a Developer ID; until then they are for bundling, not
  for running on their own.

## Releasing

`bash scripts/release.sh <vX.Y.Z>` builds a tag's release on this Mac, with no Actions (D14):

1. The tag's commit must have passed the full check on `main`, and releases must not be frozen.
   `bash scripts/release-check.sh <commit>` checks this; `release.sh` runs it itself, with or without
   `--publish`, before the term list, tools or build.
2. It checks the tag (it must exist and name the workspace version), the term list, the tools, and
   both Rust targets (`aarch64-apple-darwin` and `x86_64-apple-darwin`; it prints the
   `rustup target add` line for a missing one and installs nothing).
3. It builds `radio` for each target from a clean copy of the tag in a temporary folder, so neither
   the working tree nor HEAD matter, with `--remap-path-prefix` over the home folder, the Cargo and
   rustup folders and the build folder, then strips the debug map with Apple's `strip -S`. The remap
   can't reach the debug map, which the linker writes with the full paths of the precompiled std
   libraries, and rustc's own strip goes through `rust-objcopy`, which can fail with only a warning
   (it did on the maintainers' Mac, leaving four such paths).
4. It refuses a binary of the wrong architecture, or one that still holds a build path, this
   account's name or a private term, then signs each ad hoc. Terms of five or more letters are
   matched anywhere, since Rust packs string literals with nothing between them; shorter ones only
   as whole words, because they turn up inside the library's symbol names.
5. It packs `radio-<tag>-macos-<arch>.tar.gz` (the binary and `LICENSE`, owned by `root:wheel`, no
   macOS metadata) and `SHA256SUMS` into `target/release-dist/<tag>/`, replacing an earlier build of
   that tag only once the build has passed.

With `--publish` it first checks, before building, that `origin` is a GitHub repository, that
GitHub's copy of the tag names the same commit, and that the tag has no release yet (a draft
counts); every `gh` call names that repository. After the build it uploads the three files from its
private temporary folder as a draft, downloads them again and compares them byte for byte, checks
the draft lists exactly those three, asks GitHub for the tag's commit once more, and only then
publishes the draft. A draft that fails any of these stays a draft, and the script prints the
`gh release delete` line that removes it.

### The full check on main

Lands gate on the touched tier. `bash scripts/verify-main.sh` runs the full strict gate on the
newest `main` in a scratch worktree, checking every land since the last green at once. A pass moves
`refs/green/main` on `origin`. A red runs once more on the same commit; red twice sets
`refs/green/frozen` and names the commit, or range, to revert or fix. Releases stay frozen until a
check of a `main` that contains the frozen commit passes. It is meant to run on a schedule on a
maintainer's Mac; what calls it lives outside this repo.

## Running the checks

`bash scripts/ci-local.sh` runs everything: shellcheck, the term check's own test, the term check,
gitleaks, `cargo fmt`, `cargo clippy` and `cargo test`, for the workspace and for `fuzz/`. It needs
stable Rust (edition 2024) and a C++ compiler (the fuzz crate's `libfuzzer-sys` builds libFuzzer);
shellcheck and gitleaks are skipped when missing. `RADIO_STRICT=1` makes
every tool and the term list required, and is how the maintainers run it before a land.

`bash scripts/ci-local.sh --tier touched` checks only what a change can break: the crates it touches
and every workspace crate that depends on them (`cargo clippy` and `cargo test` with `-p`), the fuzz
crate when `radio-link` is among them (and its `one_builder` test, which reads every crate, when it is not), and the test of each shell script it touches. Files a crate's
tests read count as that crate (`contract/` and `CONTRACT.md` as `radio-client`, `README.md` as
`radio`, `fuzz/` as `radio-link`); other Markdown, `LICENSE` and `.gitignore` add nothing, and any
other path, a base that doesn't resolve, or a dependents query that fails or finds nothing runs the full tier instead. The change is everything since
the base, `RADIO_CI_BASE` (default `origin/main`), untracked files included. Shellcheck, the term
check, gitleaks and both format checks run in every tier. `--list` prints a tier's steps and runs
none; `tests/ci_local_test.sh` checks the choices. The full tier also runs
`tests/verify_main_test.sh`, which checks the full check on `main` and the release refusals.

The link's property tests (`crates/radio-link/tests/properties.rs`, D21) run 256 cases each from a
fixed seed. `RADIO_PROPERTY_CASES=100000 cargo test --locked -p radio-link --test properties` runs
more, and `RADIO_PROPERTY_SEED=<n>` replays the seed a failure names.

The fuzz targets in `fuzz/` (`frame`, `records`, `join_code`) are never fuzzed by the gate. Fuzzing
needs a nightly toolchain and `cargo-fuzz`; from the repo root:

```sh
cargo +nightly fuzz run frame -- -dict=fuzz/link.dict -max_total_time=600
```

A crash is saved under `fuzz/artifacts/<target>/`; `cargo +nightly fuzz run <target> <that file>`
replays it.
