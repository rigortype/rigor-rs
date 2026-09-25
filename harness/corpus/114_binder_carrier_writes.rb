# Writes the local-env binders used to see wrongly (rigor-rs#151, #153 rows 1-3).
#
# - A `for` index REBINDS its local(s) on every iteration (the reference's
#   `bind_for_index`). The lowered loop dropped the index target, so no write
#   collector saw the rebind and the local kept its pre-loop type.
# - A write under `defined?`, `END { }`, `BEGIN { }`, `super(...)` or
#   `yield(...)` never reaches the scope: the reference's statement evaluator
#   has no handler for those nodes and leaves the scope unchanged. The port
#   bound it as a straight-line write.
# - A write inside a `rescue` modifier is CONDITIONAL (the reference joins the
#   after-expression scope with the rescue arm). The port bound it as definite.
#
# The port now carries the `for` index names on `Node::Loop` (widened like a
# body write), marks `defined?` / `END` / `BEGIN` / `super` / `yield` carriers
# inert (no bind, no widen), and widens a write in any other recovery carrier.
#
# One scope: every row uses its own locals. Measured against the pinned
# reference (e59b7b89, fresh cwd, `--no-cache`).

# --- `for` index (#151) -------------------------------------------------------

# (1) the issue row. Reference silent; master fired `for "s"`.
f1 = "s"
for f1 in [1, 2]; end
f1.even?

# (2) a multi-target index rebinds both. Reference silent; master fired twice.
f2a = "s"
f2b = "t"
for f2a, f2b in [[1, 2]]; end
f2a.even?
f2b.even?

# (3) a read inside the body. Reference silent; master fired `for "s"`.
f3 = "s"
for f3 in [1]
  f3.even?
end

# (4) the index constant-folded as nil. Reference silent; master fired
# always-falsey.
f4 = nil
for f4 in [1]; end
if f4
  puts 1
end

# (5) control: a `for` over ANOTHER index still fires `for "s"`.
f5 = "s"
for f5i in [1]; end
f5.even?

# (6) control: a non-local index binds no local; `f6` still fires.
f6 = "s"
for @f6 in [1]; end
f6.even?

# --- inert carriers: the write never reaches the scope (#153) ---------------

# (7) `defined?`. Reference silent; master fired `for 1`.
d7 = "s"
defined?(d7 = 1)
d7.upcase

# (8) `END { }`. Reference silent; master fired `for 1`.
d8 = "s"
END { d8 = 1 }
d8.upcase

# (9) `BEGIN { }`. Reference silent; master fired `for 1`.
d9 = "s"
BEGIN { d9 = 1 }
d9.upcase

# (10) `super(...)`. Reference silent; master fired `for 1`.
d10 = "s"
super(d10 = 1)
d10.upcase

# (11) the constant fold keeps the value from BEFORE the inert write: the
# reference says always-TRUTHY, master said always-falsey (message drift).
d11 = 1
END { d11 = nil }
if d11
  puts 1
end

# (12) same, `defined?`: reference always-FALSEY, master always-truthy.
d12 = nil
defined?(d12 = 1)
if d12
  puts 1
end

# (13) same, `super`: reference always-FALSEY, master always-truthy.
d13 = nil
super(d13 = 1)
if d13
  puts 1
end

# (14) control: the inert write still does not HIDE the earlier type;
# reference and branch fire `for "s"`, master was silent.
d14 = "s"
super(d14 = 1)
d14.even?

# --- conditional carriers: the rescue modifier (#153) -----------------------

# (15) Reference silent; master fired `for 1`.
r15 = "s"
(r15 = 1) rescue nil
r15.upcase

# (16) Reference silent (the join is `nil | 1`); master always-truthy.
r16 = nil
(r16 = 1) rescue nil
if r16
  puts 1
end

# (17) a sequence inside the modifier. Reference silent; master always-truthy.
r17 = nil
(r17 = 1; r17) rescue nil
if r17
  puts 1
end

# --- controls ---------------------------------------------------------------

# (18) a straight-line rebind still binds: fires `for 1`.
k18 = "s"
k18 = 1
k18.upcase

# (19) `defined?` of ANOTHER write leaves `k19` alone: silent on all three.
k19 = "s"
defined?(k19x = 1)
k19.upcase

# --- inside a `def`: the flow passes run there too --------------------------

class Carriers114
  # (20) `for` index: reference silent; master always-falsey.
  def for_index
    w = nil
    for w in [1]; end
    if w
      puts 1
    end
  end

  # (21) rescue modifier: reference silent; master always-truthy.
  def rescue_modifier
    w = nil
    (w = 1) rescue nil
    if w
      puts 1
    end
  end

  # (22) `super` with a block: reference always-falsey; master always-truthy.
  def super_block
    w = nil
    super { w = 1 }
    if w
      puts 1
    end
  end

  # (23) `yield`: reference always-falsey; master always-truthy.
  def yield_arg
    w = nil
    yield(w = 1)
    if w
      puts 1
    end
  end

  # (24) the conversion-reach gate: a conditional or inert write is not a
  # definite assignment. Reference silent; master fired `for Float` on both.
  def reach_rescue(s)
    (s = "x") rescue nil
    Float(s).frob
  end

  def reach_defined(s)
    defined?(s = "x")
    Float(s).frob
  end

  # (25) control: a definite write still fires `for Float`.
  def reach_plain(s)
    s = "x"
    Float(s).frob
  end

  # (26) a `raise` inside a rescue modifier does not end the branch.
  # Reference silent; master fired `for String`.
  def raise_in_modifier(v)
    unless v.is_a?(String)
      raise "x" rescue nil
    end
    v.frob
  end
end
