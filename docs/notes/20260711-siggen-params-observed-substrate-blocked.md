# sig-gen `--params=observed` — SUBSTRATE-BLOCKED (measured, deferred 2026-07-11)

Investigated as the next sig-gen slice after `--overwrite` (slice 11) via the
full protocol: value probe on the reference itself + two independent Sonnet
investigations (reference `ObservationCollector` behavior; rigor-rs typing
substrate) + a main-session literal-vs-nominal measurement. **Conclusion: a
faithful, byte-safe port is BLOCKED on the ScopeIndexer substrate rigor-rs does
not have — the same per-scope-typing substrate the possible-nil / flow frontier
is blocked on (ADR-0022).** Do NOT build a literal-only partial port: it would
convert an honest "unimplemented (exit 2)" into a partial implementation that
byte-MISMATCHES the reference on the always-emitted `initialize` line.

## What the mode does

`--params=observed --observe=spec` walks the spec tree, and for each call
`Receiver.method(args)` whose receiver resolves to a class, records the TYPES of
the argument expressions, keyed `(class, method)` (with `.new` routed to
`:initialize`). The generator renders those observed types as the param list
instead of `untyped` (`("hi" | 42)`, `(name: ("rails" | "x"), ?locked: true)`).
Value concentrates almost entirely on `initialize` (an always-run stub path that
supports every param shape); ordinary methods are pre-gated to
required-positional-only by `simple_parameter_shape?` before observations are
consulted.

## Value probe (reference on itself)

`sig-gen --print --params=observed --observe=spec lib` vs `--params=untyped` over
`reference/rigor/lib`: **110 changed def-lines** carry an observed param payload.
Main-session literal-vs-nominal split of those 110:
- ~53 "pure literal" (`(kind: (:instance | :singleton))`, `(name: "rails", …)`) —
  the ONLY subset rigor-rs could type identically, and nearly all involve
  KEYWORD args.
- ~57 "nominal-bearing" (`(Proc)`, `(wall_seconds: Float, …)`,
  `[Rigor::Cache::Descriptor::FileEntry]`) — derived from scope-bound locals /
  method-call results that rigor-rs types `Dynamic` ⇒ it would skip these.

## The two decisive findings

### 1. The reference types args with the FULL ScopeIndexer, not literal matching

`ObservationCollector#collect_args` calls `scope.type_of(node)` from the same
`ScopeIndexer.index` engine the whole analyzer uses. It resolves local variables
(`x = "hi"; Foo.new(x)` → `"hi"`), self-method-call returns (ADR-57), and
block-yielded element types. **Real specs overwhelmingly use `let` / local /
helper-returned values rather than inline literals inside `.new(...)`.** A port
that only matches literal AST nodes silently degrades the MAJORITY of real
observed output.

### 2. rigor-rs has no ScopeIndexer; literal-only is NOT a safe under-emit

rigor-rs types literal args scope-independently (`Typer::type_of` dispatches on
the node variant — `crates/rigor-infer/src/lib.rs:130-313`), and `X.new`
receivers resolve name-based (`type_dot_new`, `lib.rs:457-479`), so a literal arg
types correctly however deeply nested. BUT `build_toplevel_env` never descends
into blocks (`lib.rs:769-780`), so a block-local variable READ types `Dynamic`.

The parity killer: an observed union with ANY `Dynamic`/unresolved member
collapses the WHOLE union to bare `untyped` (top-absorption, reference-confirmed).
Both tools ALWAYS emit the `initialize` stub line for a non-trivial constructor.
So for any class whose observe tree has even ONE scope-dependent caller (the
common case):
- reference emits `def initialize: ("hi" | String) -> void` (scope-typed union),
- rigor-rs (literal-only) can only emit the `(untyped) -> void` stub.

Same method, both emit it, **different bytes ⇒ a hard-guarantee break on a
shared method**, not an acceptable coverage gap. Detecting the non-literal caller
and bailing does not help: bailing still emits the untyped stub, which still
mismatches. The mismatch is intrinsic wherever the reference has more typing
power than rigor-rs on a method both always emit.

## Additional substrate gaps (would ALSO need fixing even for literal-only)

- **No keyword-hash discriminator in the arena.** `Node::Call.args` is a flat
  `Vec<NodeId>` with no positional/splat/keyword tag (`ast.rs:186-193`, TODO at
  `ast.rs:657`); a bare keyword-hash arg and a positional Hash literal both lower
  to `HashLit{all_assoc:false}`. Since ~all the matchable value is keyword args,
  capturing it needs a lowering change (add `is_keyword_hash`).
- **Per-file `SourceIndex::build`** in sig-gen (`sig_gen.rs:277`) would need to
  become project-wide `build_project(source + observe asts)` so a spec's
  `Foo.new` resolves `Foo` defined under `lib/`.

## Decision

DEFER as substrate-blocked. The only byte-safe path is porting the ScopeIndexer
(the analyzer's per-scope typing engine — large, and the same substrate ADR-0022
/ the flow frontier need). A literal-only partial port is REJECTED: it is a net
regression (introduces shared-method mismatches on `initialize`) for a minority,
fragile coverage slice. Keep `--params=observed` at its current honest exit-2
"not yet implemented".

This is the sig-gen counterpart of the `type-of --trace` / `coverage` /
`type-scan` substrate-blocked findings already recorded in CURRENT_WORK: a big
track whose faithful port needs substrate rigor-rs deliberately hasn't built.

## The remaining sig-gen frontier is now thin

After 11 merged slices, the honestly-portable sig-gen surface is essentially
exhausted. The one known SHARED-METHOD byte mismatch left in the
`reference/rigor/lib` sweep is **qualified source-class naming**:
`Selector = Data.define(...)` then `Selector.new(...)` returns `Selector` in
rigor-rs vs the fully-qualified `Rigor::Triage::Selector` in the reference
(`lib/rigor/triage.rb:28,139`). That is a real, bounded, byte-safe-to-close fix
(the sig-gen parity model wants shared-method byte-identity) — the next slice if
the sig-gen arc continues, distinct from this substrate-blocked track.
