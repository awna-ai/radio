# Security: radio

**Last reviewed:** 2026-11-13, when the link's rows were written, before any of it is built. This model describes
the design every step is held to, and each step that builds a boundary updates its row.

## What this system is

A daemon, one per computer, that local apps share for rooms, the deliberation protocol and, later, the
link to other computers. It stores what its clients post and what other computers send; it reads none
of the user's own files, except a file an app hands it to send to another computer, and runs no model.
The worst outcome is a stranger getting a say in it: an
app may hand remote operations to its own handler through Radio, so a hole in Radio's
admission is a hole in that app's remote control, even though Radio never reads those messages.
The second worst is a file meant for one of the user's computers reaching anyone else, or landing
anywhere on the receiving computer but the folder its app chose.

## The link (designed, not built)

The first link is direct only: two computers that can already reach each other (on one network, or
over a VPN the user runs) dial each other at addresses the user configured. There is no relay, no
address lookup and no port mapping yet, so nothing of the link leaves the computer except packets
to those addresses. Each row below whose proof says **not built** is the design the step that builds
it is held to; that step replaces the proof with its tests. The steps, in order: the endpoint and the
computer's key; the wire protocol; the fleet record and join codes; joining, accepting, leaving,
removing and blocking; direct addresses; app delivery.

- **Identity is the key, and only the key.** A computer is its Ed25519 key; its endpoint id is the
  public half. The transport's TLS 1.3 handshake proves both ends hold their keys. A name, a fleet
  id, a version or an address a peer sends is a claim, checked against Radio's own records or
  ignored; it never identifies anyone.
- **Admission is Radio's, at the handshake.** The transport lets any key complete a handshake with
  a listening endpoint. Radio refuses, at the hook that runs once the handshake completes and before
  any stream is read, every key that is not an accepted member of its fleet, and every removed or
  blocked one; the only exception is a newcomer dialing with a join code (its own row), which may
  open one stream for the join and nothing else.
- **Nothing is replayable.** The transport's packets are bound to the connection's keys, and 0-RTT
  (whose first flight can be replayed) is off on both sides. Signed records that can be forwarded
  over a fresh connection (introductions, removals, member records) carry a version or a counter
  from their signer, and an old or repeated one is dropped.
- **Raw files rest only where an app asked.** A delivered file is written only into the folder the
  receiving app registered; Radio keeps no copy on either side, and a send that can't reach its peer
  fails at once rather than waiting in Radio.

## Trust boundaries

| Boundary | What crosses it | Control (designed) | Proven how |
| --- | --- | --- | --- |
| Local process → socket | one JSON request line | 0600 socket in a 0700 folder; peer uid must equal the daemon's; 16 KiB cap; 5 s for the request and 5 s for the answer; unknown fields refused; `"v"` checked first | `crates/radio/src/server_tests.rs`: another uid refused before reading, cap, request and answer deadlines, unknown fields, `v` first; `contract/v1/` |
| A client app → the daemon's records | `register` (app name, version, members) | stored as data only; nothing a request names is ever run; kinds and names checked and cleaned; at most 256 members | `crates/radio-rooms/src/store_tests.rs` (register), the daemon's membership tests |
| Radio's endpoint → the network | what the link sends out unasked | the one endpoint builder starts from an empty configuration plus a crypto provider: relays off, no address lookup (no DNS, pkarr or mDNS), port mapping (UPnP, NAT-PMP, PCP) off, no proxy, no TLS key log; bound to one fixed UDP port; it dials only addresses the user configured or a join code carried; no other endpoint is built anywhere in the code | **not built.** The endpoint step: a test that the built endpoint reports no relay, no lookup service and no port mapping, and binds the configured port. The direct-address step: a capture of two linked computers showing packets only to each other |
| Another computer → Radio's endpoint | a QUIC connection from any key on any network that reaches the port | ALPN `radio/1` only; TLS 1.3 only; 0-RTT refused; once the handshake completes, the peer's authenticated key must be an accepted member (or a newcomer's join, below), and a removed or blocked key is refused there, before any stream is read; caps on connections at once, per key and in all, and on streams per connection; idle connections closed | **not built.** The wire step: an unpaired key is refused before its first stream is read; a removed and a blocked key likewise; another ALPN fails the handshake; a 0-RTT attempt gets no early data |
| A dial → the computer it names | the answer from an address the user configured | the TLS check pins the dialed endpoint id, so an address answering with another key fails the handshake; the address is only where to try, never who is there | **not built.** The wire step (a dial to an address held by a third key fails); the direct-address step (the same, through `radio link address`) |
| An accepted member → Radio's records | `Hello`, control frames, signed records (the fleet record, member records, introductions, removals) | a u32 length prefix, then JSON with unknown fields refused, capped at 64 KiB for control and 1 MiB for the largest record, the length checked before the frame is read; one request per stream; a `Hello`'s fleet id must be this fleet's, and its other fields are only shown, never trusted; each record verified with its signer's key, and applied only from the signer allowed to make it (the founding member for the fleet record, a member for its own record, an inviter or the founding member for a removal); versions only rise; introductions carry their inviter's counter and are capped at 3 per inviter in 24 hours; a removal is a tombstone that wins over an introduction of the same key in either order; every change one transaction in `fleet.db` | **not built.** The wire step (an over-cap length is refused unread, unknown fields, a wrong fleet id); the fleet-record step (a record signed by another key, an older version); the join step (an untrusted introduction stays pending, a replayed one is dropped, a removal of someone else's invitee is refused unless by the founding member, a removal before its introduction still wins, a blocked member's dial is refused) |
| A join code → the inviter | a one-time code the person carries to the new computer | the code names the inviter's endpoint id, so the newcomer's dial pins the inviter's key and no one else can answer it; the secret is 256 random bits and the inviter keeps only its SHA-256, compared in constant time; single use, consumed in the same transaction that admits the newcomer; 30 minutes by default and 24 hours at most; revocable; the code is read from standard input or the socket, never from a command line, and never written to a log or a file by Radio; the newcomer's name must match `^[a-z][a-z0-9-]{0,31}$`; a known key re-joins as itself, and a known name with a new key is refused until the old member is removed | **not built.** The fleet-record step (a used, expired or revoked code is refused; a second use of one code, raced, admits one newcomer); the join step (the name rule; the collision rule; a dial that answers with a key other than the code's inviter fails) |
| An app → the `send` op | a file path and a peer, from a same-user process | the peer must be an accepted member; the path is opened without following a link and must be a regular file of at most 64 MiB, measured on the open file; it is streamed from that open file, never reopened by name; when the peer can't be reached the op fails at once, and Radio keeps nothing to send again | **not built.** The delivery step: a link, a folder, a FIFO, an over-cap file and an unpaired peer are each refused; an unreachable peer fails within the dial's deadline |
| An accepted member → an app's delivery folder | a file another computer sent, its name, the receiving app's name | the app must be an installed client that registered a delivery folder (refused otherwise, so the file never rests anywhere); the folder must be owned by this user, mode 0700, and is opened without following a link and used only through that open folder; the name must be 1 to 128 characters of `A-Z a-z 0-9 . _ -`, not starting with a dot; the bytes go to a new hidden file in that folder (`O_CREAT \| O_EXCL \| O_NOFOLLOW`, 0600), and only a complete file of the announced size is renamed to its name, never over an existing file; a cut stream, an oversize stream or a full disk removes the hidden file; a cap on the bytes waiting in one folder; the sender recorded is the connection's authenticated key and Radio's name for it, never a name in the message; Radio never reads the file's contents | **not built.** The delivery step: an unregistered app, a name with a slash or a leading dot, a folder swapped for a link, a file of that name already there, a cut stream (nothing left under either name), an oversize stream, a folder over its cap |
| A relay → members | nothing yet | no relay exists in this phase; relays get their own rows with the steps that build them | none needed: the endpoint step proves relays are off |
| Radio → an app's handler | an app's own message, passed on unread | the handler's path comes only from that app's install record, run with fixed arguments; never from a request or a peer. At install the handler's canonical path is opened without following a link and must be a regular executable file that only its owner (this user or root) can write, in folders owned by this user or root that no one else can write (a group write only for `wheel` or `admin`, whose members can become root anyway; an other write only with the sticky bit); mode bits only, see the gaps | `crates/radio/src/install_tests.rs` (argument refusals, the folder rule); running a handler arrives with the link steps |
| An app's installer → the launch agent and binaries | a new `radio` binary | an exclusive install lock; newer wins; the bytes copied to a new temp file, then renamed (never written over), their SHA-256 recorded and checked on every later install; a start counts only when `hello` answers with the new version; a failed upgrade puts the older copy back, and the rooms store as it was before the newer daemon migrated it (a snapshot taken once the older daemon has stopped); paths derived from versions, never read from a record; the real `launchctl` only for the account's own HOME | `crates/radio/src/install_tests.rs` (`two_installs_at_once_queue`, `an_older_build_never_replaces_a_newer_one`, `a_failed_bootstrap_rolls_back`, the record and binary checks); `crates/radio/tests/install.rs` (the real daemon under a stand-in `launchctl`) |
| An older rooms store → Radio's store | another program's SQLite file, named by an app's installer | a regular file owned by this user that no one else can write, in folders owned by this user or root that no one else can write (a group write only for `wheel` or `admin`; never an other write, sticky or not), with any `-wal`, `-shm` and `-journal` beside it held to the file's own rule (so none can be a FIFO); first opened without following a link or blocking, then reopened read-only by SQLite by its path, and the path checked again to name the same file; only schema versions 1 and 2, whose three tables are real tables (a view is refused); `trusted_schema` off, a 1 MiB value limit, a 30 s deadline on the source's SQL and a cap of 1,000,000 messages; rows streamed and each checked by the store's own rules, refused rows counted and left out; one transaction; once per file (its real path, or the same file by another name while that name still exists) | `crates/radio-rooms/src/store_tests.rs` (the import tests) |
| A maintainer's Mac → a published release | the `radio` binaries and their checksums, public | built by `scripts/release.sh` from a clean copy of the tag, `--locked`; build paths remapped and the debug map stripped, and a binary that still holds a build path, the account's name or a private term is refused; the git, gh and Cargo environment variables that could redirect them cleared (see the gaps); signed ad hoc; archives owned by `root:wheel` with no macOS metadata; `SHA256SUMS` checked before upload, from the script's private temporary folder; every `gh` call names `origin`'s GitHub repository, and GitHub's tag must name the built commit before the build and again before publishing; uploaded as a draft, read back byte for byte with exactly the three files listed, then published | `tests/release_test.sh` (stand-ins for the toolchain and `gh`) |

## Data classes

| Class | Examples | Where stored | Exposure impact | Retention |
| --- | --- | --- | --- | --- |
| Conversations | room messages and statuses, registered members | `~/.local/state/radio/rooms.db`, 0600, in a 0700 folder, opened without following a link | private words of the people in a room | kept until the user deletes them; uninstall keeps them |
| Identity | the computer's link key (Ed25519) | `~/.config/radio/node.key`, 0600, in a 0700 folder, created with `O_EXCL` and opened without following a link; a key file that is a link, not this user's, or readable by anyone else is refused, never repaired (not built yet) | anyone holding it can speak as this computer to every member | kept until the computer leaves; on loss or leak the computer is removed and joins again with a new key |
| Recovery key | the secret half of the fleet's recovery key | written once, 0600, for the person to move into their password manager, then deleted from the computer; only its public half stays, in the fleet record (not built yet) | anyone holding it can name a new founding member | for the life of the fleet |
| Fleet records | the fleet record, each member's latest signed record and state, join codes' SHA-256 and expiry, tombstones, each member's last working addresses | `~/.config/radio/fleet.toml` (this computer's name and the fleet record) and `~/.local/state/radio/fleet.db` (SQLite, WAL, 0600, the daemon its one writer) (not built yet) | who the user's computers are and where they can be reached; a code's hash can't be turned back into the code | kept until the computer leaves the fleet; expired codes are deleted |
| Deliveries | files other computers sent to an app | only in the folder that app registered; Radio keeps no copy, on either side (not built yet) | whatever the app sent: the app's own data class | the receiving app's to keep or delete |
| Client records | each app's name, version, handler path | `~/.local/state/radio/clients/<app>.json`, 0600, in a 0700 folder | a forged record could make the daemon run the wrong handler | removed on that app's uninstall |
| Install record | the installed version, its SHA-256, who installed it | `~/.local/state/radio/install.json`, 0600 | a forged record can only name a version whose binary already checks out | removed when the last app uninstalls |
| Operational | the daemon's log (its stderr) | `~/.local/state/radio/radio.log`, 0600 | low, unless it carries the above (it must not: every error is a fixed phrase) | kept on uninstall; no rotation yet |

## Where secrets live

The repo needs none. On a computer, the link key is the only secret Radio keeps (above), and it
never leaves through the socket: the `id` op answers the public half. A join code's secret lives only
in the person's hands until it is used, and the inviter keeps only its hash. The recovery key's
secret half is on the computer only until the person has moved it. Radio's own releases are signed ad
hoc (D14), so Radio holds no release key; an app that signs its own updates keeps that key itself. No
token, key or password belongs in the repo; gitleaks runs in `scripts/ci-local.sh`.

## Entry points that take untrusted input

- The socket: every op in `CONTRACT.md`, from any process of the same user (`crates/radio/src/server.rs`).
- The link (designed, not built): every connection and frame from another computer, every join code
  a person pastes, and every file a peer delivers (§ *The link*).
- The deliberation protocol (`crates/radio-panels`): every post, and any saved state a caller hands it, is
  untrusted text; it does no I/O, and its patterns are fixed, never built from a post.
- `radio install`, `uninstall` and `status` arguments, from an app's installer, and the files they read
  back (`install.json`, `clients/`), each checked again on every read.

## External integrations

None today, and none hosted by anyone: the first link dials only the user's own computers at
addresses the user configured, and uses no service of the transport library's maker (no relay, no
address lookup). A relay, when it comes, gets its own rows here.

## Supply chain

- Lockfile committed; every cargo command runs `--locked`.
- No GitHub Actions; checks run locally (`scripts/ci-local.sh`) before a land.
- Each new dependency is pinned and reviewed when it is added.
- Release binaries are built on a maintainer's Mac by `scripts/release.sh` (D14), so they are only as
  sound as that Mac's toolchain; `SHA256SUMS` proves a download matches the upload, not the build.
- The link will add `iroh =1.3.0` with its own crates held at 1.3.0 in the lockfile (`iroh-base`,
  `iroh-dns`, `iroh-relay`, `noq`, `noq-proto`, `noq-udp`; `=1.3.0` on `iroh` alone resolves them to
  later versions) and `tokio`; the endpoint step runs the dependency intake. `iroh` is built with its default features off
  and only `tls-ring` and `fast-apple-datapath` on, so its `portmapper` feature (and the `portmapper`
  crate) is never compiled in.
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
| A newer version's folder left with no record (a newer install killed midway) makes every older install refuse | an older copy can't tell a half-installed newer Radio from a working one whose record was lost, and replacing it could leave Radio down | install delta review, 2026-11-12 | if an older app is ever stuck: the newer app's install repairs it |
| `radio.log` is not rotated | the daemon writes only fixed phrases, a few per start | install step, 2026-11-12 | if the log ever grows |
| `radio-panels` renders member names into its markdown result unfenced, so a name holding a newline could forge a section | the daemon does not call the crate yet, and the rooms store already turns newlines in names into spaces | protocol move, 2026-11-12 | when the daemon renders a result: refuse control characters in names, or fence them |
| A dissenting member whose name has a character outside the fence label's set (an apostrophe, an accent) stops the result from rendering | a refusal, not an injection: the label check holds; the daemon does not call the crate yet | protocol move, 2026-11-12 | when the daemon renders a result: give each member a safe label |
| Release binaries are signed ad hoc, not notarised, so a copy downloaded in a browser is quarantined, and macOS should refuse it (not run: it opens a dialog) | Radio ships inside apps, which re-sign it with their own Developer ID for their notarisation; notarising it alone needs a Developer ID the repo doesn't have | release step, 2026-11-12 | when the repo has a Developer ID |
| The release scan finds a build path, account name or term only as plain bytes, not compressed or split across a binary's sections; terms under five letters only as whole words | the binary holds no compressed strings of its own, and the paths come from rustc's panic locations and the debug map, which the remap and the strip cover; a short term matched anywhere turns up inside the library's own symbol names | release step, 2026-11-12 | if a release ever embeds compressed data |
| Cargo's config files, a `RUSTUP_TOOLCHAIN` override and gh's own config can still change the release build or the account gh uses, and a moment remains between the last tag check and publishing | they are the maintainer's own settings on their own Mac; the scans still refuse a binary holding a path, the account's name or a term, and the tag is checked again just before the draft is published | release delta review, 2026-11-12 | if a release is ever built on a shared or automated machine |
| The link key is a file that any process of the same user can read | the same-user boundary above, as for any per-user key such as an SSH key; the file's own checks keep out other users and links | link design, 2026-11-13 | if the daemon can hold it in the login keychain with an access list that names only Radio |
| A newcomer believes the fleet record and member list its inviter hands it | joining through a member is joining that member's view; the record is signed by the founding member, so an inviter can withhold members but not forge the record | link design, 2026-11-13 | when member records are gossiped between members |
| Anyone who reaches the port can make Radio do a handshake's work before admission refuses the key | the cost is one TLS handshake, capped by the connection limits; with direct addresses only, the port is reachable only from the user's own network or VPN | link design, 2026-11-13 | when a relay or a public address makes the port reachable from anywhere |
| An unused join code admits whoever redeems it first | it is single use, short-lived and revocable, it reaches only the inviter's own addresses, and every member sees the newcomer's name; the person carries it themselves | link design, 2026-11-13 | if codes are ever sent through a channel the person doesn't control |
| Any same-user process can register an app's delivery folder, or send any file the user can read to a member | the same-user boundary above: such a process can already read those files and write that folder; the peer must still be an accepted member | link design, 2026-11-13 | when apps are told apart on the socket |
| The network between two computers sees which of them talk, when and how much, though not what | the transport encrypts every byte; hiding the traffic's shape is out of scope | link design, 2026-11-13 | never, by design |
| A delivered file's name and bytes are the sending computer's choice, within the name rule and the cap | Radio never reads the file; the receiving app treats it as untrusted input from that member | link design, 2026-11-13 | never, by design: the app checks what it takes |
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
- 2026-11-12: releases are built by `scripts/release.sh`: a boundary row, the supply-chain line and three gaps.
- 2026-11-13: the link's design (direct dial only, joining, app delivery): § *The link*, eight boundary rows
  marked not built, four data classes, the keys, the transport dependency and seven gaps.
