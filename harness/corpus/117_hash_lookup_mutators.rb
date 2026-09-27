# `HashLookupMutation` (`hash_lookup_mutation.rb`, pin `e59b7b89`): the three
# lookup mutators `Hash#default=` / `Hash#default_proc=` / `Hash#compare_by_identity`
# rewrite a literal-seeded HashShape binding. `default=` / `default_proc=` OPEN
# the shape — declared keys keep their value pins, but a key outside `pairs`
# may hit the configured default, so it reads `untyped` (the motivating row:
# `counts = { a: 1 }; counts.default = 0; counts[:b] + 1` is SILENT, not
# `for nil`). `compare_by_identity` leaves an identity-stable shape alone
# (Symbol / true / false / nil / fixnum keys read exactly as before) and only
# widens one holding identity-sensitive keys (String / heap Float / bignum)
# to `Hash[K, V | untyped]` — the `untyped` arm keeps a literal String-key
# read from folding, since a fresh `"...".` literal may miss by object id.
#
# Every line below is oracle-measured at the pin, one fresh temp cwd,
# `--no-cache`.

# --- STAYS SILENT: opened / identity-widened reads can't fold ----------------

# (1) the motivating row — an opened shape's missing key reads `untyped`.
counts = { a: 1 }
counts.default = 0
counts[:b] + 1

# (2) `default_proc =` opens too — even when assigned `nil`.
dproc = { a: 1 }
dproc.default_proc = nil
dproc[:b] + 1

# (3) every lookup surface on the open shape defers: `dig`, `values_at`,
# `fetch` (a miss declines — Ruby raises KeyError), `key?` (an open shape's
# extras could hold the key), and the `default` reader itself.
opened = { a: 1 }
opened.default = 0
opened.dig(:b).upcase
opened.values_at(:b).first.upcase
opened.fetch(:b).upcase
opened.key?(:b).upcase
opened.default.upcase
opened.default_proc.upcase

# (4) a String key is a fresh object per literal: after `compare_by_identity`
# the read may miss by identity, so the shape degrades to
# `Hash[String, Dynamic[top] | Integer]` and both reads stay silent.
sensitive = { "k" => 1 }
sensitive.compare_by_identity
sensitive["k"].upcase
sensitive["z"].upcase

# (5) identity-sensitive non-String keys (a heap Float) widen the same way.
floated = { 1.5 => "x" }
floated.compare_by_identity
floated[1.5].upcase

# (6) `compare_by_identity` on an OPEN shape holding a sensitive key degrades
# fully to `Hash[untyped, untyped]` — `widen_hash_shape` on an open shape loses
# every bound, so even the declared key's read stops folding.
reopened = { "k" => 1 }
reopened.default = 0
reopened.compare_by_identity
reopened["k"].upcase
reopened[:a].upcase

# (7) a lookup mutation inside a block body applies to the OUTER binding
# (`widen_after_block`), so the missing-key read is untyped.
blocked = { a: 1 }
[1].each { blocked.default = 0 }
blocked[:b] + 1

# (8) a BRANCH-contained mutation may not run: the join of the open and closed
# edges declines, and reads past it stay silent on every key.
branched = { a: 1 }
branched.default = 0 if rand > 0
branched[:a].upcase
branched[:b] + 1

# (9) non-shape receivers are untouched by the mutation model — `default=` on
# a String binding has no HashShape to open, so the binding stays pinned (and
# the call itself witnesses `default='` absent on String, on both engines).
nstr = "s"
nstr.default = 0
nstr.upcase

# --- FIRES on both sides (must-still-fire controls) --------------------------

# (10) declared keys keep their value pins on an open shape.
kept = { a: 1 }
kept.default = 0
kept[:a].upcase
kept.values_at(:a).first.upcase
kept.foo

# (11) `compare_by_identity` on an identity-STABLE shape (Symbol / nil /
# fixnum keys) leaves the closed shape untouched: reads fold as before.
stable = { k: 1 }
stable.compare_by_identity
stable[:k].upcase
stable["k"].upcase

stable2 = { nil => 1, 5 => "x" }
stable2.compare_by_identity
stable2[nil].upcase
stable2[5].frobnicate_zzz

# (12) the String-keyed widening still projects to Hash for dispatch —
# `foo` witnesses on the widened nominal.
swide = { "k" => 1 }
swide.compare_by_identity
swide.foo

# (13) the untouched literal behaves exactly as before: a missing key on a
# CLOSED shape folds `nil`.
plain = { a: 1 }
plain[:b] + 1

# (14) a safe-nav mutator call still opens the binding (`counts&.default = 0`
# runs `open_shape` whenever the receiver is non-nil).
navigated = { a: 1 }
navigated&.default = 0
navigated.foo

# (15) the mutator applies wherever the call sits — every evaluated position
# opens the shape: a write value, a call argument, a block carried inside a
# value expression, an `if`/`case` predicate, an interpolation part, a
# clause-less `begin` body, and `lambda`/`Proc.new` block bodies (the
# syntactic `widen_after_block` walk reaches all of them).
vpos = { a: 1 }
r = (vpos.default = 0)
vpos[:a].upcase
vpos[:b] + 1

apos = { a: 1 }
p(apos.default = 0)
apos[:a].upcase

bpos = { a: 1 }
r2 = [1].each { bpos.default = 0 }
bpos[:a].upcase

ppos = { a: 1 }
if ppos.default = 0
  1
end
ppos[:a].upcase

cpos = { a: 1 }
case (cpos.default = 0)
when 1 then nil
end
cpos[:a].upcase

ipos = { a: 1 }
s = "a#{ipos.default = 0}b"
ipos[:a].upcase

bgn = { a: 1 }
begin
  bgn.default = 0
end
bgn[:a].upcase

lam = { a: 1 }
x = lambda { lam.default = 0 }
lam[:a].upcase

# (16) a literal `-> { }` body is never evaluated by the reference — the
# mutation inside is a NO-OP: the closed shape keeps folding `nil` (it stays
# a no-op even through a later `.call`, and inside a conditional branch).
laz = { a: 1 }
-> { laz.default = 0 }
laz[:b] + 1
laz[:a].upcase

laz2 = { a: 1 }
runner = -> { laz2.default = 0 }
runner.call
laz2[:b] + 1

# (17) structural reachability, not span hulls: a `->` inside heredoc
# interpolation nested in a block body still APPLIES — `widen_after_block`
# reaches it even though the interpolated node sits outside the block's
# byte span on disk.
heredoc = { a: 1 }
[1].each do
  <<~END
  #{-> { heredoc.default = 0 }}
  END
end
heredoc[:a].upcase
heredoc[:b] + 1
heredoc.foo

# (18) `ReceiverAlias` candidates: a mutator on a transparent receiver
# expression applies to EVERY local it may evaluate to — `||`, ternary and
# statement-tail receivers each open both/inner shapes.
alog = { a: 1 }
blog = { b: 2 }
(alog || blog).default = 0
alog[:a].upcase
blog[:b].upcase
alog[:z] + 1
blog[:z] + 1

cond = rand > 0
cter = { a: 1 }
dter = { b: 2 }
(cond ? cter : dter).default = 0
cter[:a].upcase
cter[:z] + 1
dter[:b].upcase

stail = { a: 1 }
(nil; stail).default = 0
stail[:a].upcase
stail[:z] + 1

# (19) nested mutators apply innermost/argument-first (Ruby evaluation
# order): the `compare_by_identity` widens to `Hash[String, Integer|untyped]`
# BEFORE `default=` reopens it, so `nested.foo` witnesses the widened-open
# nominal rather than `Hash[untyped, untyped]`.
nested = { "k" => 1 }
nested.default = nested.compare_by_identity
nested.foo
nested["k"].upcase

# (20) `&->` block-pass is a no-op (the lambda body is never evaluated),
# but `&proc`/`&lambda` blocks DO apply — the reference walks non-lambda
# block bodies.
blkp = { a: 1 }
blkp.tap(&-> { blkp.default = 0 })
blkp[:a].upcase
blkp[:b] + 1

blkl = { a: 1 }
blkl.tap(&lambda { blkl.default = 0 })
blkl[:b] + 1

# (21) a constant write's RHS is evaluated for its own type only: the
# mutation does NOT escape into the surrounding local scope (`eval_constant_write`
# returns the entry scope unchanged), so `cwrite` stays closed.
cwrite = { a: 1 }
CW = (cwrite.default = 0)
cwrite[:a].upcase
cwrite[:b] + 1
cwrite.foo

# (22) a same-statement rebind of the mutated local is seen at dispatch:
# `eval_send` threads scope through receiver → arguments → the call's own
# `dispatch`, so the argument write binds `{ b: "x" }` BEFORE `default=` is
# evaluated — the mutation opens the NEW shape (`foo` witnesses
# `{ b: "x", ... }`, the opened extra read declines).
seqarg = { a: 1 }
seqarg.default = (seqarg = { b: "x" })
seqarg[:a].upcase
seqarg.foo
seqarg[:z] + 1

# (23) the argument-write value is what the predicate sees: after
# `seqnil.default = (seqnil = { a: nil })` the binding holds `{ a: nil }`
# opened, so `seqnil[:a]` folds `nil` — falsey.
seqnil = { a: 1 }
seqnil.default = (seqnil = { a: nil })
if seqnil[:a]
  1
end

# (24) a parenthesized receiver group evaluates before the dispatch, so the
# mutation lands on the rebound `{ b: "x" }` — both later reads stay silent.
precv = { a: 1 }
(precv = { b: "x" }; precv).default = 0
precv[:a].upcase
precv[:z] + 1

# (25) `&&=` / `||=` receivers are writes too — the compound result binds the
# local BEFORE `default=` dispatches on it.
andw = { a: 1 }
(andw &&= { b: "x" }).default = 0
andw[:a].upcase
andw[:z] + 1

ornil = nil
(ornil ||= { a: 1 }).default = 0
ornil[:a].upcase
ornil[:z] + 1

ortruthy = { a: 1 }
(ortruthy ||= { b: 1 }).default = 0
ortruthy.foo
ortruthy[:a].upcase

# (26) argument position matters: a write BEFORE the mutation widens the
# mutation's carrier to `untyped`; a write AFTER it wins the binding (the
# mutation opened a shape the write then replaced, so `argrev[:z]` folds
# `nil`).
def twargs(a, b); end
argord = { a: 1 }
twargs(argord = { b: "x" }, argord.default = 0)
argord[:a].upcase
argord[:z] + 1

argrev = { a: 1 }
twargs(argrev.default = 0, argrev = { b: "x" })
argrev[:z] + 1
argrev[:b].upcase

# (27) a write inside a deferred block poisons the replayed carrier — the
# block may never run, so `blkw` declines on every key.
blkw = { a: 1 }
xs2 = [1]
xs2.each { blkw.default = (blkw = { b: "x" }) }
blkw[:a].upcase
blkw[:z] + 1

# (28) the rest of the constant-write family is typed-only too: const-path,
# `+=`, and `||=` writes evaluate the RHS for its own type without letting the
# mutation reach the surrounding local scope.
cpath = { a: 1 }
OUTER::INNER = (cpath.default = 0)
cpath[:b] + 1
cpath.foo

corw = { a: 1 }
CONW ||= (corw.default = 0)
corw[:b] + 1

copw = { a: 1 }
COPW += (copw.default = 0)
copw[:b] + 1

# (29) a bare `begin … end` receiver is NOT a ReceiverAlias candidate
# (`BEGIN_RESCUE` declines `begin...end`), so the mutation names nothing and
# `bgres` keeps folding as a closed shape.
bgres = { a: 1 }
(begin; bgres; end).default = 0
bgres[:b] + 1
bgres.foo

# (30) a statement-group predicate folds to its TAIL: `(x; y)` has the type of
# `y`, so the opened-shape `default=` call (truthy) keeps
# `flow.always-truthy-condition` firing on `if`.
ifgr = { a: 1 }
if (ifgr = { b: "x" }; ifgr.default = 0)
  1
end
ifgr[:a].upcase

# (31) a `begin … ensure … end`'s value is its MAIN body's tail — the ensure
# statements run for side effects and never supply it, so the receiver types
# `"s"` (silent upcase), `nil` (fires), and the begin's own value `1` on the
# write below.
(begin; "s"; ensure; 1; end).upcase
(begin; nil; ensure; 1; end).foo
(begin; [1]; ensure; nil; end).first.foo

enbind = (begin; 1; ensure; nil; end)
enbind.foo

if begin; nil; ensure; 1; end
  1
end

enres = { a: 1 }
(begin; enres; ensure; nil; end).default = 0
enres[:b] + 1
enres.foo

# (32) compound and multi-write attribute targets widen the receiver through
# the same lookup-mutator path as `h.default = 0` —
# `eval_attribute_compound_write` and `widen_attribute_targets` both reach
# `widen_receiver_aliases`, so each of these opens its shape.
cwor = { a: 1 }
cwor.default ||= 0
cwor[:b] + 1
cwor.foo

cwop = { a: 1 }
cwop.default += 1
cwop[:b] + 1
cwop.foo

mwcall = { a: 1 }
_mw, mwcall.default = 1, 0
mwcall[:b] + 1
mwcall.foo

mwsplat = { a: 1 }
*_rest, mwsplat.default = [1, 0]
mwsplat[:b] + 1
mwsplat.foo

# (33) the compound write's rvalue is typed scope-pure: a rebind or a lookup
# mutation inside it never reaches the surrounding scope — `cmpure` keeps its
# entry binding (opened by the `||=`), and `cmpg` stays closed.
cmpure = { a: 1 }
cmpure.default ||= (cmpure = { b: "x" })
cmpure[:b].upcase

cmpg = { a: 1 }
cmph = { a: 1 }
cmph.default ||= (cmpg.default = 0)
cmpg[:b] + 1
cmph[:b] + 1

# (34) an attribute compound write in a `type_of` position never dispatches
# its writer — a call argument, a receiver operand, an interpolation part, a
# container element and a `return` operand are all typed scope-purely, so the
# shape stays CLOSED and the literal read still folds `nil`; the same write
# as a write's RHS (or a bare statement / predicate) still opens it.
posarg = { a: 1 }
p(posarg.default ||= 0)
posarg[:b] + 1
posarg.foo

posrecv = { a: 1 }
(posrecv.default ||= 0).foo
posrecv[:b] + 1
posrecv.foo

posstr = { a: 1 }
"a#{posstr.default ||= 0}b"
posstr[:b] + 1
posstr.foo

posary = { a: 1 }
_posary = [posary.default ||= 0]
posary[:b] + 1
posary.foo

poswrhs = { a: 1 }
boundw = (poswrhs.default ||= 0)
boundw.foo
poswrhs[:b] + 1
poswrhs.foo

posret = { a: 1 }
return (posret.default ||= 0)
posret[:b] + 1

# (35) a destructure's call-target writers dispatch AFTER the local binds —
# `bound.apply_to` before `widen_attribute_targets` — so `mwbind`'s
# `default=` opens the JUST-BOUND `{ b: 1 }`, and nested / splat targets
# dispatch too.
mwbind = { a: 1 }
mwbind.default, mwbind = 0, { b: 1 }
mwbind[:b] + 1
mwbind.foo

mwnest = { a: 1 }
_m1, (_m2, mwnest.default) = 1, [2, 0]
mwnest[:b] + 1
mwnest.foo

# (36) a conditional-position mutation still JOINS — a read after it reads
# the widened carrier, a read before it keeps the closed shape.
cflag = ARGV[0]
condord = { a: 1 }
_condord = [cflag ? (condord.default = 0) : nil, condord[:b] + 1]

condpre = { a: 1 }
_condpre = [condpre[:b] + 1, (cflag ? condpre.default = 0 : nil)]

# (37) an `if` arm reads the post-predicate env — the predicate's own
# mutation already dispatched — and statements inside an arm see the arm's
# own sequential env (a read BEFORE the arm's mutation still folds).
predr = { a: 1 }
if predr.default ||= 0
  predr[:b] + 1
end

armr = { a: 1 }
if cflag
  armr[:b] + 1
  armr.default = 0
  armr[:c] + 1
end

# (38) a block body's OWN locals are rebindable / mutable through nested
# constructs: the reference's `CapturedLocals.writes` widens a block-local
# write inside a nested block (`blkmut`'s `inner` write opens the enclosing
# block's `h` via `widen_after_block`), while a mutation inside a lambda that
# sits in a body MEMBER position (`f = -> { … }`) is dead —
# `escaping_closure_captures` never applies `HashLookupMutation`.
outer_block = [1]
outer_block.each do
  blkhash = { a: 1 }
  [1].each { blkhash.default = 0 }
  blkhash[:z] + 1

  blkdead = { a: 1 }
  _f = -> { blkdead.default = 0 }
  blkdead[:z] + 1

  blkcmp = { a: 1 }
  _g = -> { blkcmp.default ||= 0 }
  blkcmp[:z] + 1

  blknest = { a: 1 }
  [1].each { _f2 = -> { blknest.default = 0 } }
  blknest[:z] + 1

  blkrebind = nil
  [1].each { blkrebind = 1 }
  blkrebind.foo
end
