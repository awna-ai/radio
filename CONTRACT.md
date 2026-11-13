# Radio's socket contract

Protocol major: 1
Protocol minor: 5

This is the one contract between Radio and the apps that use it: what an app may send, what it
gets back, and what may change between versions. It is written to be exact; the README's *For app
developers* section shows how to use it. `radio-client` carries the two numbers above as constants, and a test checks
them against these lines. They are independent of the repo's semver tags.

## Where and who

- The socket is `~/.local/state/radio/radio.sock`, mode 0600, in a folder of mode 0700.
- One daemon listens, holding an exclusive lock on `radio.sock.lock` beside it.
- A connecting process must run as the same user as the daemon (its peer uid is checked on every
  connection). Anyone else is refused before a byte is read.

## Framing

- One request per connection: one line of JSON in, one line of JSON out, then the daemon closes.
- A request is at most 16 KiB. A request not fully sent within 5 seconds of connecting is answered
  `the request took too long`.
- An answer is at most 8 MiB, which holds the largest read or page of rooms.
- A client that gives up waiting (`Radio did not answer`) can't tell whether a write was applied:
  the daemon may still apply it after the client has gone. Read before retrying a post.
- Unknown request fields are refused (`deny_unknown_fields`), so a client sends only what the daemon
  announced. A client reading an answer ignores fields it doesn't know: a newer minor adds them.
- A failure answers `{"ok": false, "error": "<phrase>"}`, plus `"refused": true` when the request was
  refused (another user, not a member, someone else's status line) rather than failed. Phrases are
  fixed words, never a message's text.

## Versions

- **Every request carries `"v": <major>`.** For a major the daemon doesn't serve, the answer is
  `{"ok": false, "error": "unsupported_version", "supported": [<majors>]}` and nothing else.
- **`hello`** needs no registration. It answers
  `{"ok": true, "radio": "<semver>", "protocol": {"major": <n>, "minor": <n>}, "majors": [<n>, …], "ops": [<op>, …]}`.
  A client calls it once per process and then uses only the ops and optional fields that minor lists.
- **Within a major, change is additive only.** A new op or a new optional field is a minor bump. Nothing
  is removed, and nothing changes meaning.
- **A breaking change is a new major.** The daemon serves it and the major before it while any
  registered client still speaks the older one.
- **Why that is enough:** an install never replaces a newer daemon with an older one, so the daemon on a
  computer is never older than the copy an app bundles. The only skew left is an app older than the
  daemon, and the previous-major rule covers it.

## Members

- An author or member is `{"kind": "<kind>", "name": "<name>"}`.
- The kind `human` is the person at the computer, who is always a member, under any name.
- Every other kind is a word an app chose and registered: 1 to 40 characters, a lowercase ASCII letter
  first, then lowercase letters, digits, `_` or `-`. The daemon gives no kind but `human` a meaning.
- `register` replaces that app's whole list. A post or status change by anyone but the human is refused
  `not a member` unless some registered app lists that kind and name. Registrations are kept across
  restarts of the daemon.
- A member who isn't the human changes only the status lines it set (same kind and name); the human
  changes any line.

## Ops in major 1

| Op | Request fields | Answer fields | What it does |
| --- | --- | --- | --- |
| `hello` | none | `radio`, `protocol`, `majors`, `ops` | Names the daemon's version, protocol and ops (above) |
| `register` | `app`, `app_version`, `members: [member]` | none | An app says who its local members are (at most 256) |
| `create_room` | `room`, `title` | none | Opens a room homed on this computer |
| `post` | `room`, `author: member`, `body` | `seq` | Adds a message (at most 4 KiB) to a room |
| `status_set` | `room`, `key`, `line`, `author: member`, optional `expires_at` | `seq` | Sets one status line in a room, and logs it as a message |
| `status_clear` | `room`, `key`, `author: member` | `removed`, `seq` when removed | Clears it, and logs that |
| `list_rooms` | optional `after`, optional `limit` (1 to 500) | `rooms: [{id, home, title, created, last_seq}]` | The rooms by id, those after `after` when given, at most `limit` (500 by default); page with the last id |
| `read` | `room`, optional `since`, optional `limit` (1 to 500) | `messages: [{seq, kind, author_host, author, body, sent_at, delivered_at?}]` | The newest `limit` messages (50 by default), or with `since` those numbered after it (500 by default), oldest first |
| `status` | `room` | `status: [{key, line, set_by, set_at, expires_at?}]` | The room's live status lines |
| `id` | none | `id` | This computer's link id: the public half of its key, 64 lowercase hex characters. The first call makes the key |
| `fleet_init` | `name` | `fleet: {id, name, version, founder}`, `recovery_file` | Makes a fleet with this computer as its founding member (its key is made if missing). `name` is 1 to 32 characters: a lowercase letter, then lowercase letters, digits or `-`. Writes the fleet's recovery key, once, to the file named in `recovery_file` (0600): move it into a password manager and delete the file. Fails when this computer is already in a fleet |
| `code_create` | optional `name`, optional `ttl` (60 to 86400 seconds, 1800 by default) | `code`, `code_id`, `expires_at` | Makes a one-time join code for this computer's fleet. `code` is the only copy of its secret: Radio keeps a SHA-256 of it. `name` (the name rule above) only labels it in `code_list`. At most 64 codes are open at once |
| `code_list` | none | `codes: [{id, name?, created, expires_at, uses}]` | The open join codes, oldest first, without their secrets |
| `code_revoke` | `id` | `removed` | Closes an open join code |
| `computer_list` | none | `computers: [{id, name?, state, invited_by?, trusted}]` | This computer's view of its fleet: every other computer, by name, then those removed or gone, by id. `id` is a computer's link id; `state` is `accepted` (it may connect), `pending` (introduced by a computer this one doesn't trust, and waiting for the person to accept it), `blocked` (refused here only), `removed` or `left`; `invited_by` is its inviter's id; `trusted` means a computer it invites is accepted here at once |
| `computer_accept` | `computer` | `computer` | Lets a pending or blocked computer connect. `computer` is its name or its id |
| `computer_block` | `computer` | `computer` | Refuses a computer on this computer only; never sent to the others. `computer_accept` lifts it |
| `computer_trust` | `computer` | `computer` | Marks an accepted computer trusted: the computers it invites from now on are accepted here without asking |
| `resign` | `app`, optional `section`, optional `roles` | `record: {id, name, version, roles, section?}` | Re-signs this computer's record with the app's new section: a string of up to 4,096 bytes that Radio carries to the fleet's other computers and never reads. An empty `section` removes the app's section; without `section` it stays. `roles` (at most 8 names, by the computer-name rule) replaces the computer's roles; without it they stay. The app must be registered; a computer holds at most 8 apps' sections (`too many app sections`), and the whole record must fit 48 KiB as JSON (`the computer record is too large`). The record's name is the daemon's name (its host name, or `radio serve --name`), which must follow the computer-name rule (`not a computer name`). A change that changes nothing keeps the record and its version; otherwise the version rises. Answers the record with this app's section |
| `computer_records` | `app` | `records: [{id, name, version, roles, section?}]` | This computer's record and those of the computers it has accepted, by id: each one's name, the newest version this computer holds, its roles and this app's section, when it has one. A record is kept only when it is signed by the computer it names and newer than the one held; a pending or blocked computer's record is kept but not listed until it is accepted |
| `link_address_add` | `address` | `port`, `addresses` | Adds one of this computer's link addresses, the address its other computers reach it at (on a VPN, its VPN address). `address` is an IP address with no port: IPv4 or IPv6, never unspecified, multicast, broadcast or IPv6 link-local, and an IPv4-mapped IPv6 address is kept as IPv4. Anything else, a port or a name included, answers `not a link address`. At most one of each family: a second answers `this computer already has a link address of that family; remove it first`; adding one already there changes nothing. Works with or without a fleet, and leaving a fleet keeps them. `port` is the link's one fixed UDP port (7341); `addresses` is the list after the change, IPv4 first |
| `link_address_list` | none | `port`, `addresses` | This computer's link addresses and the fixed port |
| `link_address_remove` | `address` | `removed`, `port`, `addresses` | Removes one of this computer's link addresses; `removed` says whether it was there. An `address` that is not an IP address answers `not a link address`; an IP address that could never be a link address answers `removed: false` |

A message's `kind` is `post`, `status_set` or `status_clear` today; a later minor may add kinds, so a
client reads it as a word. Times are seconds since the Unix epoch.

A join code is one line: `rj1.<fleet id>.<inviter id>.<secret>.<hints>`, where the fleet and inviter ids are 64 lowercase hex characters, the secret is 32 bytes and the hints are a JSON object `{"direct": ["<ip>:<port>", …]}` listing exactly the inviting computer's link addresses on the fixed port when the code was made (none when it had none), both in unpadded base64url. Treat it as a password: pass it on standard input or through the socket, never on a command line, and never log it.

A computer's record is signed by that computer's own key and carries its name, its link id, its roles, the addresses it can be reached at and one section per app (at most 8). Radio checks the signature and the version of every record it receives and carries the sections without reading them, so an app's section is for that app on the other computers to parse. Sections are not secret: every computer in the fleet holds them.

Every read goes through the socket: the daemon is the only process that opens its rooms store. The
exact field names and types of each op are `radio-client`'s types. `contract/v1/` holds a fixture per
op, replayed against a real daemon and through `radio-client`'s types, so a minor that would break an
older client fails its test. Ops for deliberations, seats and the link between computers arrive as
minors of major 1 when they are built.
