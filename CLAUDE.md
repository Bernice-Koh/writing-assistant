# Working with Claude on writing-assistant

This file tells Claude Code how to operate in this repository. It is loaded automatically
every session.

## Project context

writing-assistant is a Windows writing tool built for its owner, not a general audience,
though the repository is public. It checks spelling, grammar, and style as the owner types,
across the browser, Microsoft Word, and native desktop applications. The owner declares a
target voice, held as a Style Card: a list of clauses that a model maintains from the owner's
expressed preferences. The tool flags text that breaks a clause, rewrites passages on request
with a local model, and underlines phrasing that reads as LLM-generated.

Full product and architecture detail is in README.md. This file only covers how Claude
operates in the repository.

## Repo layout

Current top level:

- `CLAUDE.md`: this file.
- `README.md`: product, architecture, onboarding, and stack.
- `LICENSE`: MIT. Third-party components keep their own licences, listed in README.md.
- `docs/`: committed engineering and prose documentation. Start here for conventions.
- `.github/`: the CI workflow.
- `crates/`: two of the three members of the Cargo workspace; see `docs/CONVENTIONS.md`.
  `crates/engine` is the engine library, `wa-engine`: capture, spelling, LanguageTool
  subprocess management, the telltale matcher, the analyzer, the flag types, the settings
  file, and the SQLite store's file, thread, migration runner, and insert-only guard, whose
  tables so far are `schema_version`, `passages`, the Card and ruleset tables, `candidates`,
  `screen_results`, `items`, `item_slots`, `events`, and `habit_rollups`; nothing writes a passage
  or a candidate until Phase 4, an item until Phase 5, a Card or a ruleset until Phase 3, an event
  until Phase 3's questionnaire, a habit rollup until the habit ledger in Phase 9, or a screen
  result until the screen exists (#103).
  The rewriter and the learning loop are not yet implemented.
  `crates/engine/examples/` holds the spike harnesses. `crates/wa` is the `wa` command-line
  binary, with no subcommands yet.
- `src-tauri/`: the Tauri app, the workspace's third member, kept at the path the Tauri CLI
  expects: the overlay, the commands, and the setup that starts the engine.
  `src-tauri/resources/` holds the committed dictionaries and telltale catalogue, which the
  engine also reads from there; the LanguageTool distribution and its Java runtime are
  gitignored there and built by `src-tauri/scripts/build-languagetool-jre.ps1`.
- `src/`: the React and TypeScript frontend: the overlay and the main window.
- `extension/`: the browser extension behind the web capture backend, which the app does not
  start today. Microsoft Word is captured natively (see
  `crates/engine/src/capture/native/`). Not yet decided: whether the extension stays, which
  rests on verifying Gmail and GitHub through native capture.
- `adapter/`: not yet created. A Python `uv` project with a committed lockfile: the bench,
  and the training-job scripts for submission, polling, download, and conversion.
- Root files: the Cargo workspace manifest and lockfile, the `npm` manifest and lockfile, the
  Vite entry page, and the configuration for Vite, TypeScript, ESLint, Prettier, rustfmt, and
  SonarQube.
- `_local/`: private working material: early research, draft specifications, decision logs.
  Gitignored. See the archive boundary in `docs/constitution.md`.

This section is a thin index and grows as source directories are added. Update it whenever a
new top-level folder is created.

## What to read before editing what

Every rule in the four files below comes from the project owner and is non-negotiable unless
the file says otherwise.

- `docs/constitution.md` holds the durable principles and invariants that outrank
  convenience. If something in progress conflicts with it, stop and flag it rather than
  silently working around it.
- Before writing or editing any committed document in this repository, read
  `docs/style-guide.md`. It governs prose conventions everywhere, not just files under
  `docs/`, including comment prose in code.
- Before writing or editing code in Rust, TypeScript, or Python, read `docs/CODE_STYLE.md`.
- Before touching tooling, testing, folder structure, git workflow, GitHub issues, or secrets
  handling, read `docs/CONVENTIONS.md`.
- Before creating a new top-level folder, update the "Repo layout" section above in the same
  change.

## Standing rules

- The owner is the sole intended user of this project. The repository being public does not
  make the audience general. Never simplify terminology on comprehension grounds.
- Verify a claim from a primary source or a measurement against the real target before a
  design decision rests on it. A spike is such a measurement: keep it as a harness under the
  `examples/` folder of the crate whose code it exercises, `crates/engine/examples/` today, and
  never use it as a design argument.
- No fine-tuning run may be used to resolve a design question. A training run may select
  among branches written before it ran, and never originates one.
- Report every number with its n, its interval, and the baseline it has to beat. Write a
  comparison's protocol before running it.
- Never cite, quote, or name a path under `_local/` in a committed file, a commit message, an
  issue, or a pull request.
- Never append a "co-authored with Claude Code" trailer, or any Claude/Anthropic
  attribution, to a commit, pull request, or issue.

## When you're stuck

Read the relevant doc. If it doesn't answer the question, ask rather than guessing: a wrong
guess here becomes a convention someone has to unwind later.
