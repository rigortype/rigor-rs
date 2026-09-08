# An `is_a?` guard against a class the environment cannot ORDER widens to
# Dynamic.
#
# Upstream #533 item 4 (`70ca7e74`), shipped between `v0.3.4` and `v0.3.8`:
# `narrow_nominal_to_class`'s `:unknown` arm used to keep the old bound, which
# licensed `call.undefined-method` on the branch the guard had just PROVEN
# (concurrent-ruby's `break done.value if done.is_a?(Concurrent::Maybe)` kept
# `Array` and fired on `.value`). It now answers `untyped` — "the guard proved
# membership in a class the engine cannot name, which destroys the old
# knowledge". `:subclass` still keeps the bound, `:superclass` still narrows,
# `:disjoint` is still `Bot`, and `instance_of?` is still `Bot`. Fixture 86 line
# 134 was the fourth of the four `v0.3.4 -> v0.3.8` re-pin false positives.
#
# The consequence that makes `untyped` different from `Bot` — and the reason
# rigor-rs carries a SEPARATE `ClassFact::Widened` rather than reusing `Bot` — is
# the JOIN. `Bot` is the join identity (`Bot ∪ Array = Array`), so a call after
# the `if` still fires; `untyped` ABSORBS (`Dynamic ∪ Array = Dynamic`), so it
# goes silent. Rows b2b and b15b are that pair, measured on both engines.
#
# Every firing line and every silent control is oracle-measured at the `v0.3.8`
# pin (`ffb456b0`), one fresh temp cwd per case, `--no-cache`, both reference
# libs pinned onto `-I` (UPSTREAM.md hazard 1).

class ProjKlass100 < Hash; end

# --- STAYS SILENT: the guard erased the carrier ------------------------------

# (1) the bare truthy edge — fixture 86 line 134's shape.
def b1
  h = Array.new
  h.frobnicate_b1 if h.is_a?(UnknownZzzClass)
end

# (2) the JOIN. The call AFTER the `if` is silent too, because the widened edge
# absorbs the untouched one.
def b2
  h = Array.new
  h.frobnicate_b2a if h.is_a?(UnknownZzzClass)
  h.frobnicate_b2b
end

# (3) the early-return fall-through.
def b3
  h = Array.new
  return unless h.is_a?(UnknownZzzClass)

  h.frobnicate_b3
end

# (4) an `||` union with ONE unorderable member widens the whole edge, even
# though its `Hash` member alone would be `Bot`.
def b4
  h = Array.new
  h.frobnicate_b4 if h.is_a?(UnknownZzzClass) || h.is_a?(Hash)
end

# (5) `case`/`when` and the bare `===` spelling reach the same meet …
def b8
  h = Array.new
  case h
  when UnknownZzzClass then h.frobnicate_b8
  end
end

def b9
  h = Array.new
  h.frobnicate_b9 if UnknownZzzClass === h
end

# … and so does `kind_of?`.
def b12
  h = Array.new
  h.frobnicate_b12 if h.kind_of?(UnknownZzzClass)
end

# (6) a widening out of a `case` survives past the `case`.
def b27
  h = Array.new
  case h
  when UnknownZzzClass then h.frobnicate_b27a
  end
  h.frobnicate_b27b
end

# (7) a MUTATION does not clear the widening — it has no carrier to widen.
def b32
  h = Array.new
  h.frobnicate_b32a if h.is_a?(UnknownZzzClass)
  h.push(1)
  h.frobnicate_b32b
end

# (8) the widening crosses INTO a block.
def b28
  h = Array.new
  return unless h.is_a?(UnknownZzzClass)

  [1].each do |_i|
    h.frobnicate_b28
  end
end

# (9) a PROJECT class the core hierarchy cannot order against the carrier widens
# on the same rule — the reference no longer distinguishes it from an RBS-less
# gem class.
def b22
  h = Array.new
  h.frobnicate_b22 if h.is_a?(ProjKlass100)
end

# (10) a later ORDERABLE guard does not revive the edge.
def b34
  h = Array.new
  h.frobnicate_b34a if h.is_a?(UnknownZzzClass)
  h.frobnicate_b34b if h.is_a?(Enumerable)
  h.frobnicate_b34c
end

# --- MUST STILL FIRE: the arms the widening must not swallow -----------------

# (11) the FALSEY edge is never narrowed (`narrow_nominal_not_class`).
def b5
  h = Array.new
  h.frobnicate_b5 unless h.is_a?(UnknownZzzClass)
end

# (12) a SUBCLASS guard keeps the bound — `Array` IS `Enumerable`, an ordering
# the environment CAN decide. A fix that widened on "guard class we did not
# narrow to" instead of on the ORDERING would silence this.
def b11
  h = Array.new
  h.frobnicate_b11 if h.is_a?(Enumerable)
end

# (13) a SHAPED carrier still collapses to `Bot` (`narrow_shape_to_class` is
# untouched by the re-pin), and `Bot` is the JOIN IDENTITY — so the guarded call
# is silent while the call AFTER the `if` fires. This is the row that separates
# the two facts; reusing `Bot` for the widening, or widening for the shape,
# breaks one half of it.
def b15
  h = [1, 2]
  h.frobnicate_b15a if h.is_a?(UnknownZzzClass)
  h.frobnicate_b15b
end

def b30
  h = [1, 2]
  case h
  when UnknownZzzClass then h.frobnicate_b30a
  end
  h.frobnicate_b30b
end

# (14) `instance_of?` is `Bot` before the ordering is ever consulted, so it does
# NOT widen and its join still fires.
def b13
  h = Array.new
  h.frobnicate_b13a if h.instance_of?(UnknownZzzClass)
  h.frobnicate_b13b
end

# (15) a TERMINATING truthy edge widens only the path that returns; the code
# after the `if` runs on the untouched falsey edge.
def b24
  h = Array.new
  return if h.is_a?(UnknownZzzClass)

  h.frobnicate_b24
end

def b33
  h = Array.new
  if h.is_a?(UnknownZzzClass)
    return
  end
  h.frobnicate_b33
end

# (16) a REASSIGNMENT clears the widening, inside the guarded branch …
def b18
  h = Array.new
  if h.is_a?(UnknownZzzClass)
    h = Array.new
  end
  h.frobnicate_b18
end

# … and after the join.
def b16
  h = Array.new
  h.frobnicate_b16a if h.is_a?(UnknownZzzClass)
  h = Array.new
  h.frobnicate_b16b
end

# (17) a guard INSIDE a block does not escape it.
def b17
  h = Array.new
  [1].each do |_i|
    h.frobnicate_b17a if h.is_a?(UnknownZzzClass)
  end
  h.frobnicate_b17b
end

# (18) a `while` predicate narrows nothing on this port, so nothing escapes it
# either (`b31a` is a recorded coverage gap: the reference fires there).
def b31
  h = Array.new
  while h.is_a?(UnknownZzzClass)
    h.frobnicate_b31a
  end
  h.frobnicate_b31b
end

# (19) a use BEFORE the guard is untouched.
def b26(flag)
  h = Array.new
  h.frobnicate_b26a if flag
  h.frobnicate_b26b if h.is_a?(UnknownZzzClass)
  h.frobnicate_b26c
end

# --- SILENT ON BOTH for a DIFFERENT reason — pinned so a change is visible ----

# (20) `Comparable` is PROVEN disjoint from Array, so this is `Bot`, not a
# widening.
def b10
  h = Array.new
  h.frobnicate_b10 if h.is_a?(Comparable)
end

# (21) a project class guarded against a NON-carrier local: the guard is not
# mintable and the receiver never gains a class to witness against.
def b6
  s = String.new
  s.frobnicate_b6 if s.is_a?(ProjKlass100)
end
