# rigor-rs#133: a rebind on a `next` / `break` path (upstream #1248, #1215)

**Verdict: closed by a decline, not by porting the mechanism.** The FP rows go silent, every
must-still-fire control still fires, and the standing sweep is byte-identical.

## What the reference does

Upstream #1215 (`cbdd4a8f`, blocks) and #1248 (`740b9150`, loops) join the scope at every
`next` that targets a block invocation or a loop body into its exit scope
(`StatementEvaluator#evaluate_invocation` / `#loop_iteration`, collected through a
`next_scope_sink` threaded through `sub_eval`). They also join each `break` arm into the
continuation from a pass whose entry is the converged binding (`join_block_break_bindings`,
`loop_break_arms`), and carry a jump inside `begin … ensure` through the clause.

## Why the port could not take the same shape

The issue's rows are all at **top level**, and the port types a top-level read from
`Typer::build_toplevel_env`. That is a flat binder: it applies only straight-line `x = …` /
`a, b = …` statements and never sees a rebind nested in an `if`, a loop or a block. There is
no loop fixpoint or block write-back behind it that a `next` sink could join into. The
`next` was therefore never the cause. Measured on master:

| shape (top level) | reference `e59b7b89` | port master |
|---|---|---|
| `w = String.new; w = 1 if $c; w.even?` (no loop, no jump) | silent | `for String` |
| same `if`/`else`, both arms rebinding | silent | `for String` |
| `while … w = i; end` (no `next`) | silent | `for String` |
| `n = 1; n += 1.5; n.nan?` | silent | `for 1` |

Inside a `def` the port reads no top-level env (`ScopedEnv::at`), so every shape and every
control is silent there. The controls are coverage gaps, not FPs.

## The fix

`Typer::build_toplevel_check_env` (rules-only; the CLI's `type-of` / LSP / `annotate` /
`sig-gen` keep `build_toplevel_env`) walks the same statements in order. Any statement it does
not bind widens, to `Dynamic`, every top-level local rebound inside it: a plain, operator or
multiple write, or a `rescue => e`. Writes inside a `def` / `class` / `module` body do not
count. A later straight-line write re-establishes the type.

One trap: `check_narrowed_call` and `check_collection_call` fire **only** on a
`Dynamic`/`Top` env type. Widening a local would open a gate that the stale concrete type used
to close, for example a disjoint `is_a?` guard that the reference collapses to `Bot`. Those two
rules keep reading the unwidened env (`ScopedEnv::gate_at`), so they behave exactly as on
master.

## Measured

Fixture `112_jump_path_rebind.rb`: on master, 7 FPs (rows 1, 3, 4, 6, 8, 16, 17). On the
branch, 0 FPs. One gap remains, row 8: the reference reports `possible-nil` for `nil |
String`, which the port does not model.

| gate | result |
|---|---|
| `run_snapshot.rb` | 564 matched / 48 gaps / **0 FP** |
| sweep, rigor-rs master vs branch (8 corpora, 8,933 files, local clones) | **identical**: 173,835 diagnostics, 0 added, 0 removed |

The live `run.rb` under a locally installed Ruby 4.0.6 shows one FP, fixture 82 `:20:29`. It
is identical on master: that environment's reference is silent there, while the committed
snapshot fires.

## Coverage traded (FP-safe losses, all at top level)

These rows used to match on `(rule, line, column)`, often with the wrong message. They are
silent now:

- `w = "s"; if $c; w = 1; end; w.zzz`: the reference says `for "s" | 1`.
- a use **before** a later nested rebind: `w = "s"; w.zzz; xs.each { |e| w = e }`. The env
  is still end-of-file state for every use.
- a copy of a widened local: `x = w`.

A position-aware top-level env would recover all of them. That is the real port of #1248 /
#1215 onto the port's flow substrate, and it is out of this slice's scope.

## Residual FP found in passing

`w = "s"; for w in [1, 2]; end; w.even?` still fires `for "s"`, while the reference is silent.
The arena drops the `for` index target (`ast.rs`, `Node::Loop`), so no widening can see the
rebind. This is pre-existing and unrelated to jumps. It needs an index-target field on the
lowered loop.
