# Code Style

Language-level rules for writing code in this repository. Workflow, folder structure,
testing strategy, and git rules live in `CONVENTIONS.md`; comment and documentation prose
conventions live in `style-guide.md`. This file is about code structure and mechanics.

Each rule carries a short why. Knowing why turns the edge cases the rule doesn't cover into a
judgement call instead of a guess.

## Universal principles

### Comments are for why, not what

Code says what. If a comment restates what the code does, delete it or rename until the
comment is unnecessary. Comment tone and address follow `style-guide.md`: never directed at a
reader, never a teaching aside.

Why: a "what" comment rots as the code changes under it; a "why" comment survives, because
the reason for a decision usually outlives the decision itself.

`// HACK:` and `// TODO:` are fine, with a date and one line of context.

### Names carry the explanation

Prefer a longer, clearer name over a short name plus a comment.

Why: a name appears at every call site; a comment appears once.

`compute_sentence_fingerprint()` beats `hash()` with a comment. `is_suppressed_by_hard_flag`
beats `flag2`.

### Validate at boundaries, trust inside

Validate inputs where they cross into this codebase: the capture layer's text and
rectangles, LanguageTool's HTTP responses, llama-server's responses, the Anthropic API's
structured output, the adapter a training job returns, and every row that enters the store
through `wa ingest`. Inside a module, trust the types and don't re-check.

Why: one layer of strict validation at the boundary protects everything downstream;
defensive checks scattered through internal code rot independently of each other and bury the
logic they're guarding.

---

## Rust

`crates/` and `src-tauri/`. Formatting is `rustfmt`, linting is `clippy` with warnings denied; both are CI
gates (see `CONVENTIONS.md`), not suggestions.

### No unwrap in production paths

Prefer `?`. Where a panic is genuinely correct, an invariant that cannot fail, use
`.expect("why this cannot fail")`, never a bare `.unwrap()`. Tests are exempt.

Why: `.expect(...)` records the invariant at the crash site; a bare `.unwrap()` panics with no
explanation attached.

### Error handling

`thiserror` for typed errors in the engine's modules; `anyhow` at the Tauri command layer.
Raise the most specific variant at the deepest layer that can name the failure. Propagate
with `?`; don't match on a `Result` just to re-wrap it.

### Tauri commands

A `#[tauri::command]` that can fail returns `Result<T, E>` where `E: Serialize`, mapped to a
tagged shape the frontend can switch on rather than a bare string:

```rust
#[derive(serde::Serialize)]
#[serde(tag = "kind", content = "message")]
enum CommandError { Io(String), LanguageTool(String), NotFound(String) }
```

### Naming

| Kind | Convention | Example |
|---|---|---|
| Modules, files, functions, variables | `snake_case` | `ai_tell.rs`, `resolve_within_sentence()` |
| Types, traits, enum variants | `UpperCamelCase` | `PositionedFlag`, `FlagOrigin` |
| Constants, statics | `SCREAMING_SNAKE` | `QUIET_THRESHOLD` |

### Logging

`log` and `env_logger`. Never `println!` for diagnostics. Levels: `error` for failures, `warn`
for unexpected-but-recoverable, `info` for normal flow, `debug` for high-frequency detail. A
line that fires on every text-changed event or every poll logs at `debug` only, since at
`info` it floods the log.

Never log draft text at any level, whole or in part. A log is plain text that nothing governs:
it is redirected to files and pasted into issues on a public repository, and it outlives the
draft it quotes. A log line names a flag id, an offset, a length, or a message kind instead.

---

## TypeScript

`src/`. Strict mode; no `any` without a `// reason:` comment. `extension/` is plain
JavaScript under the same naming rules.

### Naming

| Kind | Convention | Example |
|---|---|---|
| Files | `kebab-case.tsx` | `working-card.tsx` |
| Components | `PascalCase` | `WorkingCard` |
| Types, interfaces | `PascalCase` | `PositionedFlag`, `FlagOrigin` |
| Functions, variables, hooks | `camelCase` | `useHoveredFlag` |
| Constants | `UPPER_SNAKE` | `UNDERLINE_THICKNESS` |

Why kebab-case files: it dodges the Windows/Linux case-sensitivity trap, where `Card.tsx` and
`card.tsx` are different files on one platform and the same file on the other. This matters
directly here, since the app runs on Windows and CI checks the frontend on Linux.

### State

Local state (`useState`, `useReducer`) is the default. Zustand is for state that needs to
live outside a single component tree. A Tauri command result is not mirrored into a global
store; call the command where the data is needed.

### One exported component per file

Unless tightly coupled, such as a list and its row. File path is component identity.

---

## Python

`adapter/`, a `uv` project: the bench, and the training-job scripts for submission, polling,
download, and conversion. It opens the store read-only and writes through `wa ingest`.

Type hints on every function signature. `snake_case` for functions, modules, and variables;
`PascalCase` for classes; `UPPER_SNAKE` for constants. A short docstring on every public
function; private helpers (leading `_`) can skip one if the name and signature are obvious.

---

## Cross-language

### File length

No fixed ceiling (see `style-guide.md`), but past roughly 400 lines, ask whether the file has
taken on a second purpose. It almost always has.

### Dead code

Delete it. Git remembers; the codebase shouldn't carry corpses.
