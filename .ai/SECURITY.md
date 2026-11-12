# Security: radio

**Last reviewed:** 2026-11-12, when the install step was built. This model describes the design every step is
held to, and each step that builds a boundary updates its row.

## What this system is

A daemon, one per computer, that local apps share for rooms, the deliberation protocol and, later, the
link to other computers. It stores what its clients post and what other computers send; it reads none
of the user's own files and runs no model. The worst outcome is a stranger getting a say in it: an
app may hand remote operations to its own handler through the Radio, so a hole in the Radio's
admission is a hole in that app's remote control, even though the Radio never reads those messages.

## Trust boundaries

| Boundary | What crosses it | Control (designed) | Proven how |
| --- | --- | --- | --- |
| Local process → socket | one JSON request line | 0600 socket in a 0700 folder; peer uid must equal the daemon's; 16 KiB cap; 5 s for the request and 5 s for the answer; unknown fields refused; `"v"` checked first | `crates/radio/src/server_tests.rs`: another uid refused before reading, cap, request and answer deadlines, unknown fields, `v` first; `contract/v1/` |
| A client app → the daemon's records | `register` (app name, version, members) | stored as data only; nothing a request names is ever run; kinds and names checked and cleaned; at most 256 members | `crates/radio-rooms/src/store_tests.rs` (register), the daemon's membership tests |
| Another computer → the Radio | link messages | an accepted member's signed records, freshness, replay and size checks on every message, before any handler sees it | tests in the link steps (not built yet) |
| The Radio → an app's handler | an app's own message, passed on unread | the handler's path comes only from that app's install record, run with fixed arguments; never from a request or a peer. At install the handler's canonical path is opened without following a link and must be a regular executable file that only its owner (this user or root) can write, in folders owned by this user or root that no one else can write (a group write only for `wheel` or `admin`, whose members can become root anyway; an other write only with the sticky bit); mode bits only, see the gaps | `crates/radio/src/install_tests.rs` (argument refusals, the folder rule); running a handler arrives with the link steps |
| An app's installer → the launch agent and binaries | a new `radio` binary | an exclusive install lock; newer wins; the bytes copied to a new temp file, then renamed (never written over), their SHA-256 recorded and checked on every later install; a start counts only when `hello` answers with the new version; a failed upgrade puts the older copy back, and the rooms store as it was before the newer daemon migrated it (a snapshot taken once the older daemon has stopped); paths derived from versions, never read from a record; the real `launchctl` only for the account's own HOME | `crates/radio/src/install_tests.rs` (`two_installs_at_once_queue`, `an_older_build_never_replaces_a_newer_one`, `a_failed_bootstrap_rolls_back`, the record and binary checks); `crates/radio/tests/install.rs` (the real daemon under a stand-in `launchctl`) |
| An older rooms store → the Radio's store | another program's SQLite file, named by an app's installer | a regular file owned by this user that no one else can write, in folders owned by this user or root that no one else can write (a group write only for `wheel` or `admin`; never an other write, sticky or not), with any `-wal`, `-shm` and `-journal` beside it held to the file's own rule (so none can be a FIFO); first opened without following a link or blocking, then reopened read-only by SQLite by its path, and the path checked again to name the same file; only schema versions 1 and 2, whose three tables are real tables (a view is refused); `trusted_schema` off, a 1 MiB value limit, a 30 s deadline on the source's SQL and a cap of 1,000,000 messages; rows streamed and each checked by the store's own rules, refused rows counted and left out; one transaction; once per file (its real path, or the same file by another name while that name still exists) | `crates/radio-rooms/src/store_tests.rs` (the import tests) |

## Data classes

| Class | Examples | Where stored | Exposure impact | Retention |
| --- | --- | --- | --- | --- |
| Conversations | room messages and statuses, registered members | `~/.local/state/radio/rooms.db`, 0600, in a 0700 folder, opened without following a link | private words of the people in a room | kept until the user deletes them; uninstall keeps them |
| Identity | the computer's link key | `~/.config/radio/` (not built yet) | anyone holding it can speak as this computer | rotated on loss |
| Client records | each app's name, version, handler path | `~/.local/state/radio/clients/<app>.json`, 0600, in a 0700 folder | a forged record could make the daemon run the wrong handler | removed on that app's uninstall |
| Install record | the installed version, its SHA-256, who installed it | `~/.local/state/radio/install.json`, 0600 | a forged record can only name a version whose binary already checks out | removed when the last app uninstalls |
| Operational | the daemon's log (its stderr) | `~/.local/state/radio/radio.log`, 0600 | low, unless it carries the above (it must not: every error is a fixed phrase) | kept on uninstall; no rotation yet |

## Where secrets live

The repo needs none. On a computer, the link key will be the only secret (above). No token, key or
password belongs in the repo; gitleaks runs in `scripts/ci-local.sh`.

## Entry points that take untrusted input

- The socket: every op in `CONTRACT.md`, from any process of the same user (`crates/radio/src/server.rs`).
- The link (later): every message from another computer.
- The deliberation protocol (`crates/radio-panels`): every post, and any saved state a caller hands it, is
  untrusted text; it does no I/O, and its patterns are fixed, never built from a post.
- `radio install`, `uninstall` and `status` arguments, from an app's installer, and the files they read
  back (`install.json`, `clients/`), each checked again on every read.

## External integrations

None today. The link to other computers will be the first, and gets its own row here when it is built.

## Supply chain

- Lockfile committed; every cargo command runs `--locked`.
- No GitHub Actions; checks run locally (`scripts/ci-local.sh`) before a land.
- Each new dependency is pinned and reviewed when it is added.
- `radio-panels` adds `regex =1.13.1` (linear-time matching, fixed patterns) and turns on
  `serde_json`'s `preserve_order` (`indexmap`) and `float_roundtrip` for the workspace build.

## Known gaps and accepted risks

| Gap | Why accepted | Accepted by | Revisit |
| --- | --- | --- | --- |
| A process running as the same user can use the socket | the same rule as any per-user daemon; the user's own processes are trusted at this boundary | founding design, 2026-11-10 | when the link is built |
| Any same-user process can `register` an app's members or post as any registered member | the socket can't tell which app is calling; membership keeps out names no app listed, not a hostile local process | socket step, 2026-11-11 | when install records name each app's handler |
| A write can be applied after its client timed out, so a blind retry posts twice | the client reads back before retrying (`CONTRACT.md`); one connection at a time keeps the store simple | socket review, 2026-11-11 | when connections are served concurrently |
| A raw insert into the store can put a NUL byte inside a kind, past the database's check | only the daemon writes the store, and its code refuses NUL before any insert | socket review, 2026-11-11 | if anything else ever writes the store |
| The daemon serves one connection at a time, so a client holds others for up to 5 s while sending its request and 5 s while reading its answer | the same-user boundary above; both deadlines cover the whole request and the whole answer | socket step, 2026-11-11 | if a client ever needs more |
| Any same-user process can run `radio install` with any handler it can write, or point `RADIO_LAUNCHCTL` at another program | the same-user boundary above; the handler check keeps out files other users can change | install step, 2026-11-12 | when the daemon starts running handlers |
| The handler is checked at install, not yet when it runs; an `admin` member can replace an app in `/Applications` | the daemon runs no handler yet; `admin` members can become root, so a folder only they can write gives them nothing new | install review, 2026-11-12 | when the daemon starts running handlers: check again, or check the app's signature, before each run |
| Uninstalling an app keeps the members it registered in `rooms.db` | data stays on uninstall (D6); the members are names, and the app is no longer a client | install step, 2026-11-12 | when registration is tied to install records |
| The owner and folder checks read mode bits, not access control lists | only a file's owner or root can add an entry that lets others write, the same as a loose mode the owner sets | install delta review, 2026-11-12 | if a check must hold against the owner's own settings |
| A store imported under one name can be imported again under a hard link once the first name is gone | the key is the real path; the earlier name's inode is compared only while it still opens, since an inode number alone can match a later file | install delta review, 2026-11-12 | if an app ever imports one store under more than one name |
| If restoring the store fails during a rollback, the version before it meets the newer store and does not start | the restore is a rename inside one folder; the install says the version before it did not start again, and a newer install repairs it | install delta review, 2026-11-12 | if a failed restore is ever seen |
| Two slow clients ahead of an install's check can get a busy daemon restarted | each client holds the daemon for at most 10 s, and every write is a transaction, so a restart loses no committed write | install delta review, 2026-11-12 | when connections are served concurrently |
| An earlier import's path on a stalled network volume can hold the install while it checks that path | the open does not block on the file, but a stalled volume can stall the path lookup | install delta review, 2026-11-12 | if a store is ever imported from a network volume |
| A newer version's folder left with no record (a newer install killed midway) makes every older install refuse | an older copy can't tell a half-installed newer Radio from a working one whose record was lost, and replacing it could leave the Radio down | install delta review, 2026-11-12 | if an older app is ever stuck: the newer app's install repairs it |
| `radio.log` is not rotated | the daemon writes only fixed phrases, a few per start | install step, 2026-11-12 | if the log ever grows |
| `radio-panels` renders member names into its markdown result unfenced, so a name holding a newline could forge a section | the daemon does not call the crate yet, and the rooms store already turns newlines in names into spaces | protocol move, 2026-11-12 | when the daemon renders a result: refuse control characters in names, or fence them |
| A dissenting member whose name has a character outside the fence label's set (an apostrophe, an accent) stops the result from rendering | a refusal, not an injection: the label check holds; the daemon does not call the crate yet | protocol move, 2026-11-12 | when the daemon renders a result: give each member a safe label |
| The term check's list lives outside the repo, so a contributor without it runs no term check | the list names private terms and cannot be published | founding design, 2026-11-10 | never, by design |
| Commit and tag identities (author, committer, tagger) are not term-checked | they are the publishing account's own GitHub noreply identity, which GitHub shows on every push anyway | fix-up review, 2026-11-11 | if a second account publishes here |
| Text stored as UTF-16 (a NUL between characters) is not matched | only deliberate evasion produces it in this repo; gitleaks still reads it | fix-up review, 2026-11-11 | if a UTF-16 file is ever added |

## Who to tell

Report a security problem privately through GitHub's private vulnerability reporting on this repo
(its Security tab, "Report a vulnerability"), never in a public issue.

## Change log

- 2026-11-11: created from the founding design.
- 2026-11-11: the term check reads every object in the history once; identities and UTF-16 noted as gaps.
- 2026-11-11: the term check runs under the C locale, ignores replace refs, refuses grafts and
missing objects, and reads ref names.
- 2026-11-11: the socket and the rooms store are built; their rows name their tests.
- 2026-11-12: the install step is built: the installer, client and install records, the import of an
  older store.
- 2026-11-12: the import checks the source's folders and side files; an older copy never replaces a newer
  install; mode-bit checks, hard-link re-imports, a failed restore, two slow clients, a stalled volume
  and a half-installed newer version noted as gaps.
- 2026-11-12: the deliberation protocol is in (`radio-panels`): an entry point, its dependencies and two
  rendering gaps.
