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
conference-app, rails, redmine, dependabot-core, rubocop-ast, haml, mail).

- **Removed: 0.** The FP shape (straight-line binding → multi-write rebind →
  condition on it) genuinely does not occur in the surveyed corpora, exactly as
  the spec predicted. The fix is demonstrated by the probe, not by a corpus
  delta.
- **Added: 4, all COVERAGE GAINS, all spot-verified against the oracle.**
  - `logger-1.7.0/lib/logger.rb:372:6 flow.always-truthy-condition` (×3, the
    same vendored gem under redmine / haml / mail). Source:
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
