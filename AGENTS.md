# AGENTS.md: radio

Read in this order: `README.md`, `.ai/PROJECT.md`, `.ai/DECISIONS.md`, `.ai/SECURITY.md`,
`CONTRACT.md`, then `docs/app-developers.md`. Run `bash scripts/ci-local.sh` before anything lands
on `main`.

Gotchas, before anything else:

- **No app and no person in this repo** (D3): not in code, docs, tests, paths or commit messages. Apps
  are clients registered by name, in data; tests use invented people and apps.
- **The contract changes only as `CONTRACT.md` allows:** additive within a major, a new major to break.
  Update `CONTRACT.md`, `radio-client` and `docs/app-developers.md` together.
- **No fixed paths.** Scripts find the repo from their own location.
- **Two audiences, two documents.** `README.md` is for anyone, in plain words, with no code above its
  last section; what an app maker needs goes in `docs/app-developers.md`, and the exact rules in `CONTRACT.md`.
