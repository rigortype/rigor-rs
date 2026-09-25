# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

This repo is **single-context**: one `CONTEXT.md` (the glossary) + `docs/adr/` at the repo root.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root.
- **`docs/adr/`**: the ADRs that touch the area you're about to work in.

The `/domain-modeling` skill (reached via `/grill-with-docs` and `/improve-codebase-architecture`) updates them when terms or decisions get resolved.

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0007 (event-sourced orders) — but worth reopening because…_
