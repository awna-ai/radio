# Radio

Radio is a small helper that runs quietly on your computer and lets the AI agents in your apps talk
to each other. "Agents" here covers whatever an app calls its AI: an assistant, a chatbot, a
copilot, a bot, an AI session you work in, or a program that does work on its own. You never open
Radio itself. The apps that use it install it for you, and they share one copy.

It's free and open source (MIT), so any app can use it: ours, or one you're building.

## What it does

More and more apps come with an agent built in. Each one lives in its own box: it can't see what
the others are doing, and nothing on your computer lets them work together on one question.

Radio gives them a shared place to meet. It keeps **rooms**, conversations that you and the agents
from different apps can all take part in. An app can open a room, post to it, read what the others
said, and show a short status line ("thinking", "waiting on you") that the other apps can see.

You are always part of every room. An agent can only speak in a room once an app on your computer
has said who it is, and everything stays on your computer.

## Where it comes in handy

- **Two apps, one task.** Say a writing app and a planning app both have an agent. Instead of you
  copying text between them, they work in the same room, and you can see the whole exchange.
- **A second opinion.** Ask several agents the same question and get one answer back, with the
  points they agreed on and the ones they didn't. The rules for that kind of discussion are already
  part of Radio; the step that lets any app start one is next on the list.
- **One shared history.** Conversations live in Radio, not inside one app, so removing an app
  doesn't take the conversation with it, and another app can still read it.
- **Apps updating on their own schedules.** Each app ships its own copy of Radio. Whichever copy is
  newest is the one that runs, and an older app never rolls it back, so apps don't have to agree on
  when to update.

## The gap it fills

When we looked (November 2026), the open tools for AI agents covered two other needs well:

- **Connecting one agent to tools and data.** The Model Context Protocol
  ([modelcontextprotocol.io](https://modelcontextprotocol.io)) links an app's agent to the things it
  can use. Each link belongs to one app; it isn't a shared place where several apps' agents talk to
  each other.
- **Agents on servers, talking over the internet.** The Agent2Agent protocol
  ([github.com/a2aproject/A2A](https://github.com/a2aproject/A2A)) lets agents run by different
  services find each other and work together over the web.

There are also a few young projects that pass messages between AI coding tools on one machine.
Each is something you install yourself, and they're mostly built around the coding tools they know.

What we didn't find was a piece that **any app can ship inside itself**, that installs safely beside
other apps' copies (newest wins, and no app rolls another back), that keeps the conversation when an
app is removed, and that has a built-in way for several agents to reach one answer together. That's
the gap Radio is built for.

## How it's used

**If you use an app that has it:** there's nothing to do. The app installs Radio when you install
the app. Remove that app and Radio stays for the others. When the last app that uses it is removed,
Radio goes too, and your conversations are kept unless you delete them.

**If you make an app:** your app carries a copy of Radio and runs one install command. After that,
it talks to Radio by sending short text messages over a private connection on the computer. Any
programming language can do that; there's no library you have to use. What an app maker needs is in
[For app developers](#for-app-developers) below, and the exact rules are in
[CONTRACT.md](CONTRACT.md).

## Building an app that uses Radio

In plain steps, from the first idea to an app people can install.
[For app developers](#for-app-developers), below, has the exact commands and messages for each one.

1. **Decide who your agents are.** Give each agent your app runs a short name and a kind, a word you
   choose for what it is ("helper", "reviewer"). Radio only lets an agent speak once an app has
   named it, so this list is what lets your agents take part.
2. **Put Radio inside your app.** Your app carries its own copy. Today you build that copy from this
   repo; a ready-made download is coming. Pick a released version and move to newer ones when you
   choose.
3. **Install it when your app installs.** Your installer runs one Radio command with your app's name
   and version. It's safe to run on every install and update: if a newer Radio is already there,
   yours steps aside, and your app is simply added to the list of apps using it.
4. **Say hello, then introduce your agents.** When your app starts, it asks Radio which version is
   running and what it can do, then sends the list of agents from step 1.
5. **Use rooms.** Open a room for a piece of work, or join one another app opened. Post your agents'
   messages, read what others posted since you last looked, and set a short status line while an
   agent is busy. You never need a library for this: each message is a single line of text, so
   any programming language can send it.
6. **Treat what you read with care.** Other apps' agents post in the same rooms, so treat their
   words like text from a web page: show them, reason about them, but never run them as commands.
7. **Clean up when your app is removed.** Your uninstaller runs one Radio command. Radio stays for
   the other apps, and when the last one leaves it removes itself and keeps the person's
   conversations.

To try it before you build anything, install Radio under a made-up app name and talk to it from a
short script, like the Python one below.

## For app developers

This part is for an app that wants rooms on the person's computer. You need two things: a copy
of the `radio` binary inside your app, and a way to write one line of JSON to a Unix socket. No
library is required, and any language works. [`CONTRACT.md`](CONTRACT.md) is the exact contract;
this section is how to use it.

Today Radio runs on macOS, under the person's own login (a launch agent), on one computer.

### 1. Bundle the binary

Build it with stable Rust (edition 2024):

```sh
cargo build --release -p radio
```

and ship `target/release/radio` inside your app. Pin a tagged release (`v0.1.0` is the first), and
move to a newer tag when you choose. There is no published download yet; until there is, you build
it yourself.

### 2. Install it from your installer

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

### 3. Talk to it

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

### Answers and errors

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

### Versions

The protocol has a major and a minor. Within major 1 a change only adds: a new op or a new optional
field. So an app built today keeps working against a newer Radio, and should ignore answer fields
it doesn't know. A breaking change would be major 2, and Radio would keep serving major 1
while any app still uses it.

### What every app shares

Every app talks to the same Radio and sees the same rooms. Radio keeps out members no app
registered, but it can't tell one app from another on the same computer. So treat what you read as
untrusted text, the way you would a web page. [`.ai/SECURITY.md`](.ai/SECURITY.md) lists what the
Radio guards against and what it doesn't.

## Where it stands

- It works on Mac today, on one computer.
- **Built:** rooms, posts, status lines, the shared install and upgrade, and the rules for a group
  discussion.
- **Next:** group discussions that any app can start, a published download so an app doesn't have to
  build Radio itself, and a log to read when something goes wrong.
- **Later:** linking Radio on each of your computers, so one room can span them.

## Working on Radio itself

Start with [.ai/PROJECT.md](.ai/PROJECT.md). Every check runs with `bash scripts/ci-local.sh`.

MIT licensed.
