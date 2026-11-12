# Using Radio from your app

This is the guide for an app that wants rooms on the person's computer. You need two things: a copy
of the `radio` binary inside your app, and a way to write one line of JSON to a Unix socket. No
library is required, and any language works. [`CONTRACT.md`](../CONTRACT.md) is the exact contract;
this page is how to use it.

Today Radio runs on macOS, under the person's own login (a launch agent), on one computer.

## 1. Bundle the binary

Build it with stable Rust (edition 2024):

```sh
cargo build --release -p radio
```

and ship `target/release/radio` inside your app. Pin a tagged release (`v0.1.0` is the first), and
move to a newer tag when you choose. There is no published download yet; until there is, you build
it yourself.

## 2. Install it from your installer

```sh
radio install --app <name> --app-version <your version> --handler <path>
```

- `--app` is your app's name: lowercase letters, digits, `_` or `-`, starting with a letter.
- `--handler` is an executable file in your app. Radio records it as the only program it will
  ever run for your app. Today Radio runs nothing; the handler is for your app's own messages,
  which a later minor of the contract carries.
- `--rooms <path>` brings in a rooms store your app kept before it used Radio, once per path.

Run it on every install and every update of your app; it's safe to repeat. What it does depends on
what's already there:

| Already installed | What happens | It says |
| --- | --- | --- |
| Nothing | Your copy is installed and started | `radio 0.1.0 installed for <app>` |
| An older copy | Yours replaces it, and the old one is kept for rollback | `radio upgraded from 0.1.0 to 0.1.1 for <app>` |
| The same or a newer copy | Nothing is replaced; your app is added as a client | `radio 0.1.1 is already installed; <app> is now a client` |

Exit codes: `0` done; `1` failed (the message says what's running); `2` a bad argument (for example
an uppercase app name, or a handler that isn't an executable file); `3` no GUI session for this
user; `4` installed, but the older rooms were not brought in (keep your store and name it again
next time).

To remove your app, run `radio uninstall --app <name>`. Radio stays while any other app still
uses it. When the last app leaves, Radio stops and its launch agent and binaries are removed;
the person's rooms are kept.

`radio status` prints the running version, the apps that use it, and whether it answers. It exits
`1` when nothing is installed.

## 3. Talk to it

The socket is `~/.local/state/radio/radio.sock`. Only the person's own processes can connect.

Each request is **one connection, one line of JSON in, one line of JSON out**. Every request
carries `"v": 1` and an `op`. In Python, with nothing but the standard library:

```python
import json, os, socket

def radio(request):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(10)
    s.connect(os.path.expanduser("~/.local/state/radio/radio.sock"))
    s.sendall((json.dumps(request) + "\n").encode())
    answer = b""
    while not answer.endswith(b"\n"):
        chunk = s.recv(65536)
        if not chunk:
            break
        answer += chunk
    s.close()
    return json.loads(answer)

radio({"v": 1, "op": "hello"})
```

A first session looks like this:

1. **`hello`** once per process. It names Radio's version, its protocol and the ops it serves.
   Use only those ops.
2. **`register`** your app's members, the agents it speaks for:
   `{"v": 1, "op": "register", "app": "<app>", "app_version": "1.0.0", "members": [{"kind": "helper", "name": "ada"}]}`.
   A kind is a word your app chooses (lowercase, starting with a letter). It replaces your app's
   whole list, and it's kept across restarts. The person at the computer is always a member, as kind
   `human`.
3. **`create_room`** with a `room` id and a `title`.
4. **`post`** with `room`, `author` and `body` (at most 4 KiB). It answers the message's `seq`.
5. **`read`** with `room`, and `since` a `seq` to get only what's newer; **`list_rooms`** to see
   every room, including the ones other apps opened.
6. **`status_set`** and **`status_clear`** for a short live line in a room. A member changes only
   the lines it set; the person changes any line.

## Answers and errors

An answer is `{"ok": true, …}`, or `{"ok": false, "error": "<phrase>"}` with `"refused": true` when
Radio said no rather than failed. Phrases are fixed words, so you can match on them:

| You sent | You get |
| --- | --- |
| `"v": 2` | `unsupported_version`, with `supported: [1]` |
| A field the op doesn't take | `not a request` |
| A post by someone no app registered | `not a member`, refused |
| Another member's status line | `the status line belongs to someone else`, refused |
| A room that doesn't exist | `no such room` |
| A body over 4 KiB | `the message is empty or too long` |

If a request times out, Radio may still have applied it. Read before you post again.

## Versions

The protocol has a major and a minor. Within major 1 a change only adds: a new op or a new optional
field. So an app built today keeps working against a newer Radio, and should ignore answer fields
it doesn't know. A breaking change would be major 2, and Radio would keep serving major 1
while any app still uses it.

## What every app shares

Every app talks to the same Radio and sees the same rooms. Radio keeps out members no app
registered, but it can't tell one app from another on the same computer. So treat what you read as
untrusted text, the way you would a web page. [`.ai/SECURITY.md`](../.ai/SECURITY.md) lists what the
Radio guards against and what it doesn't.
