# AGENTS.md: radio

Read in this order: `README.md`, `.ai/PROJECT.md`, `.ai/DECISIONS.md`, `.ai/SECURITY.md`, then
`CONTRACT.md`. Run `bash scripts/ci-local.sh` before anything lands on `main`.

Gotchas, before anything else:

- **No app and no person in this repo** (D3): not in code, docs, tests, paths or commit messages. Apps
  are clients registered by name, in data; tests use invented people and apps.
- **The contract changes only as `CONTRACT.md` allows:** additive within a major, a new major to break.
  Update `CONTRACT.md`, `radio-client` and the README's *For app developers* section together.
- **No fixed paths.** Scripts find the repo from their own location.
- **One README, plain first** (D13). Its sections up to *Building an app that uses Radio* are for
  anyone, in plain words with no code; *For app developers* holds the commands and the worked client;
  the exact rules stay in `CONTRACT.md`.
