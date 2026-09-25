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
