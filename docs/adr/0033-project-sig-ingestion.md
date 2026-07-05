# Ingest project `sig/` RBS — the ADR-0007 project-signature leg

Status: accepted

[ADR-0007](0007-rbs-stdlib-shipping.md) defines the analysis-time type
environment as the merge of **embedded stdlib RBS ⊕ project `sig/` ⊕ gem RBS ⊕
inline RBS**, with precedence matching the reference's `signature_paths` model.
Only the embedded leg (vendored core + stdlib + bundled-plugin RBS) is wired
today — [`CoreData::load_with_plugins`](../../crates/rigor-index/src/rbs.rs)
ingests those and nothing else. This ADR concretizes and commits the **project
`sig/`** leg. Gem RBS, `rbs_collection`, and inline RBS remain unimplemented and
are explicitly out of scope here.

## The decision

- Add a `signature_paths:` config key (default `["sig"]`), resolved relative to
  the project root. This mirrors the reference's config surface.
- After the core + plugin load, feed each resolved signature directory through
  the **existing** [`ingest_rbs_dir`](../../crates/rigor-index/src/rbs.rs) into
  the same `Builder`. No new parser, no new format, no filesystem-cache tier —
  the project's `.rbs` are parsed by the same native `ruby-rbs` parser and folded
  through the same reopen-union `Builder::merge` as embedded RBS.
- **Precedence is union**, the same semantics the embedded merge already applies.
  RBS class reopens are additive (members union); a genuine conflicting duplicate
  definition is invalid RBS the reference itself rejects, so no project-overrides-
  core overlay is invented (see *Considered options*).
- **Scope is project `sig/` only.** Gem RBS / `rbs_collection` (bundler auto-
  detection), inline RBS, and `target_ruby`-driven stdlib selection are separate,
  larger legs deferred to their own slices.

## Why this is consistent with the Ruby-free policy

[ADR-0001](0001-rust-reimplementation-strategy.md) / ADR-0007's single-binary,
Ruby-free distribution forbids depending on a **`ruby`/`rbs`-gem runtime** — it
does not forbid reading `.rbs` text. `.rbs` files are plain text already parsed
natively by `ruby-rbs` 0.3 (the same parser that ingests core/stdlib/plugin RBS).
Pointing that parser at the project's own signature dirs adds **zero** runtime
dependency. This leg was always part of ADR-0007's design; it is unimplemented,
not blocked.

## Central consequence — a coverage lever, gated on parity

The dispatch rules gate witnessing on `index.knows_class(class_name)`
([`crates/rigor-rules/src/lib.rs`](../../crates/rigor-rules/src/lib.rs), the
`call.undefined-method` guard `if !index.knows_class(class_name)`), the rigor-rs
analogue of the reference's `rbs_class_known?`. Today that known-class set is
core + stdlib + plugin; the reference's set **also** includes classes declared in
the project's `sig/`. So ingesting project sig widens `knows_class` to match the
reference — which is primarily a **coverage lever**: it closes missed
`call.undefined-method` (and sibling arity / argument-type / singleton) witnesses
on receivers whose class the reference knows only through project sig, and which
rigor-rs currently treats leniently as in-source-only (the
[undefined-method leniency](../../CONTEXT.md) boundary shifts *toward* the
reference, not past it).

## Robustness — the v0.2.7 hazard stays absent

The 2026-07-05 follow-up (recorded in [`docs/CURRENT_WORK.md`](../CURRENT_WORK.md))
established that rigor-rs is structurally immune to the reference's v0.2.7
env-collapse bug (a malformed project `.rbs` nulling the whole RBS environment via
`DuplicatedDeclarationError`): there is no `resolve_type_names`-style global
validation pass, `Builder::merge` unions by name key with no class/module *kind*
concept and raises nothing, and per-file parse failures are isolated
([ADR-0016](0016-never-crash-isolation.md)). Ingesting **user-authored** RBS
therefore degrades soundly — an unparsed construct drops that one declaration (a
coverage gap, never a crash), and a malformed file loses only its own bad decls
while its well-formed siblings still load.

## Residual risk and the landing gate

The remaining exposure is a **wrong-but-parseable** user sig feeding a false
positive into an arity / argument-type rule. The reference has the identical
exposure (it resolves dispatch against the same project sig), so parity means
**matching** the reference, not being safer than it. The landing gate is the
differential harness ([`UPSTREAM.md`](../../UPSTREAM.md)) run against a corpus
project that ships a `sig/` directory: measure the coverage delta and confirm
zero *unregistered* false positives before committing. Genuine reference
divergences are registered via [ADR-0011](0011-reference-oracle-exceptions.md).

## Considered options

- **Shell out to `rbs` / Ruby to load project sig** — rejected: breaks the
  Ruby-free distribution (ADR-0001) for no gain; the native `ruby-rbs` parser
  already handles `.rbs`.
- **Leave project sig unloaded (status quo)** — rejected: a standing parity gap
  (missed dispatch witnesses on sig-declared classes) and a direct contradiction
  of ADR-0007's stated type-environment definition.
- **Land all four legs at once (project + gem + `rbs_collection` + inline)** —
  rejected: gem RBS and `rbs_collection` require bundler / lockfile auto-detection
  — a materially bigger, separable slice. Project `sig/` is the minimal
  self-contained first step and the highest-leverage one (it is where a project's
  own hand-written types live).
- **Invent a project-overrides-core precedence overlay** — rejected as premature:
  RBS reopen semantics are additive-union, and a true conflicting duplicate
  definition is invalid RBS. Union already matches the reference for the additive
  case; revisit only if a measured corpus divergence in duplicate-definition
  ordering demands it.

## Revisiting

Supersede or extend when the gem-RBS / `rbs_collection` / inline legs land, or if
the harness surfaces a duplicate-definition ordering divergence that union
precedence cannot honour.
