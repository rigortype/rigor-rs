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

Verified 2026-09-25 on the standard environment (Ruby 4.0.6, submodule at the pin, release
binaries of both master and the branch):

| gate | result |
|---|---|
| `cargo test --workspace`, `cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings` (fresh target) | pass / clean |
| `run_snapshot.rb` | 564 matched / 48 gaps / **0 FP** |
| `run.rb` (live) | 564 matched / 48 gaps / **0 FP**; fixture 82 `:20:29` matches |
| `snapshot.rb` regen | **no drift** in any committed snapshot |
| `fp_audit.py --gaps --sweep` (standing set, 8/8 corpora, 9,337 files) | **0 FP** on the branch and on master; output identical apart from timings, gap totals included |
| `docs_check.py` | pass |

The first run of this slice was on a machine that did not have the standing corpora. It used
shallow clones of 8,933 files and saw three things that do not reproduce here: one FP in live
`run.rb` (fixture 82), three drifted snapshots, and one sweep candidate (gitlab-foss
`environment.rb:51:61`). All three came from that environment. None is a property of the slice.

Every row 1–18 was re-probed in its own fresh cwd, with `--no-cache` on the reference. Each
row reproduces the PR's table, and every control (2, 5, 7, 10, 12, 14, 18) fires with a message
byte-identical to the reference's.

## Design review: other `Dynamic`-only gates

The widened env reaches `check_call`, `check_wrong_arity`, `check_always_raises`,
`check_argument_type_mismatch`, the `raise`-operand rule and `static.value-use.void`. Each of
them declines on a `Dynamic` receiver or operand; `faithful_param_rejects_arg` treats a
`Dynamic` argument as a gradual `maybe`. `class_narrowing_pass`,
`collection_shape_snapshots` and `nilable_receiver_snapshots` build their own envs, and the
diff does not touch them. `check_narrowed_call` and `check_collection_call` are the only
rules that fire on a `Dynamic`/`Top` receiver, and they read the unwidened `gate_at`.

Adversarial probes found no new FP. On these shapes the branch also removes master FPs that
the fixture does not name:
- `w = 1; [1].each { w = "s" }; if w.is_a?(Float); w.zzz; end`: the reference is silent,
  master fires `for 1`.
- `e = 1; e = StandardError if $c; raise e`: master fires `raise-non-exception`.
- `w = [1]; [1].each { w = nil }; w.first(1, 2, 3)`: master fires wrong-arity where the
  reference says possible-nil.

## Adversarial review (before merge)

An Opus reviewer probed about 80 shapes: span edge cases, recovery carriers, folds through a
widened argument, rule precedence, and performance. It found **no branch-only FP on (rule,
line, col)**. It did find one claim above that is too strong. Widening cannot ADD a site, but it
can CHANGE the message at a site that all three engines flag. Take
`w = 5; if $c; w = 5; end; "abc".center(w).lenght`: the reference and master say
`for " abc "`, while the branch says `for String`. The widened argument no longer folds, so the
message falls back to the RBS return. The gates key on (rule, line, col) and cannot see this.
Accepted for this slice, and recorded with the additional coverage losses on rigor-rs#152. The
cheapest of those is a block-param shadow (`{ |w| w = 2 }`) being counted as a rebind. The
review also surfaced pre-existing FPs that master shares, filed as rigor-rs#153: `END { w = 1 }`,
`defined?(w = 1)` and `(w = 1) rescue nil` are bound as straight-line writes.

## Coverage traded (FP-safe losses, all at top level)

These rows used to match on `(rule, line, column)`, often with the wrong message. They are
silent now:

- `w = "s"; if $c; w = 1; end; w.zzz`: the reference says `for "s" | 1`.
- a use **before** a later nested rebind: `w = "s"; w.zzz; xs.each { |e| w = e }`. The env
  is still end-of-file state for every use.
- a copy of a widened local: `x = w`.

A position-aware top-level env would recover all of them. That is the real port of #1248 /
#1215 onto the port's flow substrate, and it is out of this slice's scope. Tracked, together with
row 8, as rigor-rs#152.

## Residual FP found in passing

`w = "s"; for w in [1, 2]; end; w.even?` still fires `for "s"`, while the reference is silent.
The arena drops the `for` index target (`ast.rs`, `Node::Loop`), so no widening can see the
rebind. This is pre-existing and unrelated to jumps. It needs an index-target field on the
lowered loop. Tracked as rigor-rs#151.
