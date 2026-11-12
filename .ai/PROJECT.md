# radio: one daemon per computer for rooms and messages

The Radio is a small daemon, one per computer, that several apps share. It keeps rooms (shared
conversations between people and the assistants their apps run), carries the deliberation protocol,
and will hold the computer's one link to other computers. Apps talk to it over a local socket
(`CONTRACT.md`); none of them reads its files. The repo is public, so it names no app and holds no
one's data (`.ai/DECISIONS.md` D3).

| Path | What | Test |
| --- | --- | --- |
| `CONTRACT.md` | The socket contract: framing, versions, members, the ops of each major | `crates/radio-client/tests/`, `contract/v1/` |
| `contract/v1/` | One fixture per op of major 1, replayed against a real daemon and through the client's types | `crates/radio/src/contract_tests.rs`, `crates/radio-client/tests/fixtures.rs` |
| `crates/radio-client/` | The client side of the contract: its numbers, its types, one request function | `crates/radio-client/tests/` |
| `crates/radio-rooms/` | The rooms store (SQLite, the daemon its one writer): rooms, messages, status lines, registered members | `crates/radio-rooms/src/store_tests.rs` |
| `crates/radio/` | The daemon: `radio serve` binds the socket and serves the ops | `crates/radio/src/` tests |
| `scripts/ci-local.sh` | Every check, run before anything lands on `main` | itself |
| `scripts/term-check.sh` | Refuses a file, path or commit message that names a private term | `tests/term_check_test.sh` |
| `.ai/SECURITY.md` | The threat model | read before touching a trust boundary |

## Layout to come

| Crate | What |
| --- | --- |
| `radio-panels` | The deliberation protocol (no I/O) |
| `radio-link` | The link to other computers: endpoint, records, admission |
| `radio` | gains `install`, `uninstall` and `status` |

## Paths on a computer

| What | Where |
| --- | --- |
| Socket | `~/.local/state/radio/radio.sock` (folder 0700) |
| State | `~/.local/state/radio/` (`rooms.db`, 0600) |
| Config | `~/.config/radio/` |
| Binaries | `~/.local/libexec/radio/<version>/radio` |
| Launch agent | `~/Library/LaunchAgents/ai.awna.radio.plist` |

## Running the checks

`bash scripts/ci-local.sh` runs everything: shellcheck, the term check's own test, the term check,
gitleaks, `cargo fmt`, `cargo clippy` and `cargo test`. It needs stable Rust (edition 2024); shellcheck
and gitleaks are skipped when missing. `RADIO_STRICT=1` makes every tool and the term list required,
and is how the maintainers run it before a land.
