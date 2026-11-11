# The Radio's socket contract

Protocol major: 1
Protocol minor: 0

This is the one contract between the Radio and the apps that use it. `radio-client` carries the same
numbers as constants, and a test checks the two lines above against them. The protocol's numbers are
independent of the repo's semver tags.

## Where and who

- The socket is `~/.local/state/radio/radio.sock`, mode 0600, in a folder of mode 0700.
- One daemon listens, holding an exclusive lock on `radio.sock.lock` beside it.
- A connecting process must run as the same user as the daemon (its peer uid is checked on every
  connection). Anyone else is refused before a byte is read.

## Framing

- One request per connection: one line of JSON in, one line of JSON out, then the daemon closes.
- A request is at most 16 KiB. The daemon answers within 5 seconds or drops the connection.
- Unknown fields are refused (`deny_unknown_fields`), so a client sends only what the daemon announced.

## Versions

- **Every request carries `"v": <major>`.** For a major the daemon doesn't serve, the answer is
  `{"ok": false, "error": "unsupported_version", "supported": [<majors>]}` and nothing else.
- **`hello`** needs no registration. It answers
  `{"radio": "<semver>", "protocol": {"major": <n>, "minor": <n>}, "majors": [<n>, …], "ops": [<op>, …]}`.
  A client calls it once per process and then uses only the ops and optional fields that minor lists.
- **Within a major, change is additive only.** A new op or a new optional field is a minor bump. Nothing
  is removed, and nothing changes meaning.
- **A breaking change is a new major.** The daemon serves it and the major before it while any
  registered client still speaks the older one.
- **Why that is enough:** an install never replaces a newer daemon with an older one, so the daemon on a
  computer is never older than the copy an app bundles. The only skew left is an app older than the
  daemon, and the previous-major rule covers it.

## Ops in major 1

| Op | Request fields | What it does |
| --- | --- | --- |
| `hello` | none | Names the daemon's version, protocol and ops (above) |
| `register` | `app`, `app_version`, `members: [{kind, name}]` | An app says who its local members are. The person at the computer is always one |
| `create_room` | the room's fields | Opens a room |
| `post` | a room, an author, the message | Adds a message to a room |
| `status_set` | a room, an author, the status | Sets an author's status in a room |
| `status_clear` | a room, an author | Clears it |
| `list_rooms` | none | Lists the rooms |
| `read` | `room`, optional `since`, optional `limit` | Reads a room's messages |
| `status` | `room` | Reads a room's statuses |

Every read goes through the socket: the daemon is the only process that opens its rooms store. The
exact field names and types of each op are `radio-client`'s types, and a fixture per op per major
replays them, so a minor that would break an older client fails its test. Ops for deliberations,
seats and the link between computers arrive as minors of major 1 when they are built.
