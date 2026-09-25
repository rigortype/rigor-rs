# rigor-rs#151 + #153 rows 1–3: writes the local-env binders saw wrongly

**Verdict: closed.** Every FP row in the two issues is silent now. Every must-still-fire
control stays byte-identical to the reference. The two message-drift rows (c1, c2) now
produce the reference's message. The standing sweep is byte-identical to master on full
`(path, line, col, rule, message)` tuples. #153 row 4 (`"abc".index("z").to_a`) has a
different cause and is out of scope.

## What the reference does

Read from `reference/rigor` @ `e59b7b89`, confirmed with probes:

- `StatementEvaluator#evaluate` dispatches on `HANDLERS`. A node class with no handler is
  typed as a pure expression, and **the scope is left unchanged**. `DefinedNode`,
  `PostExecutionNode` (`END`), `PreExecutionNode` (`BEGIN`), `SuperNode`,
  `ForwardingSuperNode` and `YieldNode` have no handler. So a write inside any of them never
  binds and never widens, at the top level, inside a `def`, and as an assignment's value.
  The spec asked for this to be checked for `BEGIN`: it behaves the same as `END` (probes
  b1/b2). `super` and `yield` were not in the spec. The probes show they behave the same way
  (g1/g2, s1–s5, m1/m3/m4/m5), so they are inert too. This resolves toward the oracle.
- `eval_for` binds the index (`bind_for_index`: a `LocalVariableTargetNode`, or a
  `MultiTargetNode` through the multi-target binder) to the element type. It joins the
  zero-iteration scope with the body exit.
- `eval_rescue_modifier` joins the after-expression scope with the nil-injected rescue arm.
  The write is conditional.

## Mechanism

**A kind flag on the carrier, plus a span set computed once at lowering.**

- `Node::Statements` gains `kind: StatementsKind`:
  - `Sequence` is a Prism `StatementsNode` or the `#{}` of an interpolation.
  - `Recovered` is the generic recovery carrier (a `rescue` modifier, `h[k] ||= v`, …).
  - `Inert` covers a `defined?` operand, an `END` or `BEGIN` body, and the arguments and
    block of `super` or `yield`. These are lowered explicitly. Their recovered children
    are the same as before.

  The structural walks all match `Statements { body, .. }` and never read `kind`, so the
  dead-assignment read gather and the call rules still see every child unchanged.
- `LoweredAst::in_inert_carrier(span)` is built from the inert spans at lowering time.
  Every write collector (`collect_flow_writes`, `collect_rebind_writes`, `toplevel_rebinds`,
  `indexed_flow_writes`, `local_reach`) drops writes inside an inert carrier. Nothing widens
  for them, which is what gives c1/c2 the reference's message. The drop uses span
  containment, so it is orphan-proof.
- `Node::Loop` gains `index: Vec<(String, Span)>`. It holds the local names a `for` index
  binds, each keyed by its own target span, which lies inside the loop span. It is empty
  for `while`/`until` and for a non-local index. The collectors treat each name as a
  rebind, so the loop widens it at its exit. **Widening is the floor. The element type is
  not bound.**

Why not a new `Node` variant: it would touch every exhaustive `match`, and any structural
walk that missed it would lose the carrier's children (a dead-assignment FP source). The
flag cannot orphan anything, and a binder that ignores it behaves as it did on master.

### Per consumer

| consumer | Sequence | Recovered | Inert |
|---|---|---|---|
| `bind_check_statement` (check env) | descend | widen | nothing (no writes left to widen) |
| `flow_eval_stmt` (always-truthy) | descend | widen | nothing |
| `nil_flow_stmt` (possible-nil) | descend | descend, then widen and drop facts | record uses in values, bind nothing |
| `class_flow_stmt` (narrowing) | descend | descend (unchanged) | record uses in values, bind nothing |
| `coll_flow_stmt` (collection shape) | descend | descend, then widen | record uses in values, bind nothing |
| `bind_statement` (flat env: `type-of`, hover, `gate_at`) | descend | descend (unchanged) | skip |
| `definitely_assigns`, `statement_sections`, `stmt_terminates`, `expr_reach` | as before | not a sequence | not a sequence / `UNKNOWN` |

Two design choices are load-bearing:

- `class_flow_stmt` keeps descending `Recovered`, and records the uses in an inert carrier.
  `yield v.use` and `super(v.use)` under a guard fire on the reference (stage 3b-1 rows
  d23/g2). A first cut that made `Inert` a no-op broke them. Post-widening a
  `Recovered` carrier there would open the `Dynamic`-only mint gate. That is the same trap
  the #148 note describes.
- No `coarse_locals` entry for the `for` index. It was tried. It silenced n1 (a master FP),
  but the scope-wide coarse set also silenced four matched rows (a2, n2, n3, and unit row
  g1c, where the guard is *before* the loop). Without it, the class-narrowing pass behaves
  exactly as on master. n1 stays a pre-existing FP.

`bind_statement` is shared with `type-of`, hover and `sig-gen`. Only the inert skip was
added there. `w = "s"; defined?(w = 1); w` now hovers `"s"` (reference `"s"`, master `1`).
`Float(s)` after `(s = "x") rescue nil` hovers `Dynamic[top]` (reference `Dynamic[Float?]`,
master `Float`), through `definitely_assigns`. `sig-gen` output does change, contrary to this
note's first draft. Take a method that does `yield(w = u)` / `defined?(w = u)` and then `Float(w)`:
it now emits `-> Float`, byte-identical to the reference, where master emitted nothing. The
`Integer(w)` form emits `-> Integer` where the reference says `-> 12`. The same method without
the inert write already mismatches that way on master. This is sound extra coverage under the
generative-tool bar, and it adds no new kind of byte mismatch (adversarial review).

## Three-way probes

Each probe ran in a fresh temp cwd, with `--no-cache` on the reference and
`--format json`. Every cell compares the full `(rule, line:col, message)` tuple. "tr/fa"
stands for always-truthy/falsey (`flow.always-truthy-condition`). "um" stands for
`call.undefined-method`.

| # | probe | reference | master | branch |
|---|---|---|---|---|
| f1 | `w="s"; for w in [1,2]; end; w.even?` | silent | um 3:3 `"s"` | silent |
| f2 | `w=nil; for w in [1]; end; if w` | silent | fa 3:4 | silent |
| f3 | f2 in a `def` | silent | fa 4:6 | silent |
| f5 | `for a, b in [[1,2]]` | silent | um 4:3 `"s"`, 5:3 `"t"` | silent |
| f6 | read inside the body | silent | um 3:5 `"s"` | silent |
| f7 | `BEGIN { w = 1 }; w.upcase` | silent | um 3:3 `1` | silent |
| r1 | `END { w = 1 }; w.upcase` | silent | um 3:3 `1` | silent |
| r2 | `defined?(w = 1); w.upcase` | silent | um 3:3 `1` | silent |
| r3 | `(w = 1) rescue nil; w.upcase` | silent | um 3:3 `1` | silent |
| c1 | `w=1; END { w = nil }; if w` | **tr** 3:4 | fa 3:4 | **tr** 3:4 |
| c2 | `w=nil; defined?(w = 1); if w` | **fa** 3:4 | tr 3:4 | **fa** 3:4 |
| c3 | `w=nil; (w = 1) rescue nil; if w` | silent | tr 3:4 | silent |
| c5 | c3 in a `def` | silent | tr 4:6 | silent |
| c6 | `(w = 1; w) rescue nil` | silent | tr 3:4 | silent |
| c7 | `super(w = 1) rescue nil; w.upcase` | silent | um 3:3 `1` | silent |
| b1 / b2 | `BEGIN` with `if w` | fa / tr 3:4 | tr / fa | fa / tr |
| g2 / s1 / s2 / s3 / s5 | `super(w = 1)`, `super { }`, `yield(w = 1)` with `if w` | fa | tr | fa |
| m1 / m3 / m5 | `w="s"; super(w = 1)` / `super { w = 1 }` / `x = super(w = 1)`, then `w.even?` | um `"s"` | silent | um `"s"` |
| s8 | `h[w = 1] \|\|= 2; w.upcase` | silent | um `1` | silent |
| a3 / a4 | `def m(s)`, rescue-mod / `defined?` write, then `Float(s).frob` | silent | um `Float` | silent |
| a9 | `raise "x" rescue nil` as the guard branch's tail | silent | um `String` | silent |
| h6 | `w=nil; for w in xs; end; w.even?` | possible-nil 4:3 | um 4:3 `nil` (FP) | silent |
| **controls** | | | | |
| k1 | `for i in [1]; end; w.even?` | um `"s"` | same | same |
| k2 | same, `if w` | fa 3:4 | same | same |
| k3 | `defined?(x = 1); w.upcase` | silent | silent | silent |
| k4 | `w = 1; w.upcase` | um `1` | same | same |
| k5 | `x = (w = 1); w.upcase` | um `1` | silent | silent (unchanged gap) |
| k6 | `foo(w = 1); w.upcase` | unresolved + um `1` | unresolved | unresolved (not worse) |
| k7 | `def m; y = 1; defined?(y); end` | silent | silent | silent |
| g6 | `for @a` / `for A` binds no local | um `"s"` | same | same |
| a6 / b6 | a definite write before `Float(s)` / `rand(s)` | fires | same | same |

Fixtures 112 and 113 are unchanged: both harnesses show the same 609 matched / 48 gaps
on the old fixtures.

## Adversarial review (before merge)

An Opus reviewer ran about 190 three-way probes. They covered:
- inert writes under `super`/`yield`/`defined?`/`BEGIN`/`END` in every flow pass
- span edges, heredocs and multibyte source
- every `for` index form
- `type-of`, hover and `sig-gen`
- dead-assignment

**No branch-only key.** The claim of no message regression was wrong: the review gate's Opus
pass found five more at keys where master matched the reference (see Residuals, #152). The
branch reaches some new key-matched sites where it carries master's existing wording gap, for
example `for Integer` where the reference says `for 12`. It also removes more master FPs than
this note lists: the `defined?(foo(v = nil))` shape, `is_a?` guards after `BEGIN`/`END`, and the
nested and in-block `for` forms. It found no code defects.

The orchestrator re-derived the "#148 widening is not FP-safe" residual against a pre-#148
binary (877ff4f). `w = 5; while $c; w = 1; end; "abc".center(w).lenght` fired there too, as
`for " abc "`. It is the union-of-literals argument FP, recorded on rigor-rs#146, and not a
product of the widening.

## Residuals (all pre-existing or FP-safe; none adds a key)

- **Six message drifts at matched keys, from widening (#152).** The reference and pre-branch
  master agree on each; the branch widens `w` so the message no longer carries the value.
  Found after merge by the review gate's Opus 5.5 pass (only e11 was known at merge).

  | # | probe | reference | branch |
  |---|---|---|---|
  | e11 | `w = 5; for w in [5]; end; "abc".center(w).lenght` | `for " abc "` | `for String` |
  | d3 | `w = 5; for w in [5]; end; [w].frob` | `for [5]` | `for [Dynamic[top]]` |
  | d1 | `w = 5; (w = 5) rescue nil; "abc".center(w).lenght` | `for " abc "` | `for String` |
  | d2 | `w = 5; (w = 5) rescue nil; [w].frob` | `for [5]` | `for [Dynamic[top]]` |
  | d4 | `w = 5; (w = 5) rescue nil; { a: w }.frob` | `for { a: 5 }` | `for { a: Dynamic[top] }` |
  | d6 | `w = 5; h = {}; h[w = 5] ||= 1; [w].frob` | `for [5]` | `for [Dynamic[top]]` |

  e11 and d3 come from the `for`-index floor; binding the element type would fix them. d1,
  d2, d4 and d6 come from the `Recovered` widening in `bind_check_statement`. For a rescue
  modifier, joining the pre- and post-bind envs as the reference's `eval_rescue_modifier`
  does would fix the same-value rows.
- **Pre-existing FPs the floor does not cure (the key is the same on master):**
  - e7/e9/x1/x2/x5/x6: a widened or `for`-rebound argument to a folding method. The
    reference unions the argument's members and goes silent. The port says `for String` /
    `for Integer` (master said `for " abc "` / `for "abc"` / `for String`).
  - e8/e13/x3 are the same family on master, through #148's `while`/block widening. This
    is the #153 row 4 cause: an RBS return used without its nil arm, or a Dynamic argument
    reaching a concrete return where the reference has a union.
  - n1: `def m(v); for v in ["a"]; end; if v.is_a?(String); v.frob`. The reference is
    silent and both ports fire `for String`.
- **Coverage the floor gives up** (silent where the reference fires): b8 (`for "a" | 1`),
  s7 (`h[w = 1] ||= 2` then `if w`, reference always-falsey), g9/h6 (reference
  possible-nil from the rescue/`for` join), e3/e5/n6 (narrowed union after a rebind).
  Master was silent on or mis-worded each of these.
- **Pre-existing gaps not touched:** `flow.dead-assignment` on a write under `defined?`,
  `END`, or a `rescue` modifier (d1/d2/d4). The reference fires. The port is silent
  because `descend_trailing` treats the write as the implicit return. Also k5/k6/g3/s9.
- The recovery carriers other than `rescue` are widened, not inert, even where the
  reference also has no handler. Only the six node kinds above were verified.

## Gates

Numbers are on the branch head. The environment was Ruby 4.0.6, with the submodule at
`e59b7b89`.

| gate | result |
|---|---|
| `cargo test --workspace` | pass (1,328 tests) |
| `cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings`, fresh `CARGO_TARGET_DIR` | clean |
| `run.rb` (live, release binary) | 619 matched / 48 gaps / **0 FP** (114 fixtures; master on 113: 609 / 48 / 0) |
| `run_snapshot.rb` | 619 / 48 / **0 FP** |
| `snapshot.rb` | wrote only `114_binder_carrier_writes.json`; `--check` shows no drift |
| `fp_audit.py --gaps --sweep` (8/8 corpora, 9,337 files) | **0 FP**, 3,829 gaps |
| sweep, master port vs branch port, full tuples | **identical**: 170,716 diagnostics on each side, +0 / −0 |
| `docs_check.py` | pass |

Fixture `114_binder_carrier_writes.rb` agrees exactly with the reference: 10 diagnostics,
all matched, and the branch emits nothing else. Master emits 22 tuples the reference does
not, including the five wrong-message rows (lines 84, 91, 98, 166, 175).
