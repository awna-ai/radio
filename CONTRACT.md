# Radio's socket contract

Protocol major: 1
Protocol minor: 7

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
| `join` | `code` | `fleet: {id, name, version, founder}` | Joins the fleet of the computer that made `code` (`code_create` there): Radio dials the addresses the code carries, pinned to that computer's key, and answers once the inviter has let this computer in. Pass the code through the socket or `radio join`'s standard input, never a command line. `not a join code`, `already in a fleet`, and `the join code has no address to dial` (a code made before its computer had a link address) answer at once; `the computer could not be reached` (nothing answered, or a different key did), `the join was refused` and `the computer did not answer` come from the other computer. Needs a link address here |
| `computer_remove` | `computer` | `delivered` | Removes another computer from the fleet for everyone. Radio signs the removal (only the computer's inviter or the founding member may) and sends it to the removed computer (when it is accepted here and has an address) and to every accepted computer it has an address for; `delivered` counts those it reached. One that was off misses it. Past 110 seconds it answers `still telling the other computers; check computer_list later` and finishes in the background, unless the daemon stops or a link address changes first. Needs a link address here |
| `leave` | none | `delivered` | Takes this computer out of its fleet: Radio signs the leave, sends it to every accepted computer it has an address for (`delivered` counts those it reached), then forgets the fleet. Past 110 seconds it answers `still telling the other computers; check computer_list later` and finishes in the background, unless the daemon stops or a link address changes first (a leave cut short that way has told some computers and may still hold the fleet here). Needs a link address here |
| `send` | `app`, `computer`, `name`, `file` | `size` | Sends one file to this app on one computer of the fleet, named by its name or id. The app must be registered (`the app is not registered`) and the link directory must be set (`the link is not set up`). Starts the link first; needs a link address here (`this computer has no link address`) and a valid daemon name (`this computer's name breaks the computer-name rule; restart Radio with --name`). `file` must be absolute (`the file must be an absolute path`), and `name` must be 1 to 128 bytes, each in `A-Z a-z 0-9 . _ -`, not starting with a dot (`not a file name`). A final link or a file that cannot be opened answers `the file could not be opened`; a non-regular file answers `not a regular file`; over 64 MiB answers `the file is larger than 64 MiB`. The computer lookup answers `not in a fleet`, `not a computer`, `no such computer` or `the computer was removed`; a computer not accepted here answers `the computer is not let in`, and one without addresses answers `no address is known for that computer`. A dial that fails answers `the computer could not be reached`; a missing or invalid answer answers `the computer did not answer`. A refusal answers `that app takes no files on that computer`, `a file with that name is already there`, `that computer is busy; try again`, `that app's folder on that computer is not usable`, `that computer could not write the file`, `that app's folder on that computer is full` or `that computer has no room for the file`. A source that grows, shrinks or becomes unreadable answers `the file changed while it was sent`. A stopped link answers `the link stopped`; the outer wait answers `the send did not finish in time`. The answer comes within 135 seconds, served beside other clients; at most 4 sends wait at once, and a fifth answers `too many sends at once; try again`. The client waits 140 seconds. A send in flight when Radio stops gets no answer. `size` is the number of bytes sent |
| `delivery_folder_set` | `app`, `folder` | none | Radio makes a dedicated folder, mode 0700, for this app to take files in on this computer, with or without a fleet; leaving a fleet keeps it. The app must be registered (`the app is not registered`) and installed (`the app is not installed`), and the link directory must be set (`the link is not set up`). `folder` must be absolute (`the folder must be an absolute path`) with a normal last component (`not a folder name`), after trailing slashes are stripped. The path must not exist (`the folder already exists; name a new one`); setting the same path again is fine only while it is still the folder Radio made for this app, recorded by device and inode. A path held by another app answers `that folder is another app's`; a folder Radio cannot make or open answers `the folder could not be made`. An invalid app name answers `not an app name`. Changing the path makes a new folder and leaves the old folder and its files as they are |
| `delivery_folder_clear` | `app` | `removed` | Clears only this app's delivery-folder row, leaving the folder and its files; `removed` says whether one was set. The app must be registered (`the app is not registered`) and the link directory must be set (`the link is not set up`). Works with or without a fleet |
| `deliveries` | `app`, optional `after` (0 by default), optional `limit` (100 by default) | `deliveries: [{id, computer, key, name, size, at}]` | Lists only this app's arrivals in id order, strictly after `after`, at most `limit`. The app must be registered (`the app is not registered`) and the link directory must be set (`the link is not set up`). `limit` must be 1 to 500 (`not a limit`); `after` must be a non-negative integer (`not a request`). `computer` is Radio's name for the sender, `key` its authenticated endpoint id in hex, `name` the landed file name, `size` its bytes and `at` seconds since the Unix epoch. Works with or without a fleet |

Files: a file goes to one app on one computer of the fleet, up to 64 MiB, only when
that computer answers now. Nothing is kept to resend. It lands in the folder that app
set on that computer, as its own name, mode 0600, never over another file and never
through a link; `deliveries` lists what arrived.

A message's `kind` is `post`, `status_set` or `status_clear` today; a later minor may add kinds, so a
client reads it as a word. Times are seconds since the Unix epoch.

A join code is one line: `rj1.<fleet id>.<inviter id>.<secret>.<hints>`, where the fleet and inviter ids are 64 lowercase hex characters, the secret is 32 bytes and the hints are a JSON object `{"direct": ["<ip>:<port>", …]}` listing exactly the inviting computer's link addresses on the fixed port when the code was made (none when it had none), both in unpadded base64url. Treat it as a password: pass it on standard input or through the socket, never on a command line, and never log it.

A computer's record is signed by that computer's own key and carries its name, its link id, its roles, the addresses it can be reached at and one section per app (at most 8). Radio checks the signature and the version of every record it receives and carries the sections without reading them, so an app's section is for that app on the other computers to parse. Sections are not secret: every computer in the fleet holds them.

Every read goes through the socket: the daemon is the only process that opens its rooms store. The
exact field names and types of each op are `radio-client`'s types. `contract/v1/` holds a fixture per
op, replayed against a real daemon and through `radio-client`'s types, so a minor that would break an
older client fails its test. Ops for deliberations, seats and the link between computers arrive as
minors of major 1 when they are built.
