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

`compute_sentence_fingerprint()` beats `hash()` with a comment. `is_suppressed_by_dismissal`
beats `flag2`.

### Validate at boundaries, trust inside

Validate inputs where they cross into this codebase: the capture layer's text and cursor
data, the LanguageTool subprocess's HTTP responses, the Anthropic API's structured output,
the SageMaker training job's output artifact. Inside a module, trust the types and don't
re-check.

Why: one layer of strict validation at the boundary protects everything downstream;
defensive checks scattered through internal code rot independently of each other and bury the
logic they're guarding.

---

## Rust

`src-tauri/`. Formatting is `rustfmt`, linting is `clippy`; both are CI gates once CI exists
(see `CONVENTIONS.md`), not suggestions.

### No unwrap in production paths

Prefer `?`. Where a panic is genuinely correct, an invariant that cannot fail, use
`.expect("why this cannot fail")`, never a bare `.unwrap()`. Tests are exempt.

Why: `.expect(...)` records the invariant at the crash site; a bare `.unwrap()` panics with no
explanation attached.

### Error handling

`thiserror` for typed errors in library-shaped modules (capture, languagetool, style, store,
rewrite); `anyhow` at the Tauri command layer. Raise the most specific variant at the deepest
layer that can name the failure. Propagate with `?`; don't match on a `Result` just to
re-wrap it.

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
| Modules, files, functions, variables | `snake_case` | `languagetool.rs`, `compute_drift_score()` |
| Types, traits, enum variants | `UpperCamelCase` | `CandidateFlag`, `LatencyTier` |
| Constants, statics | `SCREAMING_SNAKE` | `TIER_0_BUDGET_MS` |

### Logging

`log` and `env_logger`, per the tech stack. Never `println!` for diagnostics. Levels: `error`
for failures, `warn` for unexpected-but-recoverable, `info` for normal flow, `debug` for
high-frequency detail; the Tier 0 keystroke path logs at `debug` only, since it runs on every
keystroke and would flood its own log at `info`. Never log user draft text at any level:
keeping drafts local and private is the reason this tool exists.

---

## TypeScript

`src/` (frontend), `extension/`, `word-addin/`. Strict mode; no `any` without a
`// reason:` comment.

### Naming

| Kind | Convention | Example |
|---|---|---|
| Files | `kebab-case.tsx` | `style-card-editor.tsx` |
| Components | `PascalCase` | `StyleCardEditor` |
| Types, interfaces | `PascalCase` | `StyleCard`, `CandidateFlag` |
| Functions, variables, hooks | `camelCase` | `useDriftFlags` |
| Constants | `UPPER_SNAKE` | `TIER_0_BUDGET_MS` |

Why kebab-case files: it dodges the Windows/Linux case-sensitivity trap, where `Card.tsx` and
`card.tsx` are different files on one platform and the same file on the other. This matters
directly here, since the app ships on Windows and may be developed cross-platform.

### State

Zustand for state that needs to live outside a single component tree: the overlay, the tray,
settings. Local state (`useState`, `useReducer`) is the default otherwise. A Tauri command
result is not mirrored into a global store; call the command where the data is needed.

### One exported component per file

Unless tightly coupled, such as a list and its row. File path is component identity.

---

## Python

`adapter/`: pair synthesis and the QLoRA training job, run on Amazon SageMaker.

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
