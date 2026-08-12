# Constitution

Durable principles for writing-assistant. These outrank convenience: if a task in progress
conflicts with one of them, stop and flag it rather than silently working around it.

## Audience

The owner is the sole intended user of this project. The repository being public does not
make the audience general. Terminology is never simplified on comprehension grounds; write
and build for the owner's actual level of understanding, not a hypothetical newcomer.

## Verify before building

Feasibility is verified from existing repositories, documentation, and shipped products
before code is written. Throwaway prototypes or mocks built solely to find out whether
something works are not part of this project's method.

## Design precedes training

No fine-tuning run resolves a design question. Design decisions for the voice adapter are
settled before a training job starts, not discovered by running one.

## The archive boundary

`_local/` is private working material: early research, draft specifications, decision logs.
It is never cited from a committed file, its prose is never copied into the repository, and
its internal paths are never referenced in a committed document. Content leaves `_local/`
only by being rewritten to this repository's conventions in a committed file.

## Architecture invariants

Three invariants from the design shape every engineering decision. Full context is in
README's Architecture section; this is the binding statement of each.

- Live checking is arithmetic and rules only. A network round trip costs 500 ms or more,
  which the Tier 0 and Tier 0.5 latency budgets do not have.
- Adapters carry voice; facts come from the draft and retrieved context. Adapters encode
  style faithfully and invent details confidently, so a rewrite's factual content is never
  left to the adapter alone.
- Only deliberate acts move the target profile. Measured behaviour updates the observed
  profile only, so habit never drags the target back toward how the user already writes.
