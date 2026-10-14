# #386 — dead-position leaks: rescue entry scope, loop back-edges, folded predicates

PR #397, merged `2bc9df6` (squash of `cacad7a` + `2a2002d`). Follow-up to #368
(`dead.rs` dead positions): the exclusions existed but `local_reach` and the
flow walk let dead writes bind at reads the oracle never lets them reach.

## Leak families closed

**Protected body → rescue clause.** A `rescue` clause evaluates from the
`begin` ENTRY scope (`eval_begin`'s `bind_rescue_reference(current,
entry_scope)`), so a write in the protected body binds nothing inside a
clause — `begin q=1 rescue Float(q).w end` must stay silent. `RescueArm`
gained a `main` hull and `write_reaches` drops a main write whose use STARTS
inside a clause (`use_.0 < a.span.1` — the last clause's clipped span ends at
the `begin`'s own end, so a zero-width probe at `(end, end)` must not count:
that nearly re-broke `begin; h.default ||= 0; rescue; end` widening).
`retry` does NOT reopen this — `widen_entry_for_retry` carries only
entry-bound names, which arrive via their own pre-`begin` writes anyway.
Exception added at review: a `Node::Loop` (`while`/`until`/`for`) holding BOTH
the write and the clause read is a back-edge carrier — pass N's body write
lands in pass N+1's entry scope (`q=nil; while c; begin q=1 rescue q.f end;
end` reads `nil | 1` and declines). `loop do`/`.each` are deliberately NOT
carriers here — the reference fires `f for nil` on that shape, and the port's
silence is a pre-existing coverage loss, not an inconsistency to paper over.

**Folded-dead writes.** A write in a folded `if`/`unless`/`while false` arm
reaches nothing; `local_reach`'s write scan now filters through
`dead.write_reaches`. A write after the read only reaches via a carrier
containing both — a post-loop `q=1` never loops back into the body's `if q`.
A write's own RHS is not yet bound for reads inside it (`begin q=1 rescue
Float(q).w end` lowers as `q = (1 rescue Float(q).w)`).

**`branch_terminates?`'s Bot half.** `stmt_exits` now treats `Kernel.raise`/
`exit!`/`Kernel.exit` (`call_declares_bot`, now `pub(crate)`) and a same-file
TOPLEVEL `def` whose body is provably bot (`def die; raise; end`) as
non-returning, so those rescue arms contribute nothing post-`begin`.
`def_exits` binds the LAST toplevel def and is guarded by `self_answers`
(`C#die` beats a toplevel `die`).

**Constant-method truthiness.** `expr_truthiness_fold` folds `Const.m`
(`def self.off? = !on?`) and `self.class.m` in instance methods through the
singleton literal-tail table, eliding the dead arm AND its writes.

**Block interior.** Statement-level non-proc blocks (`each`/`loop`) no longer
bypass the dead filter wholesale: the interior scans the file region unless
the read's name is block-parameter bound (`|_k, v|` floors `v` to the untyped
yield — `v = "x" if v.nil?` cannot pin `Float(v)`). In-block reads with
in-block writes seed LITERAL nil (the reference's `join_with_nil_injection`),
not UNTYPED — `each { 1 rescue q=1; q.w }` reads `nil | 1` and declines.
Proc-like params (`1.tap {|v|}`) keep the ordinary join.

**Plain-write predicates.** `type_of` evaluates a plain write's expression
position as its RHS (`type_of_assignment_write`): `if (q = nil)` /
`(@q = nil)` / `(Q = nil)` / `(q = 1)` warn like the oracle. Op-writes are
excluded — `q ||= nil` evaluates the old value on one edge.

## Review-round fix (round 2 of 2)

Opus found two items; both landed in `2a2002d` and were confirmed by the
fix-confirmation pass (which also found the fix REMOVED two master FPs):

- the loop back-edge exception above (was a new FP);
- `walk_fold_defs` now harvests TOP-LEVEL `class << C` sites onto C's
  singleton key — a later `def off? = true` in one made `C.off?` fold `false`
  and elide a live arm (the disagree-decline merge needs both sites; nested
  `class <<` sites were already harvested).

## Deferred findings (filed / documented)

- `project_defines_anywhere` makes `call_declares_bot` decline `Kernel.raise`
  whenever ANY project `def raise` exists — even an unrelated class's
  instance method: `class C; def raise(*); end; end; begin 1 rescue q=1;
  Kernel.raise end; Float(q).w` fires on master AND branch, silent on the
  oracle (pre-existing FP) — **filed #398**.
- An always-truthy `if` whose only live arm exits does not deaden the
  following statements (t5/t6 — same on master) — **filed #399**.
- `for x in xs` as a back-edge carrier for rescue-clause reads: ref fires
  `f for nil` on `for x in [1]; begin q=1 rescue q.f end; end`; both ports
  silent (pre-existing gap — `for` lowers to `Node::Loop` but the
  carrier semantics differ there).
- `self.class.off?`/`selfclass_mod` warning gap: the error is gone, the
  oracle's always-falsey warning stays missing (safe side).
- Paren-span drift: `if (q = nil)` warns at `q` (col 5) vs the oracle's `(`
  (col 4) — the AST does not retain the parenthesized predicate span.
- `u2`/`p2`/`p3`/`s3`/`w3`/`w6` coverage losses, `Float` vs `1.0` message
  drift, param-shadow env FPs (`v = "s"; each {|v| Float(v).f}` — flow-env
  binding, not `local_reach`).

## Measured outcome

Every issue row byte-identical or count-identical to the oracle; sweep **0 FP
/ 9,337 files / 8 corpora**; `check.rs` 17/17, `rigor-infer` 346/346, gate +
CI green. Round-2 reviewer measured 19/19 probes clean incl. one new
byte-level parity gain (`class << C; def on? = true` single-site fold).
