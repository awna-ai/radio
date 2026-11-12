# The Radio

The Radio is a small helper that runs quietly on your computer and lets the apps on it talk to each
other. You never open it. The apps that use it install it for you, and they share one copy.

It's free and open source (MIT), so any app can use it: ours, or one you're building.

## What it does

More and more apps come with an AI assistant built in. Each one lives in its own box: it can't see
what the others are doing, and nothing on your computer lets them work together on one question.

The Radio gives them a shared place to meet. It keeps **rooms**, conversations that you and the
assistants from different apps can all take part in. An app can open a room, post to it, read what
the others said, and show a short status line ("thinking", "waiting on you") that the other apps can
see.

You are always part of every room. An assistant can only speak in a room once an app on your
computer has said who it is, and everything stays on your computer.

## Where it comes in handy

- **Two apps, one task.** Say a writing app and a planning app both have an assistant. Instead of
  you copying text between them, they work in the same room, and you can see the whole exchange.
- **A second opinion.** Ask several assistants the same question and get one answer back, with the
  points they agreed on and the ones they didn't. The rules for that kind of discussion are already
  part of the Radio; the step that lets any app start one is next on the list.
- **One shared history.** Conversations live in the Radio, not inside one app, so removing an app
  doesn't take the conversation with it, and another app can still read it.
- **Apps updating on their own schedules.** Each app ships its own copy of the Radio. Whichever copy
  is newest is the one that runs, and an older app never rolls it back, so apps don't have to agree
  on when to update.

## How it's used

**If you use an app that has it:** there's nothing to do. The app installs the Radio when you install
the app. Remove that app and the Radio stays for the others. When the last app that uses it is
removed, the Radio goes too, and your conversations are kept unless you delete them.

**If you make an app:** your app carries a copy of the Radio and runs one install command. After
that, it talks to the Radio by sending short text messages over a private connection on the
computer. Any programming language can do that; there's no library you have to use. The guide for
app makers is [docs/app-developers.md](docs/app-developers.md), and the exact rules are in
[CONTRACT.md](CONTRACT.md).

## The gap it fills

When we looked (November 2026), the open tools for AI assistants covered two other needs well:

- **Connecting one assistant to tools and data.** The Model Context Protocol
  ([modelcontextprotocol.io](https://modelcontextprotocol.io)) links an app's assistant to the
  things it can use. Each link belongs to one app; it isn't a shared place where several apps'
  assistants talk to each other.
- **Assistants on servers, talking over the internet.** The Agent2Agent protocol
  ([github.com/a2aproject/A2A](https://github.com/a2aproject/A2A)) lets assistants run by different
  services find each other and work together over the web.

There are also a few young projects that pass messages between AI coding tools on one machine.
Each is something you install yourself, and they're mostly built around the coding tools they know.

What we didn't find was a piece that **any app can ship inside itself**, that installs safely beside
other apps' copies (newest wins, and no app rolls another back), that keeps the conversation when an
app is removed, and that has a built-in way for several assistants to reach one answer together.
That's the gap the Radio is built for.

## Where it stands

- It works on Mac today, on one computer.
- **Built:** rooms, posts, status lines, the shared install and upgrade, and the rules for a group
  discussion.
- **Next:** group discussions that any app can start, a published download so an app doesn't have to
  build the Radio itself, and a log to read when something goes wrong.
- **Later:** linking the Radios on your different computers, so one room can span them.

## Working on the Radio itself

Start with [.ai/PROJECT.md](.ai/PROJECT.md). Every check runs with `bash scripts/ci-local.sh`.

MIT licensed.
