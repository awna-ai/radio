# radio

One daemon per computer that several apps share for rooms, messages and, later, the link to other
computers. Apps talk to it over a local socket; the contract is in [`CONTRACT.md`](CONTRACT.md).

`radio serve` runs the daemon: rooms and the socket, on one computer. Installing it and the link
between computers come next. Start with [`.ai/PROJECT.md`](.ai/PROJECT.md).

Run every check with `bash scripts/ci-local.sh`.

MIT licensed.
