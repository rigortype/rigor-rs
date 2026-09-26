# The reference JOINS a local written in a `rescue` modifier, a `for` index,
# or a `begin`/`rescue` arm with the state before the construct — nil-injected
# scope joins (`join_with_nil_injection`) — where the #154 port widened the
# local to `Dynamic[top]` (rigor-rs#167). One scope: every row uses its own
# locals. Measured against the pinned reference (e59b7b89, fresh cwd,
# `--no-cache`); every row below is a full-tuple match.

# --- rescue modifier --------------------------------------------------------

# (1) the issue row: `entry | write` — fires `for [5 | 6]`.
r1 = 5
(r1 = 6) rescue nil
[r1].frob

# (2) same constant collapses the union — fires `for [5]`.
r2 = 5
(r2 = 5) rescue nil
[r2].frob

# (3) the join renders inside a hash shape, a tuple, and a nested tuple —
# fires `for { a: 5 }`, `for [5, 1]`, `for [[5]]`.
r3 = 5
(r3 = 5) rescue nil
{ a: r3 }.frob
[r3, 1].frob
[[r3]].frob

# (4) the joined element feeds an argument-type-mismatch message —
# `expected Numeric, got [5]`.
r4 = 5
(r4 = 5) rescue nil
1.fdiv([r4])

# (5) the joined element survives a tuple `first` fold — fires `for 5`.
r5 = 5
(r5 = 5) rescue nil
[r5].first.frob

# (6) the rescue arm's own write joins the protected arm's — fires `for [6 | 7]`.
r6 = 5
(r6 = 6) rescue (r6 = 7)
[r6].frob

# (7) nested rescue modifiers join through — fires `for [5 | 6]`.
r7 = 5
((r7 = 6) rescue nil) rescue nil
[r7].frob

# (8) a rescue modifier in VALUE position binds its joined result —
# fires `for [6 | 7]`.
r8 = (6 rescue 7)
[r8].frob

# (9) a rescue modifier inside an interpolation is a carrier too —
# fires `for [5 | 6]`.
r9 = 5
"#{r9 = 6}" rescue nil
[r9].frob

# (10) union receiver: the method is absent on BOTH joined members —
# fires `for "s" | 1` (the must-still-fire control).
r10 = "s"
(r10 = 1) rescue nil
r10.frob

# (11) control: `upcase`/`downcase` exist on the String member — silent.
r11 = "s"
(r11 = 1) rescue nil
r11.upcase
r11.downcase

# (12) control: a HOMOGENEOUS union (`5 | 6`, one class) stays silent,
# and the #151 FP does not return — `w.upcase` must not fire `for 1`.
r12 = 5
(r12 = 6) rescue nil
r12.frob

# --- operator writes --------------------------------------------------------

# (13) `w op= v` — the flat env declines the op's result fold, so `w` reads
# `Dynamic[top]` where the reference folds `w += 1` to `6` (a coverage gap —
# same position fires `for [Dynamic[top]]`; the fold is follow-up coverage).
o13 = 5
o13 += 1
[o13].frob

# (14) `w ||= v` / `w &&= v` likewise widen to `Dynamic[top]` — the
# truthy/falsey-fragment union (`5 | 6`, `6?`) is unmodelled coverage.
o14a = 5
o14a ||= 6
[o14a].frob
o14b = nil
o14b ||= 6
[o14b].frob
o15a = nil
o15a &&= 6
[o15a].frob
o15b = 5
o15b &&= 6
[o15b].frob

# (16) an op-write inside a rescue modifier still JOINS the entry value —
# `for [5 | Dynamic[top]]` where the reference's fold gives `5 | 6`.
o16 = 5
(o16 += 1) rescue nil
[o16].frob

# --- index writes -----------------------------------------------------------

# (17) a plain `recv[…] = v` index argument EVALUATES and binds —
# fires `for [7]` (both index arguments evaluate, the later arg wins).
i17 = 5
h17 = {}
h17[i17 = 6] = h17[i17 = 7]
[i17].frob

# (18) a compound index-write's ARGUMENT is dropped, not rebound —
# `h[w = 6] ||= 1` leaves `w` at `5` (the reference's `IndexWriteWidening`
# quirk). All three fires `for [5]`.
i18a = 5
h18 = {}
h18[i18a = 6] ||= 1
[i18a].frob
i18b = 5
h18[i18b = 6] += 1
[i18b].frob
i18c = 5
h18[i18c = 6] &&= 1
[i18c].frob

# (19) a write in the compound index-write's VALUE position binds —
# fires `for [6]`.
i19 = 5
h19 = {}
h19[:k] ||= (i19 = 6)
[i19].frob
i19b = 5
h19[:k] += (i19b = 6)
[i19b].frob

# --- `begin`/`rescue`/`else`/`ensure` ---------------------------------------

# (20) the primary body joins the entry scope — fires `for [5 | 6]`.
b20 = 5
begin
  b20 = 6
rescue
end
[b20].frob

# (21) a live rescue arm joins too — fires `for [6 | 7]` (the arm's entry
# is the scope BEFORE the body write).
b21 = 5
begin
  b21 = 6
rescue
  b21 = 7
end
[b21].frob

# (22) `ensure` runs after the join — fires `for [8]`.
b22 = 5
begin
  b22 = 6
rescue
  b22 = 7
ensure
  b22 = 8
end
[b22].frob

# (23) an arm that unconditionally EXITS contributes nothing —
# fires `for [6]`.
b23 = 5
begin
  b23 = 6
rescue
  raise "x"
end
[b23].frob

# (24) `else` runs on the primary path — fires `for [7 | 8]`.
b24 = 5
begin
  b24 = 6
rescue
  b24 = 7
else
  b24 = 8
end
[b24].frob

# (25) `begin` as a VALUE unions the primary and live-arm results —
# `6 | 7`, `6` (exiting arm), `7 | 8` (else replaces the primary tail),
# `6` (ensure discarded).
x25a = begin
  6
rescue
  7
end
[x25a].frob
x25b = begin
  6
rescue
  raise "e"
end
[x25b].frob
x25c = begin
  6
rescue
  7
else
  8
end
[x25c].frob
x25d = begin
  6
ensure
  9
end
[x25d].frob

# --- `for` ------------------------------------------------------------------

# (26) a `for` index joins `entry | element` — fires `for [5]`, `for [5 | 6]`.
f26a = 5
for f26a in [5]; end
[f26a].frob
f26b = 5
for f26b in [6]; end
[f26b].frob

# (27) an unbound index takes the element union nil-injected —
# fires `for [1 | 2 | nil]`.
for f27 in [1, 2]; end
[f27].frob

# (28) a literal range's element is the endpoint CLASS nominal —
# fires `for [5 | Integer]`; a non-collection degrades to
# `Dynamic[top]` — `for [5 | Dynamic[top]]`.
f28a = 5
for f28a in (1..3); end
[f28a].frob
f28b = 5
for f28b in 6; end
[f28b].frob

# (29) multi-target decomposes the tuple element — fires `for [1?]`,
# `for [2?]`.
for f29a, f29b in [[1, 2]]; end
[f29a].frob
[f29b].frob

# (30) a `HashShape` eachs `[key, value]` pairs — fires `for [:a?]`,
# `for [1?]`.
for f30a, f30b in { a: 1 }; end
[f30a].frob
[f30b].frob

# (31) a body write joins nil-injected — fires `for [7?]`; a `for *w`
# splat target leaves `w` alone — fires `for [5]`.
for f31 in [1]
  f31 = 7
end
[f31].frob
f31b = 5
for *f31b in [6]; end
[f31b].frob

# --- call arguments and blocks ----------------------------------------------

# (32) a write in a call's receiver or arguments EVALUATES and binds —
# `1.fdiv(c32 = 6)` leaves `c32` at `6` — fires `for [6]`.
c32 = 5
1.fdiv(c32 = 6)
[c32].frob

# (33) a literal block's writes WIDEN, not join — the reference's
# escape classification (`record_closure_escape_if_any`) is unmodelled,
# so even a non-escaping `each`/`tap` block declines (`for
# [Dynamic[top]]` where the reference joins `"s" | 1` — a coverage gap).
# A block-PARAMETER name stays block-scoped either way — `for ["s"]`
# (rigor-rs#166 preserved).
c33 = "s"
[1].each { |e| c33 = 1 }
[c33].frob
c34 = "s"
[1].each { |c34| c34 = 1 }
[c34].frob

# --- inert carriers stay inert (#153 controls) --------------------------------

# (35) `defined?` / `BEGIN` / `super` writes neither bind nor widen —
# all three fire `for [5]`.
q35a = 5
defined?(q35a = 6)
[q35a].frob
q35b = 5
BEGIN { q35b = 6 }
[q35b].frob
q35c = 5
super(q35c = 6)
[q35c].frob

# --- review counterexample controls -------------------------------------------

# (36) a union receiver under `&.` never witnesses — the reference's
# `union_undefined_method_diagnostic` early-returns on safe navigation
# (both rows silent); the scalar `5&.frob` still fires `for 5`.
r36 = "s"
(r36 = 1) rescue nil
r36&.frob
r36b = 5
r36b&.frob

# (37) a project `include`/`prepend` extends a union member's surface —
# `1` gains `K167#k167_added`, so the union stays silent. (A unique
# method name: `frob` here would silence every Integer `.frob` control.)
module K167
  def k167_added
    1
  end
end
class Integer
  include K167
end
r37 = "s"
(r37 = 1) rescue nil
r37.k167_added

# (38) an escaping / unknown block widens, it does not join — all four
# rows silent on the reference.
r38a = "s"
loop { r38a = 1 }
r38a.frob
r38b = "s"
proc { r38b = 1 }
r38b.frob
r38c = "s"
Thread.new { r38c = 1 }
r38c.frob
r38d = "s"
Class.new { r38d = 1 }
r38d.frob

# (39) a `rescue` clause ending in `retry` never falls through — the
# primary body's write dominates, fires `for [6]` / `for 1`.
r39 = 5
begin
  r39 = 6
rescue
  retry
end
[r39].frob
r39b = "s"
begin
  r39b = 1
rescue
  retry
end
r39b.frob

# (40) an EMPTY collection's element is `Dynamic[top]`, not `bot` —
# `for [5 | Dynamic[top]]`; a `for`-first local nil-injects to
# `Dynamic[top]?` (`y.succ` / `y.frob` both stay silent).
r40 = 5
for r40 in []
end
[r40].frob
for y40 in []
end
y40.succ
[y40].frob

# (41) a CONSTANT write's RHS does not rebind locals — `X = (w = 1)`
# leaves `w` at `"s"` — fires `for "s"`.
r41 = "s"
X41 = (r41 = 1)
r41.frob

# (42) a heterogeneous union in a def-body ternary witnesses in the
# reference's `describe(:short)` order — fires `for "s" | 1`.
def m42(c) = (c ? "s" : 1).frob

# (43) a NESTED compound index write drops the receiver-chain index-arg
# write too — `eval_index_or_write` sub-evaluates only `node.value`, so
# `h[a = 3][b = 4] ||= 1` leaves `a` at `1` and `b` at `2` (fires
# `for [1]`, `for [2]`, `for "s"`).
i43a = 1
i43b = 2
h43 = {}
h43[i43a = 3][i43b = 4] ||= 1
[i43a].frob
[i43b].frob
i43c = "s"
h43[i43c = 3][i43d = 4] ||= 1
i43c.frob

# (44) a multi-target `for` distributes a union-of-tuples element
# across the slots (issue #1094): `for a, b in [[6, 7], [8, 9]]` joins
# `6 | 8` into `a` and `7 | 9` into `b`, beside the pre-state —
# fires `for [6 | 8 | 9]` / `for [7 | 8 | 9]`.
f44a = 9
f44b = 8
for f44a, f44b in [[6, 7], [8, 9]]; end
[f44a].frob
[f44b].frob

# (45) `&&` / `||` / `and` / `or` nil-inject their RHS scope into the
# LHS scope (`eval_and_or` -> `join_with_nil_injection`), so a write
# after a rescue modifier or inside the RHS unions rather than widens —
# fires `for [5 | 6 | 7]`, `for [2?]`, `for "s" | 1 | :a`.
l45a = 5
(l45a = 6) rescue nil or (l45a = 7)
[l45a].frob
l45b = 5
((l45b = 6) rescue nil) || (l45b = 7)
[l45b].frob
l45c = 5
((l45c = 6) rescue nil) && (l45c = 7)
[l45c].frob
x45 = 1
x45 && (y45 = 2)
[y45].frob
l45d = "s"
(l45d = 1) rescue nil or (l45d = :a)
l45d.frob

# (46) a rescue MODIFIER arm ending in `retry` is NOT unconditionally
# exiting — `branch_unconditionally_exits?` (statement_evaluator.rb:5027)
# lists `return`/`next`/`break`/`raise`/`throw`/`exit`/`abort`/`fail`,
# never `retry` — so the arm still joins the pre-state: fires
# `for [5 | 6]`; `w.upcase` stays silent on `"s" | 1`. (Contrast (39): a
# `begin`/`rescue` CLAUSE ending in `retry` IS terminating — `retry`
# loops back into the primary body, so the arm contributes nothing.)
r46 = 5
(r46 = 6) rescue retry
[r46].frob
r46b = "s"
(r46b = 1) rescue retry
r46b.upcase

# (47) the union receiver's witness uses `Union#describe` — a
# `true | false` pair collapses to `bool`, rendered FIRST — fires
# `for bool`, `for bool | 1`.
def m47a(c) = (c ? true : false).frob
def m47b(c) = (c ? 1 : (c ? true : false)).frob
def m47c(c) = (c ? "s" : (c ? true : false)).frob

# (48) multi-assign union distribution SOFTENS a name a member binds to bare
# `nil` (join_member_bindings → the reference's optimistic mark): the firm
# join still binds for witnesses (`[v].frob` → `for [1]`) but flow rules
# decline on it — `if v` / `v ?` stay silent. The port stands the mark in as
# `Dynamic[top]` inside the flow-snapshot pass only.
c48 = rand > 0.5
s48, v48 = (c48 ? [:ok, 1] : [:err])
[v48].frob
if v48 then p 1 end
s48b, v48b = (c48 ? [:ok, false] : [:err])
if v48b then 1 end

# (49) a rescue-MODIFIER arm only exits on return/next/break and receiverless
# raise/throw/exit/abort/fail (plus a statements/parens tail) —
# `branch_unconditionally_exits?` reads an `IfNode`'s `subsequent`, which is
# an `ElseNode` it does not unwrap, and a bare `begin`/`retry` are not listed
# at all — so each arm below still joins the pre-state and `upcase` stays
# silent. (`begin`/`rescue` CLAUSES still treat the same shapes as
# terminating via `branch_terminates?`'s bot-type half — pinned at (39).)
r49a = "s"
(r49a = 1) rescue (rand > 0.5 ? raise : raise)
r49a.upcase
r49b = "s"
(r49b = 1) rescue (if rand > 0.5 then raise else raise end)
r49b.upcase
r49c = "s"
(r49c = 1) rescue begin; raise; end
r49c.upcase

# (50) reads INSIDE a rescue-modifier arm (or a `begin`/`rescue` clause) type
# from the ENTRY scope — the reference records the arm's operand types from
# `OperandWalk.type_of(scope, node)` on the entry scope — while the arm's
# WRITES still thread the nil-injected `entry | write` join. Fires `for "s"`,
# `for [Dynamic[top]]`, `for [1 | Dynamic[top]]`, `for 5`.
r50 = "s"
(r50 = 1) rescue r50.frob
r50b = 5
(r50b = "s") rescue 1.fdiv(r50b)
(r50c = 1) rescue [r50c].frob
x50 = ((r50d = 1) rescue r50d)
[x50].frob
r50e = 5
begin
  r50e = "s"
rescue
  r50e.frob
end

# (51) a `begin`/`rescue` clause body starts at the `begin`-ENTRY env but
# THREADS like an ordinary scope — a statement sees the writes earlier
# statements of the SAME clause made, at any nesting depth (`eval_begin`
# clones the entry scope and `eval_statement`s the body through it). Silent
# below: `w` reads the clause's own write, not the `1`/`"s"` entry bindings.
# Fires `for :a` (the clause's later write wins) and `for [5]` (`x = w`
# reads the entry `w`, then `x` threads to the next statement).
w51 = 1
begin
  nil
rescue
  w51 = "s"
  w51.upcase
end
w51b = "s"
begin
  1
rescue
  w51b = 1
  1.fdiv(w51b)
end
w51c = "s"
begin
  w51c = 1
rescue
  w51c = :a
  w51c.frob
end
w51d = 5
begin
  w51d = "s"
rescue
  x51 = w51d
  [x51].frob
end
w51e = 1
begin
  nil
rescue
  (w51e = "s"; w51e.upcase)
end

# (52) `rescue => e` binds the clause's exception class (`StandardError`
# for a bare `rescue`) into the clause scope BEFORE its body threads —
# `e.message` resolves (silent), `e.upcase` fires `for StandardError`,
# and a clause-local write afterwards still threads (`e.frob` sees the
# bound type, not `"s"`).
e52 = 1
begin
  raise
rescue => e52
  e52.message
end
e52b = "s"
begin
  1
rescue => e52b
  e52b.upcase
end

# (53) the reference's `branch_unconditionally_exits?` has no
# `RescueModifierNode` arm but DOES unwrap `ParenthesesNode` in every
# caller: a NESTED rescue modifier as an arm/clause tail still joins its
# writes (`[6 | 7]`), while a multi-statement PARENS tail exits
# (`(1; raise)` — contrast `begin; raise; end` at (49), which does not).
# Fires `for [6 | 7]`, `for 1`, `for [1 | 2]`.
w53 = 5
begin
  w53 = 6
rescue
  (w53 = 7) rescue raise
end
[w53].frob
w53b = "s"
(w53b = 1) rescue (1; raise)
w53b.frob
(w53c = 1) rescue ((w53c = 2) rescue raise)
[w53c].frob

# (54) `ruby_float_to_s` keeps the minus sign in scientific notation —
# fires `for [-1.0e+20]`, `for [-1.0e+15]`, `for [-1.0e-05]`,
# `for [-1.25e-07]`, `for [-1.234567890123456e+15]`.
[-1e20].frob
[-1e15].frob
[-1e-5].frob
[-1.25e-7].frob
[-1234567890123456.0].frob

# (55) a union member with no implicit `to_ary` conversion decomposes as the
# ONE-ELEMENT tuple `[rhs]` (Ruby's own wrap — `multi_target_binder.rb`'s
# `wraps_as_single_element?`), so `a, b = (c ? 1 : [2, 3])` binds `a` to
# `1 | 2` and `b` to `3` (softened), not `Dynamic[top]`. Fires
# `for [1 | 2]`, `for [3]`, `for [3]`, `for [2]`.
c55 = rand > 0.5
a55, b55 = (c55 ? 1 : [2, 3])
[a55].frob
[b55].frob
a55b, (b55b, d55) = (c55 ? [1, [2, 3]] : [4])
[d55].frob
[b55b].frob
