# Conventions

Engineering conventions for writing-assistant: how the project is organised and operated,
not how code reads (`CODE_STYLE.md`) or how prose reads (`style-guide.md`).

Project shape: a Tauri v2 desktop app, Rust core in `src-tauri/`, React and TypeScript
frontend at the repo root under `src/`, a browser extension in `extension/`, an Office
add-in in `word-addin/`, and a Python adapter-training pipeline in `adapter/` that runs on
Amazon SageMaker. Solo developer, public repo, MIT licensed, single intended user.

## Folder structure

See README's Repository structure section for the full tree and CLAUDE.md's Repo layout for
the top-level index. Create a folder when there is code to put in it, not before; update
CLAUDE.md's Repo layout in the same change that adds a new top-level folder.

## Dependencies and tooling

- Rust: `cargo`, `Cargo.lock` committed; this is a binary, not a library.
- Frontend (`src/`, `extension/`, `word-addin/`): `npm`, lockfile committed.
- Python (`adapter/`): package manager not yet pinned. Decide before the training pipeline is
  implemented; `.gitignore` already expects a `.venv/` or `venv/` directory either way.
- Rust lint/format: `rustfmt` and `clippy`.
- Frontend lint/format: ESLint (flat config) for logic, Prettier for formatting. Never
  ESLint rules for formatting; `eslint-config-prettier` last in the chain.
- Frontend types: `tsc --noEmit`.
- CI: not yet configured. `.gitattributes` already assumes a Linux CI runner exists
  (`text eol=lf` for tooling "consumed by Linux tooling (SageMaker training jobs, CI)");
  set it up before the adapter pipeline ships.

## Testing

- Rust: `cargo test`. In-module `#[cfg(test)] mod tests` for unit tests; `src-tauri/tests/`
  for integration tests against the public API only.
- Frontend: not yet pinned. Vitest fits the existing Vite build and is the default choice;
  pin it when the first test file is written.
- The deterministic stages (style feature extraction, the hard critic's count-based rule
  checks, span verification against the source text) get thorough unit tests: they carry the
  correctness guarantees and are cheap to test.
- Mock the Anthropic API and the LanguageTool subprocess in tests. Tests never hit the live
  API or a live subprocess.

## Logging

`log` and `env_logger` in Rust; see `CODE_STYLE.md` for levels and the no-draft-text rule.
Frontend logging conventions are not yet pinned.

## Error handling

Typed errors with `thiserror` at the module level in `src-tauri/`; `anyhow` at the Tauri
command layer. A capture backend or the LanguageTool subprocess failing degrades that one
source rather than crashing the app; the Tier 0 pipeline runs on spelling and style flags
alone if LanguageTool is down. Never swallow an error silently; if it's genuinely ignorable,
log it at `debug` and say why in the log line, not in a comment.

## Git

- Conventional Commits: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, `ci:`.
- One-line commit subject, imperative mood, no body paragraph unless the why genuinely needs
  one. Subject under 72 characters.
- One concern per commit. Stage explicit paths; never `git add .` or `git add -A`.
- Never commit `.env` or anything that looks like a secret. `.env.example` (names only) is
  committed; see `.gitignore`'s Secrets section.
- Never append a "co-authored with Claude Code" trailer, or any Claude/Anthropic
  attribution, to a commit, pull request, or issue.
- Don't skip hooks (`--no-verify`) once hooks exist, unless explicitly asked.

## Branches

Optional. Direct commits to `main` are fine for solo work; branch when a change is large
enough to want to iterate before it lands, using `feat/<short>`, `fix/<short>`,
`docs/<short>`, `chore/<short>`.

## GitHub issues

- Title: short, direct, states the change or problem plainly.
- Acceptance criteria are written in Gherkin (`Given / When / Then`), one scenario per
  distinct behaviour. This is the spec; there is no separate spec document.
- No fixed template beyond that; write what the issue needs, no longer.

## Secrets

- Never commit secrets, even temporarily. `.env` is gitignored; `.env.example` (names only)
  is committed.
- The Anthropic API key and any AWS credentials for the SageMaker training job exist only in
  `.env` locally and in whatever secret store CI or deployment eventually uses, never in
  code, issues, comments, or logs.
- Style Card, profiles, exemplar corpus, and training pairs are personal data (see
  `.gitignore`'s User data section) and never enter the repository. There is no synthetic
  demo dataset to substitute; this is a single-user tool with no shared data to mock.
