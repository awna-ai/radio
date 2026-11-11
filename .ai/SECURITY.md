# Security: radio

**Last reviewed:** 2026-11-11, when the repo was founded. Nothing is built yet: this model describes the
design every step is held to, and each step that builds a boundary updates its row.

## What this system is

A daemon, one per computer, that local apps share for rooms, the deliberation protocol and, later, the
link to other computers. It stores what its clients post and what other computers send; it reads none
of the user's own files and runs no model. The worst outcome is a stranger getting a say in it: an
app may hand remote operations to its own handler through the Radio, so a hole in the Radio's
admission is a hole in that app's remote control, even though the Radio never reads those messages.

## Trust boundaries

| Boundary | What crosses it | Control (designed) | Proven how |
| --- | --- | --- | --- |
| Local process → socket | one JSON request line | 0600 socket in a 0700 folder; peer uid must equal the daemon's; 16 KiB cap; 5 s deadline; unknown fields refused; `"v"` checked first | tests in the step that builds the socket (not built yet) |
| A client app → the daemon's records | `register` (app name, version, members) | stored as data only; nothing a request names is ever run | tests in the socket step (not built yet) |
| Another computer → the Radio | link messages | an accepted member's signed records, freshness, replay and size checks on every message, before any handler sees it | tests in the link steps (not built yet) |
| The Radio → an app's handler | an app's own message, passed on unread | the handler's path comes only from that app's install record, run with fixed arguments; never from a request or a peer | tests in the install step (not built yet) |
| An app's installer → the launch agent and binaries | a new `radio` binary | an exclusive install lock; newer wins; copy to a temp file, then rename; roll back on a failed start | tests in the install step (not built yet) |

## Data classes

| Class | Examples | Where stored | Exposure impact | Retention |
| --- | --- | --- | --- | --- |
| Conversations | room messages and statuses | `~/.local/state/radio/rooms.db`, 0600 (not built yet) | private words of the people in a room | kept until the user deletes them; uninstall keeps them |
| Identity | the computer's link key | `~/.config/radio/` (not built yet) | anyone holding it can speak as this computer | rotated on loss |
| Client records | each app's name, version, handler path | `~/.local/state/radio/clients/` (not built yet) | a forged record could make the daemon run the wrong handler | removed on that app's uninstall |
| Operational | the daemon's log | `~/.local/state/radio/radio.log` (not built yet) | low, unless it carries the above (it must not) | short |

## Where secrets live

The repo needs none. On a computer, the link key will be the only secret (above). No token, key or
password belongs in the repo; gitleaks runs in `scripts/ci-local.sh`.

## Entry points that take untrusted input

- The socket: every op in `CONTRACT.md`, from any process of the same user.
- The link (later): every message from another computer.
- `radio install` arguments, from an app's installer.

## External integrations

None today. The link to other computers will be the first, and gets its own row here when it is built.

## Supply chain

- Lockfile committed; every cargo command runs `--locked`.
- No GitHub Actions; checks run locally (`scripts/ci-local.sh`) before a land.
- Each new dependency is pinned and reviewed when it is added.

## Known gaps and accepted risks

| Gap | Why accepted | Accepted by | Revisit |
| --- | --- | --- | --- |
| A process running as the same user can use the socket | the same rule as any per-user daemon; the user's own processes are trusted at this boundary | founding design, 2026-11-10 | when the link is built |
| The term check's list lives outside the repo, so a contributor without it runs no term check | the list names private terms and cannot be published | founding design, 2026-11-10 | never, by design |

## Who to tell

Report a security problem privately through GitHub's private vulnerability reporting on this repo
(its Security tab, "Report a vulnerability"), never in a public issue.

## Change log

- 2026-11-11: created from the founding design.
