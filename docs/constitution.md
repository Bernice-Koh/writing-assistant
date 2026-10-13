# Constitution

Durable principles for writing-assistant. These outrank convenience: a task in progress that
conflicts with one of them stops and is flagged. It is never silently worked around.

## Principles

### Audience

The owner is the sole intended user of this project. The repository being public does not
make the audience general. Terminology is never simplified on comprehension grounds: the
project is written and built for the owner's actual level of understanding, not a hypothetical
newcomer's.

### Verify before building

No design decision rests on an unverified claim. A claim is verified by a primary source or by
a measurement against the real target.

A spike is a measurement against a real target. It is kept in the repository as a harness that
runs again on demand, and it is never used as a design argument.

### Design precedes training

No fine-tuning run resolves a design question. A training run may select among branches
written before it ran. It never originates one: a branch that was not written down before the
job started is not adopted on the strength of the job's result.

### Every number carries its interval and its baseline

No comparison is argued from a point estimate. A rate is reported as a count over its n, with
an interval. A score is reported beside the baseline it has to beat. A comparison's protocol,
meaning its metric, margin, test, and decision rule, is written before the comparison runs.

### The archive boundary

`_local/` is private working material: early research, draft specifications, decision logs.
It is never cited from a committed file, its prose is never copied into the repository, and
its internal paths are never referenced in a committed document, a commit message, an issue,
or a pull request. Content leaves `_local/` only by being rewritten to this repository's
conventions in a committed file.

## Invariants

Three invariants bind every engineering decision. The first sentence of each is the binding
statement.

1. Nothing that runs on the owner's typing calls a model or the network. The path from a text
   change to a flag is deterministic code plus LanguageTool, a subprocess on the same machine
   reached over the loopback interface. Model calls and network calls belong to work the owner
   asks for, such as a rewrite, and to background jobs that typing never waits on.

2. Content preservation is checked deterministically on every candidate before it is shown,
   and a model is never the source of a fact. A candidate is a model's rewrite of a passage.
   The check compares its numbers, capitalised tokens, negation words, and length against the
   passage, and a candidate that fails is not shown. A model supplies wording; the facts are
   the passage's.

3. The Style Card is the only target, and it changes only in response to a preference the
   owner expresses. An expressed preference is a questionnaire answer, a reaction to a sample,
   or free-prose feedback. Measured behaviour is a pick between candidates, an edit of a
   candidate, or a dismissal. It is recorded and never moves the Card. A model writes the
   Card's clauses, so what separates target from behaviour is the kind of input, an expressed
   preference or an observed action, and not who wrote the clause.
