# Conventions

Engineering conventions for writing-assistant: how the project is organised and operated,
not how code reads (`CODE_STYLE.md`) or how prose reads (`style-guide.md`).

Project shape: a Tauri v2 desktop app for Windows, with the Rust code in a Cargo workspace of
`crates/engine`, `crates/wa`, and `src-tauri/`, the React and TypeScript frontend under `src/`,
a browser extension in `extension/`, and a Python project in `adapter/` for the bench and the
adapter training jobs. Solo developer, public repo, MIT licensed, single intended user.

## Folder structure

CLAUDE.md's Repo layout is the top-level index. A folder is created when there is code to put
in it, not before. The Repo layout is updated in the same change that adds a top-level folder.

The Rust code is a Cargo workspace of three crates, with a virtual manifest and the one
`Cargo.lock` at the repository root:

- The engine library, `wa-engine` in `crates/engine`: capture, the analyzer, the rule layer,
  the screen, the store, the clients for the local model server and the frontier API, and the
  flag types.
- The Tauri app, `writing-assistant` in `src-tauri/`, where the Tauri CLI expects it: commands,
  the overlay, and the windows.
- `wa`, the command-line binary, in `crates/wa`.

`rust-version`, the edition, the licence, and the authors are declared once, in the workspace
manifest, and each member inherits them. A dependency two members share has its version
declared once there too.

The Tauri binary does not double as a command-line tool. `wa` runs the engine without the app,
for the app's scheduler, the Python project, and the owner by hand. Its subcommands cover the
screen, generation, embedding, synthesis, export, ingest, and backup.

Built today: in the engine, capture, spelling, LanguageTool, the telltale matcher, the
analyzer, the flag types, the settings file, and the store; in the app, the overlay and one
command. The store has its file, the thread that owns its connection, the migration runner, the
insert-only guard, and the tables `schema_version`, `passages`, those of the Card and the ruleset,
`candidates`, `screen_results`, `items`, `item_slots`, `events`, and `habit_rollups`; nothing writes
a passage or a candidate until Phase 4, an item until Phase 5, a Card or a ruleset until Phase 3, an
event until Phase 3's questionnaire, a habit rollup until the habit ledger in Phase 9, or a screen
result until the screen exists (#103), and the other artefact tables arrive with the features that
first write them. `wa` has one subcommand, `wa backup`, which copies the store file alone;
`wa screen` is planned, and each later subcommand arrives with its own issue. Every subcommand
opens the store through `Store::open_existing`, which refuses when the store is absent, so the app
is the store's one creator.
`adapter/` does not exist.

The resources stay under `src-tauri/resources/`, where the app bundles them from, and each file
exists once in the repository. The engine reaches them by relative path: its tests read the
dictionary pair, and the telltale catalogue is compiled in with `include_str!`.

Two rules follow from the split:

- A checker has one implementation, in the engine. The app calls it in process, and the bench
  calls it through `wa`, so what the bench measures is what ships.
- The engine is the only writer to the store. The Python project opens the store read-only,
  and its results enter through `wa ingest`.

A spike is kept as a harness under the `examples/` folder of the crate whose code it
exercises, `crates/engine/examples/` today, with a module comment that states what it measures
and the command that runs it.

## Dependencies and tooling

- Rust: `cargo`, with `Cargo.lock` committed.
- Frontend (`src/`): `npm`, with `package-lock.json` committed. `extension/` is plain
  JavaScript with no `package.json` of its own, linted from the root config.
- Python (`adapter/`): `uv`, with `pyproject.toml` and `uv.lock` committed and the environment
  in `.venv/`. `ruff` lints and formats, and `pytest` runs the tests. Both are CI gates in the
  Python job that arrives with `adapter/`.
- Rust lint and format: `clippy` and `rustfmt`.
- Frontend lint and format: ESLint (flat config) for logic, Prettier for formatting. Never
  ESLint rules for formatting; `eslint-config-prettier` last in the chain.
- Prettier does not format Markdown. `.prettierignore` excludes `*.md`, and prose layout
  follows `style-guide.md`.
- Frontend types: `tsc --noEmit`.
- Line endings: `.gitattributes` sets LF on every file type CI consumes and CRLF on
  Windows-native build inputs.

## CI

One GitHub Actions workflow, `.github/workflows/ci.yml`, runs on every pull request and on
every push to `main`. It has three jobs.

| Job | Runner | Steps |
|---|---|---|
| Rust | `windows-latest` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`, each from the repository root |
| Frontend | `ubuntu-latest`, Node 22 | `npm ci --ignore-scripts`, `npm run lint`, `npm run format:check`, and `npm run typecheck` |
| SonarQube Cloud | `ubuntu-latest` | Defined and disabled. #50 enables it with Rust coverage |

The Rust job runs on Windows because the engine and the app link the Windows API and build
nowhere else. Clippy's `--all-targets` flag compiles the harnesses under
`crates/engine/examples/`; CI does not run them. Third-party actions are pinned to a commit
hash, and GitHub's own actions to a major version.

No Python job exists; one arrives with `adapter/`. The Rust and Frontend jobs hold no
credentials.

## Testing

- Rust: `cargo test --workspace` from the repository root, or `cargo test -p <package>` for
  one crate. In-module `#[cfg(test)] mod tests` for unit tests; a crate's `tests/` directory
  for integration tests against the public API only.
- Frontend: Vitest. No test file exists yet; the dependency and its CI step arrive with the
  first one.
- The deterministic code gets thorough unit tests: spelling, the telltale matcher, span
  resolution through an anchor, and the rule layer and the screen as they land. It carries
  the correctness guarantees and is cheap to test.
- Tests never reach a live service. The Anthropic API, the training service, the LanguageTool
  subprocess, and llama-server are mocked.
- A harness under `crates/engine/examples/` is a measurement against a real application, run by
  hand. It is not a test.

## Logging

`log` and `env_logger` in Rust; see `CODE_STYLE.md` for levels and the no-draft-text rule.
Not yet decided: frontend logging conventions.

## Error handling

Typed errors with `thiserror` at the module level in the Rust code; `anyhow` at the Tauri
command layer. One checking source failing degrades that source and never ends checking: with
LanguageTool down, spelling and telltale flags continue. LanguageTool runs as a supervised
subprocess for the same reason, so a fault in it stays out of the app's process, and
llama-server is supervised the same way once it is bundled. Never swallow an error silently;
if it is ignorable, log it at `debug` and say why in the log line, not in a comment.

## Git

- Conventional Commits: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`.
- One-line commit subject, imperative mood, no body paragraph unless the why needs one.
  Subject under 72 characters.
- One concern per commit. Stage explicit paths; never `git add .` or `git add -A`.
- Never commit `.env` or anything that looks like a secret; see Secrets and personal data
  below.
- Never append a "co-authored with Claude Code" trailer, or any Claude/Anthropic
  attribution, to a commit, pull request, or issue.
- Don't skip hooks (`--no-verify`) once hooks exist, unless explicitly asked.

## Branches

Every change lands on `main` through a pull request, including one-line fixes; direct commits
to `main` are not made. The repository is public, and a public repository holds to
branch-then-review regardless of team size.

Branch names: `feat/<short>`, `fix/<short>`, `docs/<short>`, `chore/<short>`.

A pull request merges once GitHub Actions passes on the pull request itself, not only on a
local run of the same checks. Merge with a merge commit; delete the branch afterwards.

## GitHub issues

- Title: short, direct, states the change or problem plainly.
- Acceptance criteria are written in Gherkin (`Given / When / Then`), one scenario per
  distinct behaviour. This is the spec; there is no separate spec document.
- No fixed template beyond that; write what the issue needs, no longer.

## Secrets and personal data

- Never commit secrets, even temporarily.
- At runtime every credential lives in the Windows credential manager: the Anthropic API key,
  the AWS access pair for the model customisation jobs and their bucket, and any later
  provider key. Each is entered once in settings and read through the `keyring` crate in Rust
  and the `keyring` package in Python. Not yet built: no code reads a credential today.
- A dotenv file is permitted in development builds only. A release build never reads one.
  `.env` is gitignored. A names-only `.env.example` may be committed; none exists yet.
- A credential never appears in the config file, the store, a log line, an issue, a comment,
  or code.
- The store, the Style Card and its renderings, candidates, the judgement log, and adapters
  are personal data. They live under the data root and never enter the repository;
  `.gitignore`'s User data section is the backstop.
- The data root is `%LOCALAPPDATA%\com.bernicekoh.writing-assistant`: one folder under the
  local, non-roaming application-data directory, named by the bundle identifier, and resolved
  by one engine function, `store::data_root`, that the app and `wa` share. It holds the config
  as `config.json` directly under it, and the store, model files, adapters, and Card
  renderings as each is built. A test keeps the engine's identifier equal to the one in
  `tauri.conf.json`, and a test that reads or writes under the root is handed a temporary
  directory, never the real one. The app loads the config once at startup and writes the
  defaults when the file is missing; the controls that edit it arrive with the settings shell in
  Phase 3.
