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

## Across your computers (coming)

Today Radio works on one computer. Later, it will link the copies on your other computers, so one
room can span them: an agent on your laptop and one on your desktop can work in the same
conversation. None of this is built yet; this is how it's meant to work.

- **You pair a computer once.** One of your computers makes a one-time code, and the new computer
  uses it to join. Your other computers then accept it, on their own or after asking you.
- **Each computer has its own identity.** Each one holds its own key, and your computers know each
  other by it. Only computers you've let in can connect; any other is turned away.
- **First, between computers that can already reach each other.** The first version links
  computers on the same network, or on a private network (a VPN) you already run between them:
  you tell each computer the other's address once. It sends files from an app on one computer to
  the same app on another, and nothing else yet.
- **Later, at home or away.** It will work on the same network or over the internet. Your computers find each
  other on a home or office network by themselves, and connect directly wherever the network
  allows; when it doesn't, they go through a relay. Each one remembers the addresses that worked
  last time and tries those first.
- **A relay only introduces.** A relay helps two computers find each other. Once they can reach
  each other directly, messages go straight between them; until then the relay passes them along.
  Messages are encrypted from one computer to the other, so a relay can see which computers talk
  and when, never what they say. If a relay goes down, computers already connected keep talking.
  Anyone can run their own relay.
- **Later, nothing else to set up.** Once relays are built, there's no router port to open and no
  account to make: pairing is the only step.

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
of the `radio` binary inside your app, and a way to write one line of JSON to a Unix socket, or
to run a command. No library is required, and any language works. [`CONTRACT.md`](CONTRACT.md) is the exact contract;
this section is how to use it.

Today Radio runs on macOS, under the person's own login (a launch agent), on one computer.

### 1. Bundle the binary

Build it with stable Rust (edition 2024):

```sh
cargo build --release -p radio
```

and ship `target/release/radio` inside your app. Pin a tagged release (`v0.1.0` is the first), and
move to a newer tag when you choose.

A tag that has a release on GitHub also carries the binary ready-built, so you don't need Rust:
`radio-<tag>-macos-arm64.tar.gz` (Apple silicon), `radio-<tag>-macos-x86_64.tar.gz` (Intel) and
`SHA256SUMS`, starting with `v0.1.0`. Fetch with `curl` (or `gh release download`), check, and
unpack:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing
tar -xzf radio-<tag>-macos-arm64.tar.gz
```

For a universal app, join the two with `lipo -create -output radio <arm64>/radio <x86_64>/radio`.

**Signing.** The released binaries are signed ad hoc and not notarised: they are made to ship inside
your app. If you notarise your app, sign `radio` with your own Developer ID, the hardened runtime and
a secure timestamp, as notarisation requires of every executable in it:

```sh
codesign --force --sign "Developer ID Application: <you>" --options runtime --timestamp radio
```

Your app's ticket then covers it, and Radio's install keeps its copy byte for byte. A `radio`
downloaded in a browser is quarantined, and macOS should refuse to run it on its own, since it
isn't notarised; a copy fetched with `curl` is not quarantined.

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
uses it. When the last app leaves, Radio stops and its launch agent, binaries, socket and lock
files are removed (the socket and its lock only when no copy of Radio is still running there);
the person's rooms and Radio's log are kept.

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

**`id`** asks for this computer's link id: the public half of its key, 64 lowercase hex characters.
The first call makes the key.

### Using Radio from the command line

Every op is also a `radio` command, so a shell script, a quick test or an app in any language can
use Radio by running the binary it bundles. Each command sends exactly its op's request over the
socket and prints Radio's answer, unchanged, as one line of JSON. It adds no op, field or phrase:
the socket stays the contract.

| Command | Op |
| --- | --- |
| `radio hello` | `hello` |
| `radio id` | `id` |
| `radio register --app <app> --app-version <version> --member <kind>:<name>` (repeat `--member`; none clears the list) | `register` |
| `radio room create <room> --title <title>` | `create_room` |
| `radio rooms [--after <room>] [--limit <n>]` | `list_rooms` |
| `radio post <room> --as <kind>:<name> --body <text>`, or `--body-file <path>` (`-` reads stdin) | `post` |
| `radio read <room> [--since <seq>] [--limit <n>]` | `read` |
| `radio line list <room>` | `status` |
| `radio line set <room> <key> <line> --as <kind>:<name> [--expires-at <seconds>]` | `status_set` |
| `radio line clear <room> <key> --as <kind>:<name>` | `status_clear` |

Status lines are under `line`, since `radio status` is the install status. `--as human:<name>` is
the person at the computer. `--expires-at` is seconds since the Unix epoch. A body file is read as
UTF-8, and Radio applies its own 4 KiB limit.

Exit codes: `0` when the answer is `"ok": true`; `3` when it is `"refused": true`; `1` for any
other failure, including Radio not running (the phrase is then on stderr, and stdout is empty);
`2` for a bad argument, with the usage (or what is wrong with the body file) on stderr and nothing
sent.

Python, with nothing but the standard library (3.7 or newer, so macOS's own `python3` runs it):

```python
from __future__ import annotations

import json
import subprocess

def radio(*args: str, body: str | None = None) -> dict:
    done = subprocess.run(["radio", *args], input=body or "", capture_output=True, text=True)
    answer = json.loads(done.stdout or "{}")
    if done.returncode != 0:
        raise RuntimeError(f"radio {args[0]}: {answer.get('error', done.stderr.strip())}")
    return answer

radio("register", "--app", "notes", "--app-version", "1.0.0", "--member", "helper:ada")
radio("room", "create", "launch-plan", "--title", "Plan the launch")
radio("post", "launch-plan", "--as", "helper:ada", "--body-file", "-", body="The first draft is ready.")

seen = 0
for message in radio("read", "launch-plan", "--since", str(seen))["messages"]:
    print(message["seq"], message["author"]["name"], message["body"])
    seen = message["seq"]
```

Node.js, in TypeScript with no packages (an ES module such as `example.mts`, so top-level `await`
works):

```ts
import { spawn } from "node:child_process";

type Answer = { ok: boolean; error?: string; refused?: boolean; [field: string]: unknown };

function radio(args: string[], body?: string): Promise<Answer> {
  return new Promise((resolve, reject) => {
    const child = spawn("radio", args, { stdio: ["pipe", "pipe", "pipe"] });
    let out = "";
    let err = "";
    child.stdout.on("data", (chunk) => (out += chunk));
    child.stderr.on("data", (chunk) => (err += chunk));
    child.on("error", reject);
    child.stdin.on("error", () => {}); // the spawn error above reports a missing radio
    child.on("close", (code) => {
      let answer: Answer;
      try {
        answer = JSON.parse(out || "{}") as Answer;
      } catch {
        return reject(new Error(`radio ${args[0]}: ${err.trim()}`));
      }
      if (code === 0) resolve(answer);
      else reject(new Error(`radio ${args[0]}: ${answer.error ?? err.trim()}`));
    });
    child.stdin.end(body ?? "");
  });
}

await radio(["register", "--app", "notes", "--app-version", "1.0.0", "--member", "helper:ada"]);
await radio(["room", "create", "launch-plan", "--title", "Plan the launch"]);
await radio(["post", "launch-plan", "--as", "helper:ada", "--body-file", "-"], "The first draft is ready.");

const { messages } = (await radio(["read", "launch-plan", "--since", "0"])) as unknown as {
  messages: { seq: number; author: { name: string }; body: string }[];
};
for (const m of messages) console.log(m.seq, m.author.name, m.body);
```

Both run the `radio` on your `PATH`; use the path of the copy your app bundles instead.

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

Two phrases come from the client side (`radio-client` and the `radio` command), never from Radio's
answers: `Radio is not running` when nothing answers on the socket, and `Radio did not answer`
when an answer doesn't come in time or can't be read. `v0.1.0` printed them as `the Radio is not
running` and `the Radio did not answer`, so an app that also runs with `v0.1.0` matches both.

### When something goes wrong

Radio writes `~/.local/state/radio/radio.log`, one line per event: when it starts (with its version,
its protocol and the computer's name), binds its socket, fails to start, and stops; when an app
installs, upgrades or keeps it, or is removed as a client, or an install fails; for every answer
that isn't `"ok": true`, the op and the phrase; and when an answer couldn't be delivered. A line
never holds what an app sent (a body, a title, a member's name). It does name the computer, which
is often its owner's name, so look before you paste a log somewhere public. For example:

```text
2026-11-13T03:40:00Z serve start version="0.1.1" protocol="1.1" name="garden-mac"
2026-11-13T03:40:00Z serve bound socket="radio.sock"
2026-11-13T03:40:01Z install installed version="0.1.1" app="kiteapp"
2026-11-13T03:41:12Z serve refused op="post" error="not a member"
2026-11-13T03:52:30Z serve stop signal="TERM"
```

Successful requests write nothing, unless the answer couldn't be delivered. The log starts over
once it passes 1 MiB. Its lines are for people to read, not part of the contract: their words may
change in any release.

### Versions

The protocol has a major and a minor. Within major 1 a change only adds: a new op or a new optional
field. So an app built today keeps working against a newer Radio, and should ignore answer fields
it doesn't know. A breaking change would be major 2, and Radio would keep serving major 1
while any app still uses it.

### What every app shares

Every app talks to the same Radio and sees the same rooms. Radio keeps out members no app
registered, but it can't tell one app from another on the same computer. So treat what you read as
untrusted text, the way you would a web page. [`.ai/SECURITY.md`](.ai/SECURITY.md) lists what
Radio guards against and what it doesn't.

### Across computers (coming)

So far only the `id` op is built (this computer's link id); the rest is being built. When it is, an
app can count on this:

- **It arrives as minors of protocol major 1.** The link ops only add to the contract, and `hello`
  lists them once a Radio serves them, so an app built today keeps working unchanged.
- **Radio checks every message from another computer before any app sees it.** It must come from a
  computer that was let in, be fresh, not replay one already seen, and fit within the size caps.
  Anything else is refused there.
- **Files first.** The first link carries files: your app asks its Radio to send a file to your app
  on another of the person's computers. The receiving Radio writes it only into a folder it made for
  your app when your app asked for one, and says which computer sent it. If the other computer can't
  be reached, the send fails within a few seconds and Radio keeps nothing, so your app tries again later.
- **Then your app's own messages go through Radio.** Your app asks its Radio to send a message to your
  app on another computer. Radio carries it without reading it, and on the other side hands it,
  with the identity of the computer that sent it, to the handler your app recorded at install
  (`--handler`); the handler's reply goes back. The handler's path only ever comes from that
  install record, never from a message.

## Where it stands

- It works on Mac today, on one computer.
- **Built:** rooms, posts, status lines, the shared install and upgrade, the rules for a group
  discussion, a ready-built download, a command for every step, and a log to read when something
  goes wrong.
- **Next:** group discussions that any app can start.
- **Later:** linking your computers, as [Across your computers](#across-your-computers-coming)
  describes.

## Working on Radio itself

Start with [.ai/PROJECT.md](.ai/PROJECT.md). Every check runs with `bash scripts/ci-local.sh`.

MIT licensed.
