# radio

One daemon per computer that several apps share for rooms, messages and, later, the link to other
computers. Apps talk to it over a local socket; the contract is in [`CONTRACT.md`](CONTRACT.md).

`radio serve` runs the daemon: rooms and the socket, on one computer. An app installs the copy it
bundles with `radio install --app <name> --app-version <version> --handler <path>`; the newest copy
any app brings is the one that runs, and `radio status` says which. The link between computers comes
next. Start with [`.ai/PROJECT.md`](.ai/PROJECT.md).

Run every check with `bash scripts/ci-local.sh`.

MIT licensed.
