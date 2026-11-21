# #681 — loop-pass entry edge narrowing + same-class scalar union verdict

Issue: `y = gets ? 1 : "s"; x = 0; while y.is_a?(Integer); x = y;
end; x.zz` — the reference fires `for 0 | 1`; the port fired
`for "s" | 0 | 1`, a message drift at the same (rule, line, col)
key. The loop's `predicate_writes` gate only watches TRACKED
names; a predicate that narrows an untracked local the body then
copies into a tracked one leaked the un-narrowed type. PR #806,
merged 2026-11-22 (`614989a5`, head `a4be2d25`; two `Needs fix` rounds + an owner-authorized fix-confirmation, **Approved**).

## What changed

- `loop_continuation` takes the `LoopKind` and, inside the CAP≤3
  fixpoint pass, narrows each predicate-subject local on the
  ENTRY edge before the body runs: `predicate_edge_locals(ast,
  predicate, entry_truthy)` (truthy for `while`, falsey for
  `until`) returns per-subject `DeadOp`s; `dead_subject_type`
  narrows the `pass_env` binding. Ports the reference's
  `loop_pass_entry` / `Narrowing.predicate_scopes` edge walk
  (`statement_evaluator.rb:4024`). Subjects the extractor does
  not recognise keep their binding — the safe side.
- `predicate_edge_locals` mints `DeadOp::Isa{exact:false}` for
  `C === local` for ANY `dynamic_base:false` ConstantRead `C`
  receiver — `lexical_class_name` falls back to the bare spelling
  for undeclared names too. `local === C` keeps `Cmp`.
  `eql?`/`equal?` mint nothing (absent from
  `simple_dispatch_name?`).
- `dead_subject_type` carries `narrow_class`'s environment-only
  ordering: an asked class the index cannot resolve (undeclared
  or project-only — `Bar`, `Foo`, `FOO`, `Foo::Bar`) makes every
  member's ordering `:unknown`, so the positive edge answers
  `untyped`, never `Bot` (`declines_bot?`, narrowing.rb:3651);
  the negative edge keeps every member. `instance_of?` is exempt
  — the exact-class test answers `Bot` on its own semantics (the
  oracle fires `for 0` on `instance_of?(Unresolved)`).
- `dead_subject_type`'s `Cmp` ports `narrow_equal`: the peer must
  be a literal NODE (`static_literal_type` reads no local —
  `y == z` with a pinned `z` narrows nothing); Float peers are
  untrusted (`TRUSTED_EQUALITY_LITERAL_CLASSES`); a non-singleton
  trusted literal narrows only a finite trusted-literal domain —
  `y == 1` on `1 | 2.0` keeps `2.0`; singletons (`nil`/`true`/
  `false`) narrow memberwise with shape/singleton drops on the
  positive edge.
- `type_has_decline` drops `Type::Bottom`: the `Bot` pass exit is
  a real edge answer (dead body) that `join` absorbs — without
  this, `until y` on `y = 1 | "s"` left `x` at `0 | "s"` instead
  of `0`.
- `check_call` collapses a `Union` whose members are ALL
  `Type::Constant` resolving to ONE class into that class for the
  scalar verdict. The issue's rows cannot fire `for 0 | 1`
  without it: `0 | 1` is a `Constant` union that previously fell
  to `check_union_call` and declined. Verified against the
  reference: `for 0 | 1`, `for "a" | "b"`, `for :a | :b`,
  `for 0.5 | 1.5` all fire; `[1] | [2]` / `{a:1} | {b:2}` /
  `nil | 1` / cross-class stays silent (union/decline path
  unchanged).
- Two stale test expectations updated: `h[:a] ||= 2` then
  `h[:a].upcase` fires `for 1 | 2` in the reference too — the
  index-or-write carrier now surfaces the stored slot's union to
  the scalar verdict through the same collapse.

## Measured outcome

Probe sweep vs `--no-cache` reference, full tuples (~50 rows):
the issue's rows all MATCH — `is_a?`/`kind_of?`/`instance_of?`,
`==`/`!=`, `C ===` (while + until), `!y.is_a?(String)`, `:a`
member, `&&` compounds, `until y` (`for 0`), `== nil` (`for 0`),
`!= nil` (`for 0 | 1`). Round-1 review FPs all closed: unresolved
/`is_a?`, project-class `is_a?`/`===`, module `===`, `eql?`/
`equal?`, `==` Float-domain, `==` pinned-local peer. Bonus
closures vs master: `Foo === y` / `FOO === y` / `Foo::Bar === y`
were firing `for "s" | 0 | 1` on master and now sit silent like
the oracle; `eq_str`/`eq_true`/`ne_str`/`eq1_nil_member`/
`is_a_nominal_subj`/`tuple_subj`/`until_ceq` messages tightened
to the oracle's.

Residual divergences (unchanged from master, safe side, disclosed
on the PR): `||` truthy-edge union narrowing unmodelled
(`while y.is_a?(Integer) || y == 1` → `"s" | 0 | 1` vs oracle
`0 | 1` — same-key drift, identical on master); `@y` ivar
subjects (coverage loss — the extractor takes locals only);
Dynamic subject does not rebind to `Nominal[class]`
(`narrow_class_other`, coverage loss); `respond_to?` unmodelled;
def-scoped/body-tracked/predicate-write coverage losses.

Sweep: `fp_audit.py --gaps --sweep` **0 FP** at the release
binary; gap totals identical to `origin/master` (this fix moves
messages at existing keys and synthetic coverage rows — the sweep
corpora hold none of them). `cargo test -p rigor-infer` +
`rigor-rules` green; CI green; clippy 1.88 clean in a fresh
`CARGO_TARGET_DIR`. Perf: `check` on concurrent-ruby (345 files)
0.38s vs 0.39s master; net-ssh (202 files) 0.30s vs 0.30s — one
bounded `predicate_edge_locals` walk + subject narrow per pass,
CAP ≤ 3.
