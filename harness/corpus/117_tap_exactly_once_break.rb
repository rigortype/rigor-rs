# Exactly-once block timing for `Kernel#tap` / `#then` / `#yield_self`
# (rigor-rs#140, upstream rigor#1105).
#
# These three methods invoke a literal block exactly once, immediately, before
# returning — so a block that cannot complete normally makes the callee's
# ordinary return unreachable, and the call types to its `break` arms alone
# (`bot` when there are none). Since #853 the `break` values a literal block
# carries out also JOIN the call's ordinary return when the block can complete.
#
# Consequence for `x = recv.tap { break "s" }`: the call is `"s"`, never the
# receiver — the issue's false positive. A `next` completes the block, so the
# receiver stays. A `break` inside a nested block, `while` body or `&blk`
# block-pass retargets or cannot be proven — the receiver stays too. The
# special behavior keys on the resolved Kernel declaration, not the name.
#
# Every row binds a FRESH local: the toplevel env joins a name's writes
# file-wide, so rebinding `x` would smear one row's answer into another's.
#
# No `def` may appear in this file at all (except the `return` row's own):
# ANY project definition of `tap` / `then` / `yield_self` (or of
# `raise`/`fail`/`throw`/`exit`/`exit!`/`abort`, anywhere, on any class)
# disables the corresponding proof project-wide — `project_redefines_root?` /
# `project_defines_anywhere?`.
#
# Every FIRING row is oracle-measured against the pinned reference from a fresh
# cwd with `--no-cache`; every SILENT row is measured silent there.

# --- arms-only: the block cannot complete normally ----------------------------

# (1) The headline row: `tap { break "s" }` is `"s"`, so `upcase` is valid.
a1 = [1, 2].tap { break "s" }
a1.upcase

# (2) The same row with a String-absent method: `"s".push` is absent. FIRING.
a2 = [1, 2].tap { break "s" }
a2.push 3

# (3) A bare `break` carries `nil`. FIRING.
a3 = [1, 2].tap { break }
a3.upcase

# (4) `raise` never returns, so the call is `bot` — anything is valid on it.
a4 = [1, 2].tap { raise "x" }
a4.upcase

# (5) `then` and `yield_self` are the same catalogue.
a5 = 1.then { break "s" }
a5.upcase
a6 = "a".yield_self { break 1 }
a6.upcase

# (6) A block parameter does not change the proof.
a7 = [1, 2].tap { |v| break "s" }
a7.upcase

# (7) Other non-returning Kernel calls: `throw`, `exit`, `fail`, `abort`,
# `exit!`, and the `Kernel.` / `::Kernel.` spellings — all `bot`.
a8 = [1, 2].tap { throw :done }
a8.upcase
a9 = [1, 2].tap { exit }
a9.upcase
b1 = [1, 2].tap { fail "x" }
b1.upcase
b2 = [1, 2].tap { abort }
b2.upcase
b3 = [1, 2].tap { exit! }
b3.upcase
b4 = [1, 2].tap { Kernel.raise "x" }
b4.upcase
b5 = [1, 2].tap { ::Kernel.raise "x" }
b5.upcase

# (8) `redo` re-runs the block forever — `bot`.
b6 = [1, 2].tap { redo }
b6.upcase

# (9) An `ensure` or a `begin` body that must exit proves it too.
b7 = [1, 2].tap { begin; break "s"; ensure; puts "x"; end }
b7.upcase

# (10) A write before the `break` types the arm through the block's own env.
b8 = [1, 2].tap { y = "s"; break y }
b8.upcase

# (11) `return` out of the block from inside a method body: `bot`.
def tap_return
  b9 = [1, 2].tap { return "s" }
  b9.upcase
end

# --- the union: block can complete, `break` arms join the ordinary return -----

# (12) A conditional break may not run at all — `Array | "s"`, silent either way.
c = ARGV.first
c1 = [1, 2].tap { break "s" if c }
c1.upcase
c1.push 3

# (13) Both branches of an `if` can break — `"s" | 1`.
c2 = [1, 2].tap { if c; break "s"; else; break 1; end }
c2.upcase

# (14) A `next` keeps the block's normal completion reachable — the union,
# not the drop.
c3 = [1, 2].tap { next; break "s" }
c3.upcase

# (15) A `break` under a `rescue` modifier is recovered and unions.
c4 = [1, 2].tap { raise "x" rescue break "s" }
c4.upcase

# --- declines: the receiver (or the unknown) stays ------------------------------

# (16) A block-level `next` completes the block — plain `Array`. FIRING.
d1 = [1, 2].tap { next }
d1.upcase

# (17) `next "s"` completes the block with a value — `tap` still returns
# `self`; the block's value belongs to `then`'s return, not `tap`'s. FIRING.
d2 = [1, 2].tap { next "s" }
d2.upcase

# (18) A `break` inside a NESTED block exits `each`, not `tap`. FIRING.
d3 = [1, 2].tap { [3].each { break "s" } }
d3.upcase

# (19) A `break` inside a `while` body exits the loop, not the block. FIRING.
d4 = [1, 2].tap { while c; break "s"; end }
d4.upcase

# (20) A `&blk` block-pass carries no body to prove — the receiver stays.
blk = ->(v) { }
d5 = [1, 2].tap(&blk)
d5.upcase

# (21) An explicit `self.` receiver is NOT a Kernel-spelled private call in
# the reference's own dispatch — `self.raise` keeps the ordinary result.
d6 = [1, 2].tap { self.raise "x" }
d6.upcase

# (22) An arbitrary receiver's `raise` is not Kernel's either. FIRING.
d7 = [1, 2].tap { c.raise "x" }
d7.upcase

# (23) `break` on a provably dead branch contributes no arm — and a `next` on
# one does not count as reachable either. FIRING on the arm and the branch.
d8 = [1, 2].tap { break "s" if nil }
d8.upcase
d9 = [1, 2].tap { if false; next; end; break "s" }
d9.push 3

# (24) A safe-navigation `tap` declines through the nilable receiver.
e1 = [1, 2]&.tap { break "s" }
e1.upcase

# (25) A `loop` body's `break` exits `loop` — the block still completes.
e2 = [1, 2].tap { loop { break }; break "s" }
e2.upcase

# (26) `tap()` with empty parens carries no Prism ArgumentsNode — the
# reference's `node.arguments` gate sees nil there too, so the proof still
# applies and the call is `"s"`. Only a NON-EMPTY argument list declines.
e3 = [1, 2].tap() { break "s" }
e3.upcase

# --- block parameters hide (and bind over) the enclosing env -----------------

# (27) `|v|` redeclares `v`: the arm is the RECEIVER, not the outer `"s"`.
# `push` on it is valid — a leaked outer binding would fire here.
w1 = "s"
f1 = [1, 2].tap { |w1| break w1 }
f1.push 3

# (28) `|(v, w)|` destructures: the names hide the outer binding but stay
# UNBOUND — the reference's destructure read off a nominal `Array[T]` is
# optimistic (the runtime may pad `nil`), and an optimistic slot never
# witnesses. `push` silent either way — a leaked outer `"s"` would fire.
w2 = "s"
f2 = [1, 2].tap { |(f2v, w2)| break w2 }
f2.push 3

# (29) `|*w|` binds the leftover array — `push` is valid. An outer leak would
# fire here.
w3 = "s"
f3 = [1, 2].tap { |*w3| break w3 }
f3.push 3

# (30) A plain keyword parameter hides the outer name and stays unbound —
# `push` silent on the Dynamic arm. A `&blk` capture binds `Proc` (the
# reference's binder answer for the captured block) — FIRING on `Proc#push`.
w4 = "s"
f4 = [1, 2].tap { |w4:| break w4 }
f4.push 3
w5 = "s"
f5 = [1, 2].tap { |&w5| break w5 }
f5.push 3

# (31) A `|;local|` declaration hides the outer name and binds nothing —
# `break w6` types Dynamic, so `push` declines. The reference leaks the
# outer binding through the `;`-local and fires `for "s"` here; the port's
# silence is the safe side of that oracle leak.
w6 = "s"
f6 = [1, 2].tap { |w6p; w6| break w6 }
f6.push 3

# --- the block's VALUE is not its reachability --------------------------------

# (32) A non-completing PART still types the whole expression: the array's
# value is `Array`, so `tap` keeps its receiver. FIRING.
g1 = [1, 2].tap { [raise("x")] }
g1.upcase

# (33) Same for a call argument — `push(raise "x")` types to `push`'s
# return, so the block's tail value is non-bot. FIRING.
g2 = [1, 2].tap { [1, 2].push(raise "x") }
g2.upcase

# (34) `break "s" if raise "x"` — the `if`'s value joins its missing `else`
# (`nil`), so the block return is non-bot and the arm still unions: the call
# is `"s" | Array`. FIRING on a method every arm lacks.
g3 = [1, 2].tap { break "s" if raise "x" }
g3.frobnicate_zzz

# (35) `raise "x"; break "s"` — the `break` tail IS bot, so the call is the
# arm alone: `"s"`. FIRING on `push`, silent on `upcase`.
g4 = [1, 2].tap { raise "x"; break "s" }
g4.push 3

# (36) A mid-body `next "s"` joins the block's return — the block's value is
# `"s"`, not bot, so `tap` keeps its receiver even between two `raise`s.
# FIRING.
g5 = [1, 2].tap { raise "x"; next "s"; raise "y" }
g5.upcase

# (37) A `next` carrying a `bot` value contributes nothing — `bot` alone.
g6 = [1, 2].tap { next raise "x"; raise "y" }
g6.upcase

# (38) `if c; break "s"; else; break 1; end` — every arm's value is `bot`, so
# the call is the arms alone: `"s" | 1`. FIRING on a method all arms lack.
g7 = [1, 2].tap { if c; break "s"; else; break 1; end }
g7.push 3

# (39) `case`/`when` arms join the same way: all-`break` branches plus an
# `else` give `bot` — the call is the arms alone. FIRING.
g8 = [1, 2].tap { break "a"; case c; when 1 then break 1; else break "s"; end }
g8.push 3

# (40) A `case` on a value-pinned subject with no `else`: the definite-match
# `when` is the value, and `bot` still drops — `"a" | 1`. FIRING.
g9 = [1, 2].tap { break "a"; case 1; when 1 then break 1; end }
g9.push 3

# (41) But a `case` tail alone never satisfies `never_completes_normally?` —
# the syntactic walk has no `CaseNode` — so the union survives. FIRING on a
# method every arm lacks.
h1 = [1, 2].tap { case c; when 1 then break "s"; else break 1; end }
h1.frobnicate_zzz

# (42) `begin`/`else`: the `else` arm contributes the block's value when the
# protected body completes — `begin raise; rescue break; else 1; end` is
# non-bot, so `tap` keeps `Array` in the union. `push` is on it — silent.
h2 = [1, 2].tap { break "a"; begin; raise "x"; rescue; break "b"; else; 1; end }
h2.push 3

# (43) And with every arm `bot` (`else break "s"`), it drops — `"a" | "s" | 1`.
# FIRING on a method all arms lack.
h3 = [1, 2].tap { break "a"; begin; raise "x"; rescue; break 1; else; break "s"; end }
h3.push 3

# --- safe navigation + union receivers ----------------------------------------

# (44) `&.` splits on the receiver NODE, not its type: a literal `nil&.tap`
# folds to `nil` without dispatching (`nil&.tap { break "s" }` is `nil`,
# FIRING on `nil.upcase`), but an INFERRED-exactly-nil receiver keeps the
# plain pipeline (upstream #540/#541 — the nil traces to a wrong uplink) —
# `h4 = nil; h4&.tap { break "s" }` still answers `"s"`, and `h4` itself is
# still `nil`. FIRING on both `for "s"` and `for nil`.
h4z = nil&.tap { break "s" }
h4z.upcase
h4 = nil
h4a = h4&.tap { break "s" }
h4a.frobnicate_zzz
h4.upcase

# (45) A non-nil `&.` receiver still runs the block — `"s"`. FIRING.
h5 = [1, 2]&.tap { break "s" }
h5.push 3

# (46) `break "s"; break 1` — a multi-class union arm: FIRING only where
# EVERY arm lacks the method (`push`), silent on `upcase` (String has it).
h6 = [1, 2].tap { break "s"; break 1 }
h6.push 3
h7 = [1, 2].tap { break "s"; break 1 }
h7.upcase

# (47) A nil-bearing union stays silent — the N3 decision.
h8 = c ? "s" : nil
h8.frobnicate_zzz

# (48) The implicit `it` parameter is the receiver-bound self-arg — `break
# it` is the receiver. FIRING on `Array#upcase`'s absence.
i1 = [1, 2].tap { break it }
i1.upcase

# (49) `**kw` binds the captured keyword `Hash`. FIRING on `Hash#push`.
w7 = "s"
i2 = [1, 2].tap { |**w7| break w7 }
i2.push 3
