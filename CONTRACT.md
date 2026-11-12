# Radio's socket contract

Protocol major: 1
Protocol minor: 0

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
- A client that gives up waiting (`the Radio did not answer`) can't tell whether a write was applied:
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

A message's `kind` is `post`, `status_set` or `status_clear` today; a later minor may add kinds, so a
client reads it as a word. Times are seconds since the Unix epoch.

Every read goes through the socket: the daemon is the only process that opens its rooms store. The
exact field names and types of each op are `radio-client`'s types. `contract/v1/` holds a fixture per
op, replayed against a real daemon and through `radio-client`'s types, so a minor that would break an
older client fails its test. Ops for deliberations, seats and the link between computers arrive as
minors of major 1 when they are built.
