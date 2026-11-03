# Guards narrow CONSTANT receivers, and `respond_to?(:m)` admits `m`
# (upstream `1be2fd0b`, rigor#1453).
#
# Since `1be2fd0b` a constant read narrows under truthiness, `nil?`, `!`, the
# class guards, `C === x` and `respond_to?` exactly as a local does, keyed by
# its SPELLING (`::File` and `File` are different keys), and `respond_to?(:m)`'s
# truthy edge drops every member whose class RBS knows to lack `m` — a receiver
# with no member left reads `Dynamic[top]`. rigor-rs types a constant receiver
# with no flow, so it skips the call sites the reference's guard narrowed away
# (`guard_suppression.rs`). Every row is oracle-measured at the `v0.4.0` pin.

# (1) the mail sweep row (`file_delivery.rb:28`): silent inside the guard.
if ::File.respond_to?(:makedirs)
  ::File.makedirs "a1"
end

# (2) MUST STILL FIRE: unguarded, and a constant's narrowing does not survive
# a join with a path that never narrowed it.
File.makedirs "a2"
if File.respond_to?(:makedirs)
  puts 1
end
File.makedirs "a3"

# (3) every guard position: modifier, ternary, `&&`, early return, a loop
# entry edge — all silent.
File.makedirs("a4") if File.respond_to?(:makedirs)
x5 = File.respond_to?(:makedirs) ? File.makedirs("a5") : nil
File.respond_to?(:makedirs) && File.makedirs("a6")
def a7
  return unless File.respond_to?(:makedirs)
  File.makedirs("a7")
end
File.makedirs "a8" while File.respond_to?(:makedirs)

# (4) MUST STILL FIRE: the falsey edge keeps the carrier; so does a guard on
# a method the class has, a guard on another constant, and a guard spelled
# `::File` for a `File` read.
unless File.respond_to?(:makedirs)
  File.makedirs("b1")
end
if File.respond_to?(:exist?)
  File.makedirs("b2")
end
if Dir.respond_to?(:makedirs)
  File.makedirs "b3"
end
if ::File.respond_to?(:makedirs)
  File.makedirs "b4"
end

# (5) a value constant under a disjoint class guard, `nil?` and `respond_to?`
# is silent; unguarded it fires.
FOO = "x"
if FOO.respond_to?(:zork)
  FOO.zork
end
if FOO.is_a?(Integer)
  FOO.frob_c2
end
if FOO.nil?
  FOO.frob_c3
end
FOO.zork

# (6) the else arm of `A and B` joins `A`'s falsey edge with `A`'s truthy one;
# the constant keeps its carrier there — MUST STILL FIRE.
if File.respond_to?(:makedirs) and File.respond_to?(:other)
  puts 1
else
  File.makedirs "d1"
end

# (7) a local narrowed by a class guard, then `respond_to?` on a method its
# class lacks: `Dynamic[top]` on the truthy edge (the v0.4.0 retraction of
# `nan?' for Numeric`); the unguarded control still fires.
def e1(old_value)
  if old_value.kind_of? Numeric
    old_value.respond_to?(:nan?) && old_value.nan?
  end
end

def e2(old_value)
  if old_value.kind_of? Numeric
    old_value.nan?
  end
end

# (8) the widened arm poisons the join for such a local: silent after it.
def e3(v)
  if v.is_a?(String)
    if v.respond_to?(:zork)
      puts 1
    end
    v.frob_e3
  end
end
