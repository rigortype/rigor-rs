# MultiWrite substrate — Slice 1 results (2026-07-25)

Implements Slice 1 of [the spec](20260725-multiwrite-substrate-spec.md): a
`Node::MultiWrite` arena lowering, flow-write recording for every bound target,
a faithful `MultiTargetBinder` port, and the retirement of the coverage.rs
multi-write taint workaround. Slice 2 (RBS tuple returns / `Process::Status` /
fixture 68) is untouched.

## The arena shape

`crates/rigor-parse/src/ast.rs`:

```rust
Node::MultiWrite { targets: MultiTargets, value: NodeId, target_exprs: Vec<NodeId>, span }

struct MultiTargets { lefts: Vec<MultiTarget>, rest: Option<Box<MultiTarget>>, rights: Vec<MultiTarget>, span }
enum   MultiTarget  { Local { name, name_span }, Nested(MultiTargets), Ignored { span } }
```

- `MultiTargets` is the reference's `lefts` / `rest` / `rights` triple verbatim
  — Prism's `MultiWriteNode` and the nested `MultiTargetNode` share it, which is
  why the reference's binder treats them uniformly
  (`multi_target_binder.rb:20-22`). A composite child-group struct in the style
  of `RescueClause`.
- `MultiTarget::Local` carries `name_span` (the `LocalVariableTargetNode`
  location IS the name token), matching the enum's name-anchored convention.
- **`Ignored` is materialised, not dropped.** An ivar / constant / index / call
  / const-path target, an anonymous `*`, and an `ImplicitRestNode` all bind
  nothing, but the tuple decomposition is POSITIONAL — dropping a slot would
  shift every later target onto the wrong element.
- `rest` is `Option<Box<MultiTarget>>` (Box only because the enum is recursive).
  It is `Some` whenever Prism reports ANY rest node, including an anonymous `*`
  and an implicit rest, because the reference keys on `rest_present:`
  (presence), not on bindability.
- **`target_exprs`** — added after the corpus sweep caught a regression; see
  "the one thing the spec did not cover" below.

Both exhaustive matches gained the variant: `Node::span()` (`ast.rs`) and
`node_kind()` (`crates/rigor-cli/src/type_of.rs`, label `"MultiWrite"` — the
rigor-rs-native variant name, per that function's stated convention).

`collect_recoverable_children` gained a `visit_multi_write_node` override so a
multi-write buried under an unhandled wrapper is recovered WHOLE (it re-lowers
its own RHS) instead of having its LHS names dropped again. It was NOT given a
`LocalVariableTargetNode` override: multi-writes no longer need it, and the two
remaining target-dropping forms (`for` index, `rescue =>`) are explicitly a
separate slice.

## The binder port

`crates/rigor-infer/src/multi_target_binder.rs`, one function per reference
rule (the module header carries the mapping table):

| reference (`multi_target_binder.rb`) | here |
| --- | --- |
| `bind` / `visit` | `bind` / `visit` |
| `decompose` (tuple vs everything else) | `decompose` |
| `decompose_tuple` (front / rest / back split) | `decompose_tuple` |
| `decompose_default` (non-tuple RHS ⇒ every slot `Dynamic[top]`) | `decompose_default` |
| `slot_type` (MISSING slot ⇒ `Constant[nil]`) | `slot_type` |
| **`soften_optional_slot`** | **`soften_optional_slot`** |
| `nil_literal?` | `is_nil_literal` |
| `bind_target` (local binds / nested recurses / else skip) | `bind_target` |
| `bind_rest_target` (ONLY a local under the splat binds) | `bind_rest_target` |

Notes on the non-obvious ones:

- `decompose_tuple`'s `middle_end` reproduces Ruby's
  `[elements.size - back_count, front_count].max` in SIGNED arithmetic — the
  Ruby expression goes negative when the tuple is shorter than the trailing
  target count, and a `usize` subtraction would panic.
- **`soften_optional_slot` is ported exactly, including the two carve-outs.** A
  `Union` slot containing `Constant[nil]` drops the nil and re-joins the rest; a
  BARE nil slot stays nil; a non-`Union` slot (notably a `Dynamic[…]` wrapper) is
  untouched, because the reference's check is `is_a?(Type::Union)` and nothing
  wider. This is upstream's ADR-57 slice-3 FP-discipline decision (haml
  `parse_tag`'s 9-tuple, where the nil-ness is guarded by a CORRELATED
  cross-slot invariant per-slot flow cannot see), NOT an optimization — the
  rationale is reproduced in the doc comment so a later reader does not "clean
  it up".
- The binder returns an ORDERED `Vec<(String, TypeId)>` rather than a map. The
  reference returns a `Hash` built in the same order; applying the Vec in order
  gives identical last-write-wins semantics for a duplicated target name
  (`a, a = xs`), and keeps the binder allocation-cheap.
- The reference's SECOND surface (`BlockParameterBinder`, `|(a, b), c|`) is not
  wired: rigor-rs does not lower block parameters into the arena, so there is no
  `MultiTargets` to bind. Recorded in the module header.

Statement value: `Node::MultiWrite` types to its RHS in BOTH `type_of` (the
reference routes `Prism::MultiWriteNode` to `type_of_assignment_write`,
`expression_typer.rb:125`) and `stmt_value_type` — so `(a, b = [1, 2])` is
`[1, 2]`.

## Consumers wired

All five named in the spec, in `crates/rigor-infer/src/lib.rs`:

- `collect_flow_writes` — every bound target name, keyed by the WHOLE
  multi-write span (the same whole-statement key a single-target write uses).
  **This is the arm that closes the FP.**
- `build_method_body_env` — a `BodyWrite::Multi` case binding through the binder
  once the RHS type is known.
- `flow_eval` — widens the RHS's own writes first (same discipline as the
  single-target arm), then rebinds every target.
- `bind_statement` — same, without the widening (that pass has none).
- `nil_flow_scope` — records RHS uses, then rebinds each target AND drops the
  per-name nil / `Array.new`-provenance facts. Dropping is the FP-safe
  direction (a dropped `C | nil` fact can only SILENCE
  `call.possible-nil-receiver`), and it agrees with `soften_optional_slot`: a
  destructured slot never carries a manufactured nil.

## The FP fix — evidence

The spec's probe, verified with the hardened oracle invocation (pinned
`rigor-rbs-inline` plugin path, fresh cwd, `--no-cache`):

```ruby
def probe(other)
  x = 5
  x, _y = other, 2
  if x
    puts "truthy"
  end
end
```

| | before | after |
| --- | --- | --- |
| rigor-rs | `probe.rb:4:6: warning: condition is always truthy` | *silent* |
| reference | *silent* | *silent* |

Regression tests: `always_truthy_multi_write_rebind_widens_silent`
(`crates/rigor-rules/src/lib.rs`) pins the probe plus the nested-target and
splat-target variants.

## The one thing the spec did not cover — reads inside non-local targets

The corpus sweep caught a NEW false positive the spec's design would have
shipped. `netrc` 0.11.0 `Netrc#[]=`:

```ruby
if item = @data.detect { |datum| datum[1] == k }
  item[3], item[5] = info
```

The old lossy lowering ran the whole `MultiWriteNode` through
`collect_recoverable_children`, which recovered the `item` READS out of the
`IndexTargetNode` receivers. The new structural target lowering binds no local
for an index target and — as first written — dropped those reads, so
`flow.dead-assignment` saw `item` as never read and fired
`local 'item' assigned in '[]=' but never read`. The reference is silent (its
`gather_read_names` walks the real Prism subtree).

Fix: `Node::MultiWrite::target_exprs` holds the lowered recoverable descendants
of every `Ignored` target (at any nesting depth), so the span-scanning
structural walks find them exactly as before. Pinned by
`dead_assignment_reads_inside_multi_write_targets_count` (rules) and
`lowers_expressions_embedded_in_ignorable_multi_targets` (parse).

This is why the corpus sweep is mandatory for an FP-safety change: the harness,
the unit tests and the three "mandated" corpora were all green on the broken
version.

## Parity guards verified

- **`flow.dead-assignment` skips multi-writes.** The collector gathers write
  CANDIDATES as `Node::LocalVariableWrite` only, so `Node::MultiWrite` is
  structurally invisible to it. `dead_assignment_multi_write_is_silent`
  extended to cover the splat / nested / ignorable forms; harness fixture
  `22_dead_assignment_skips.rb:47-51` still silent.
- **`static.value-use.void` excludes multi-writes.** `void_value_use_diagnostics`
  matches `LocalVariableWrite | InstanceVariableWrite | ConstantWrite` only —
  the reference's `WRITE_NODE_CLASSES` deliberately omits `MultiWriteNode`. New
  test `void_multi_write_rhs_stays_silent`.
- `check`'s composition order is untouched.

## coverage.rs

The `MultiWriteNode` arm of `collect_prism_taints` is deleted; the `for`-index,
`rescue =>`, index-write and block-parameter arms are untouched.
`collect_flow_writes` now supplies exactly the same name set (keyed by the whole
multi-write span rather than the target span — an equivalent taint key, since
neither is ever a `straight_line` binding span and both sit inside precisely the
same enclosing scopes).

Coverage did not regress; it IMPROVED toward the oracle, because
`Node::MultiWrite` now types to its RHS `Tuple` instead of vanishing into an
excluded `Statements` carrier:

| corpus | tier moved | old → new | oracle |
| --- | --- | --- | --- |
| mastodon `app/models` | `dynamic_top` → `shaped` | 18009→18004 / 790→795 | 15962 / 1389 |
| haml `lib` | `dynamic_top` → `shaped` | 5200→5194 / 329→335 | 4113 / 484 |
| conference-app | — (no multi-writes in scope) | unchanged | — |

Total node counts are unchanged; every moved node moved from an UNDER-claim
toward the oracle, none in the over-claiming direction. The node-level test
`multi_write_if_invalidates_the_binding` is now oracle-EXACT (constant 3 /
shaped 3 / dynamic_top 5 — previously constant 3 / dynamic_top 8, with the
three `shaped` nodes flagged in its own comment as an under-claim). Verified by
running the reference's `coverage --format json` on the same source.

## Gates

| gate | result |
| --- | --- |
| `cargo build --offline && cargo test --offline` | PASS (all suites green) |
| `ruby harness/run.rb` | PASS — 70 fixtures, 216/218, **0 unregistered FP** |
| `ruby harness/run_snapshot.rb` | PASS — identical counts |
| `python3 harness/docs_check.py` | PASS (4 budgets, links resolve) |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | clean |
| probe | rigor-rs silent, reference silent |

Harness counts are UNCHANGED (218 / 217 / 216 matched / 1 gap = fixture 68,
Slice 2's target). No fixture expectation moved.

### `fp_audit --gaps` — 0 new FPs everywhere

| corpus | files | ref | rs | FP candidates |
| --- | --- | --- | --- | --- |
| mastodon `app/models` | 248 | 115 | 112 | **0** |
| conference-app | 244 | 1998 | 1998 | **0** |
| gitlab-foss `lib` | 4676 | 1374 | 1044 | **0** |
| mastodon `app` | 1236 | 459 | 410 | **0** |
| haml / liquid / rubocop-ast / faraday / kramdown / mail / parser `lib` | 470 | 61 | 13 | **0** |

### Old-vs-new diagnostic delta

Every diagnostic emitted by the pre-change and post-change release binaries was
diffed over ~29k files (mastodon `app`, gitlab-foss `app`+`lib`,
conference-app, rails, redmine, dependabot-core, rubocop-ast, haml, mail); the
PR #46 reviewer independently swept 26,255 files.

- **Removed: 2 — and they are a REAL-CORPUS WITNESS of the FP fix.**
  `liquid/vendor/bundle/…/liquid-spec-ae64d5b5bf13/lib/liquid/spec/cli/eval.rb`
  at `187:18` and `190:21`, both `flow.always-truthy-condition` ("always
  falsey"). The source is the spec's shape verbatim:

  ```ruby
  reference_output = nil
  reference_error  = nil
  if compare_mode
    reference_output, reference_error = run_reference_implementation(…)
    if reference_error          # 187:18 — rigor-rs OLD fired, oracle silent
      …
    elsif reference_output      # 190:21 — rigor-rs OLD fired, oracle silent
  ```

  Two straight-line `= nil` bindings, a multi-write rebind, then conditions on
  both names. The oracle returns NO diagnostics for that file; rigor-rs OLD
  emitted two, rigor-rs NEW emits none. **The FP therefore has a live corpus
  witness, not only the synthetic probe** — an earlier draft of this note said
  "Removed: 0", which was wrong: the file sits under `liquid/vendor/bundle/`,
  outside the `liquid/lib` path this session originally swept.
- **Added: 5 (2 distinct shapes), all COVERAGE GAINS, all oracle-verified.**
  - `logger-1.7.0/lib/logger.rb:372:6 flow.always-truthy-condition` (×4 — the
    same vendored gem under redmine / haml / mail / liquid). Source:
    `_, name, rev = %w$Id$` then `if name` — `name` is a MISSING tuple slot ⇒
    `Constant[nil]` ⇒ always falsey. **The reference emits the byte-identical
    diagnostic at the same position** (verified directly).
  - `rubocop-ast/spec/.../code_examples.rb:1959:20` — `foo, bar, baz = 1, 2`
    (line 903) then `elsif baz` ⇒ `baz` is a missing slot ⇒ nil. The reference
    cannot be compared in place (that fixture has 21 deliberate parse errors and
    the reference bails), so it was reproduced minimally:

    ```ruby
    foo, bar, baz = 1, 2
    if foo; bar; elsif baz; 1; else 2; end
    ```

    rigor-rs and the reference agree byte-for-byte on both diagnostics (`2:4`
    always truthy, `2:20` always falsey).

Both gains are the direct consequence of `slot_type`'s "missing slot ⇒
`Constant[nil]`" rule — the reference's own rule, now reachable in rigor-rs.

### One new coverage GAP (FP-safe direction) — do not read the delta as "gains only"

The delta above is not uniformly toward the oracle. A multi-write under a
`return` wrapper now widens the earlier binding, because its whole-statement
span is a `collect_flow_writes` entry contained in the `Return`'s span:

```ruby
def m(src)
  x = 5
  return (x, y = src.pair)
  if x                        # OLD: fired.  oracle: fires.  NEW: SILENT
```

0 occurrences in the 26k-file sweep. The SINGLE-target control (`return (x =
src.pair)`) was already a gap on OLD, so the multi-write has merely joined
rigor-rs's pre-existing span-based conservatism — a coverage loss in the
FP-SAFE direction (rigor-rs under-emits), never an FP. Named here so the
claim "`check` output moved only toward the oracle" is not made: it moved
toward the oracle in 6 places (2 removals + 4 additions) and away from it in
this one shape.

Net over the whole sweep: **2 removed / 5 added / 0 new FPs**. The
`node_children` review fix (below) added nothing on real corpora — it needs a
`return` inside a multi-write inside an `ensure`, which does not occur.

## `annotate` and `sig-gen` — user-visible output improved (oracle-exact)

Command output is user-facing surface, so the change is recorded even though
neither is a diagnostic:

| | OLD | NEW | oracle |
| --- | --- | --- | --- |
| `annotate`, `a, b = [1, 2]` | `#=> Dynamic[top]` | `#=> [1, 2]` | `#=> [1, 2]` |
| `annotate`, the following `[a, b]` | `#=> [Dynamic[top], Dynamic[top]]` | `#=> [1, 2]` | `#=> [1, 2]` |
| `sig-gen --print`, `def pair; a, b = [1, 2]; end` | `No candidates` | `def pair: () -> [1, 2]` | `def pair: () -> [1, 2]` |

Both are now byte-identical to the reference where OLD was wrong. The read side
(`[a, b]`) improves only at TOP LEVEL: `annotate` and `sig-gen` both build their
env with `build_toplevel_env` (→ `bind_statement`, wired here), so a def-LOCAL
multi-write's targets are still unbound for them. That def-scoping limitation is
pre-existing and orthogonal to this slice.

## Review follow-up (PR #46)

- **`node_children` gained a `MultiWrite` arm** (`crates/rigor-rules/src/lib.rs`).
  Without it `flow.return-in-ensure`'s generic descent stopped one hop short of
  the `Node::Return` this lowering now puts in the arena. Probe
  `def m(flag); do_work; ensure; a, b = (flag ? (return 1) : 2), 3; [a, b]; end`
  now fires at `4:19`, byte-identical to the oracle (silent on old AND new
  before the arm, so this is a coverage gain, not a regression fix). Pinned by
  `return_in_ensure_descends_through_a_multi_write`.
  - The arm's `target_exprs` half is correct descent but currently unreachable
    for this rule: a `return` embedded in a NON-local target
    (`obj[flag ? (return 1) : 2], b = 3, 4`) never enters the arena, because
    `collect_recoverable_children` recovers reads / writes / calls and NOT
    `ReturnNode`. Pre-existing and orthogonal to multi-writes (silent on old AND
    new; the oracle fires at `4:15`); the single-target control `obj[…] = 3`
    fires on both, since that lowers to a `Node::Call` whose args are descended.
    Pinned as a known gap in the same test so a later `ReturnNode` recovery
    flips it visibly.
- **Deferred (separate slice):** `flow_eval`'s `MultiWrite` arm does not record
  the if-EXPRESSION predicate snapshot its single-target sibling does
  (`crates/rigor-infer/src/lib.rs`, the `Node::If` peek before the bind). Silent
  on OLD and NEW, so it is a coverage gain needing its own parity evidence, not
  a fix.
