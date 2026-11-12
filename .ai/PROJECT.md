# radio: one daemon per computer for rooms and messages

The Radio is a small daemon, one per computer, that several apps share. It keeps rooms (shared
conversations between the person at the computer and the assistants their apps run), carries the
deliberation protocol, and will hold the computer's one link to other computers. Apps talk to it
over a local socket and never read its files. The repo is public, so it names no app and holds no
one's data (`.ai/DECISIONS.md` D3).

## Who reads what

| You are | Read |
| --- | --- |
| Anyone, new to it | `README.md`: what it is and the gap it fills, in plain words |
| Making an app that uses it | `docs/app-developers.md`, then `CONTRACT.md` |
| Working on the Radio | this file, then `.ai/DECISIONS.md`, `.ai/SECURITY.md` and `CONTRACT.md` |

## What's where

| Path | What | Test |
| --- | --- | --- |
| `docs/app-developers.md` | How an app bundles, installs and talks to the Radio, with a worked client | read against `CONTRACT.md` when either changes |
| `CONTRACT.md` | The socket contract: framing, versions, members, the ops of each major | `crates/radio-client/tests/`, `contract/v1/` |
| `contract/v1/` | One fixture per op of major 1, replayed against a real daemon and through the client's types | `crates/radio/src/contract_tests.rs`, `crates/radio-client/tests/fixtures.rs` |
| `crates/radio-client/` | The client side of the contract: its numbers, its types, one request function | `crates/radio-client/tests/` |
| `crates/radio-rooms/` | The rooms store (SQLite, the daemon its one writer): rooms, messages, status lines, registered members, the one-time import of an older store | `crates/radio-rooms/src/store_tests.rs` |
| `crates/radio-panels/` | The deliberation protocol: posts and their blocks, rounds, distances, the result and its rendering; no I/O, not used by the daemon yet | `crates/radio-panels/tests/` (the 66 fixtures, the reference probes, `rename.rs`) |
| `crates/radio/` | The daemon: `radio serve` binds the socket and serves the ops; `radio install`, `uninstall` and `status` (`src/install.rs`) | `crates/radio/src/` tests, `crates/radio/tests/install.rs` (a stand-in `launchctl` that runs the real daemon) |
| `scripts/ci-local.sh` | Every check, run before anything lands on `main` | itself |
| `scripts/term-check.sh` | Refuses a file, path or commit message that names a private term | `tests/term_check_test.sh` |
| `.ai/SECURITY.md` | The threat model | read before touching a trust boundary |

## Layout to come

| Crate | What |
| --- | --- |
| `radio-link` | The link to other computers: endpoint, records, admission |

## Paths on a computer

| What | Where |
| --- | --- |
| Socket | `~/.local/state/radio/radio.sock` (folder 0700) |
| State | `~/.local/state/radio/` (0700): `rooms.db`, `install.json`, `install.lock`, `clients/<app>.json`, `radio.log`, all 0600 |
| Config | `~/.config/radio/` |
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

Exit codes and what each case prints are in `docs/app-developers.md` §2. `radio uninstall --app
<name>` removes a client; the last one out stops the daemon and removes the agent, the binaries and
`install.json`, and keeps the rooms and all other data. `radio status` prints the installed version, the clients and whether the socket answers.

Tests never touch the real launch agent: `launchctl` is `/bin/launchctl` only when HOME is the
account's own home directory; any other HOME needs `RADIO_LAUNCHCTL` naming a stand-in.

**Signing.** The binary is copied byte for byte into a new file that is then renamed into place,
never written over (macOS caches a binary's signature per file, so code changed in place can be
killed), and no extended attribute is carried over. Today's builds are signed ad hoc by the linker,
which is enough for a copy that is not quarantined. A Developer ID build has to include this binary
in the app's notarised content: a bare executable cannot carry a stapled ticket, so its byte-identical
copy depends on the app's ticket, looked up by its code hash. That path is untested until a Developer
ID build exists.

## Running the checks

`bash scripts/ci-local.sh` runs everything: shellcheck, the term check's own test, the term check,
gitleaks, `cargo fmt`, `cargo clippy` and `cargo test`. It needs stable Rust (edition 2024); shellcheck
and gitleaks are skipped when missing. `RADIO_STRICT=1` makes every tool and the term list required,
and is how the maintainers run it before a land.
