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

## How it's used

**If you use an app that has it:** there's nothing to do. The app installs Radio when you install
the app. Remove that app and Radio stays for the others. When the last app that uses it is removed,
Radio goes too, and your conversations are kept unless you delete them.

**If you make an app:** your app carries a copy of Radio and runs one install command. After that,
it talks to Radio by sending short text messages over a private connection on the computer. Any
programming language can do that; there's no library you have to use. The guide for app makers is
[docs/app-developers.md](docs/app-developers.md), and the exact rules are in
[CONTRACT.md](CONTRACT.md).

## Building an app that uses Radio

In plain steps, from the first idea to an app people can install. The guide for app makers,
[docs/app-developers.md](docs/app-developers.md), has the exact commands and messages for each one.

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
short script. The guide includes a short one in Python.

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
