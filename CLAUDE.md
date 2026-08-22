# Working with Claude on writing-assistant

This file tells Claude Code how to operate in this repository. It is loaded automatically
every session.

## Project context

writing-assistant is a Windows writing tool built for its owner, not a general audience,
though the repository is public. It checks spelling, grammar, and style as text is typed,
across the browser, Microsoft Word, and native desktop applications. The user declares a
target voice during onboarding; the tool flags drift from that voice, rewrites passages into
it, and underlines the phrasing habits that mark writing as LLM-generated.

Full product and architecture detail is in README.md. This file only covers how Claude
operates in the repository.

## Repo layout

Current top level, matching README's Repository structure:

- `CLAUDE.md` - this file.
- `README.md` - product, architecture, onboarding flow, and stack.
- `LICENSE` - MIT. Third-party components keep their own licences; see README's License
  section.
- `docs/` - committed engineering and prose documentation. Start here for conventions.
- `src-tauri/` - the Rust core: capture, spelling, LanguageTool subprocess management, the
  style engine, the local store, the rewrite orchestrator, the analyzer and learning
  scheduler. Bundled resources (the LanguageTool JAR, the vendored dictionaries and
  AI-telltale catalog) live under `src-tauri/resources/`.
- `src/` - the React and TypeScript frontend: overlay, Style Card editor, onboarding,
  settings, tray.
- `extension/` - the browser extension, the web capture backend. There is no separate Word
  backend: Microsoft Word's desktop document surface is captured natively (see
  `src-tauri/src/capture/native/`). Word for the web is a different, unsolved problem; see
  README's Components section.
- `adapter/` - pair synthesis and the QLoRA training job, Python, run on Amazon SageMaker.
- `_local/` - private working material: early research, draft specifications, decision logs.
  Gitignored. See the archive boundary in `docs/constitution.md`.

This section is a thin index and grows as source directories are added. Update it whenever a
new top-level folder is created.

## What to read before editing what

Every rule in the four files below comes from the project owner and is non-negotiable unless
the file says otherwise.

- `docs/constitution.md` holds the durable principles that outrank convenience. If something
  in progress conflicts with it, stop and flag it rather than silently working around it.
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
- Verify feasibility from existing repositories, documentation, and shipped products. Do not
  build throwaway prototypes or mocks to find out whether something works.
- No fine-tuning run may be used to resolve a design question. Design decisions are settled
  before training starts, not discovered by training.
- Never append a "co-authored with Claude Code" trailer, or any Claude/Anthropic
  attribution, to a commit, pull request, or issue.

## When you're stuck

Read the relevant doc. If it doesn't answer the question, ask rather than guessing - a wrong
guess here becomes a convention someone has to unwind later.
