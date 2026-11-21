# #616 — DefinitionContext port fidelity (PR #804)

Comment/structure fidelity inside PR #613's `DefinitionContext` port
(`crates/rigor-cli/src/effects/collect.rs`), from that PR's review. No
behavioral slice — every change is a no-op by construction or a doc
citation.

## What landed

- **`record_declaration` gate placement.** The port early-returned on
  `ctx.module_call()` being `None` before the match, which also covered
  `include`/`prepend`. Upstream (`scanner.rb:278-285`) sends
  `include`/`prepend` to `record_includes`, gated on
  `instance_side_self?` (`ancestry_recorder.rb:141`) — which admits the
  top level — and gates only `define_method` (`declare_define_method`'s
  `name.nil? || body_context.nil?`, `:290`) and the `attr_*` macros
  (`synthesize_accessors`' `return if body_context.nil?`, `:299`) on
  `module_call_body`. The port now mirrors that: the gate lives inside
  the two macro arms, and `synthesize_accessors` takes the `Option`
  answer and declines inside. A no-op today — the port has no ancestry
  recording at all — but a top-level `include` would have been dropped
  when the transitive lane lands.
- **`define_method_unit` returns upstream's triple** `(name, body,
  parameters)` (`unit_scan.rb:144`) instead of a `NestedUnit` carrying a
  placeholder `BodyCtx::UNKNOWN` both callers overwrote. `parameters`
  stays `None`: upstream's `parameter_names`/`block_parameter_name` read
  only a `ParametersNode`, and a block's are a `BlockParametersNode`.
- **Citation refresh to the pin.** The issue's four pairs
  (`definition_context.rb` `:82/:96/:112/:139` → `:71/:89/:103/:124`,
  `unit_scan.rb` `:338/:348` → `:351/:359`) plus the stale ones adjacent:
  `:130`→`:128` (`block`), `:114`→`:103` (`module_call_body`),
  `:155`→`:156`, `:159`→`:161`, `:166`→`:167`, `unit_scan.rb:91`→`:144`,
  `scanner.rb:301`→`:299`. Convention: a citation lands on the `def`
  line of the function it names.
- **Already landed / skipped.** Issue item 1 (`walk_namespace` →
  `BodyCtx::CLASS_BODY`) had already landed via PR #620's
  `declaration_prefix` reshape — verified, no change needed. The
  optional `BodyCtx::for_child(node, child)` consolidation stays
  skipped: the scanner dispatches `class <<` at node level rather than
  child level, so a single `for_child` would restructure both walks for
  no parity gain.

## Measured

- `harness/gate.sh` pass; `cargo test -p rigor-cli` 715 tests; CI green
  on `48e46357` and on the master-merge head `50015954`;
  `fp_audit --gaps --sweep` **0 FP**.
- The effects collector feeds only the `effects` subcommand, so `check`
  cannot be affected; both moved gates are pure and reach an empty arm.
- Reviews: Opus 5.5 adversarial **Approved** on `48e46357`. Grok second
  review, OpenCode companions, and the Fable design companion all
  quota-blocked — verdict on the Opus review alone (one `Approved` is
  enough per `docs/agents/review.md`).

## Follow-ups filed

- **#805** (pre-existing, found by review): `collect.rs` `declaration()`
  doc cites `scanner.rb:201` (`uniqueness_validator?`); `declaration?`
  is `:274`.
