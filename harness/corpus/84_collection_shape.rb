# Collection-shape receiver survival (spec
# docs/notes/20260807-collection-shape-slice-spec.md, STAGE 1): a local seeded
# from an array/hash LITERAL keeps a collection carrier across in-place
# mutations, so a use inside a `def` body witnesses `call.undefined-method`
# against Array / Hash. The reference keeps the NOMINAL when it widens a
# mutated literal shape (`MutationWidening#widen_tuple` / `#widen_hash_shape`);
# this fixture pins that survival and, more importantly, its declines.
#
# THE LOAD-BEARING SEMANTIC (cases 6-9): a branch-contained mutation on a
# NOT-yet-widened seed is SILENT in the reference — `Scope#join` leaves a
# `Tuple[] | Array[…]` union and `receiver_descriptor` has no `Type::Union`
# arm, so dispatch declines. Straight-line widening BEFORE the branch is what
# makes the site fire (contrast case 1 with case 8, and case 4 with case 9).
# Mirroring the union here would ship false positives.
#
# Numbering note: 83 is left free for the in-flight class-narrowing stage-3
# branch; this fixture took the next number after it.

# --- positives ---------------------------------------------------------------

# (1) straight-line `<<` widens the seed, so the branch-contained mutation that
# follows joins identically — FIRES `for Array`.
def straight_line_append(c)
  output = []
  output << 'a'
  output << 'b' if c
  output.frobnicate_zzz
end

# (2) a block-contained `<<` REPLACES the outer binding (`widen_after_block`),
# so an unmutated seed is enough — FIRES `for Array`.
def each_block_append(xs)
  output = []
  xs.each do |x|
    output << x
  end
  output.frobnicate_zzz
end

# (2b) the block-contained mutation is itself BRANCH-contained, and the block is
# NESTED. `widen_after_block` is a syntactic walk of the block body against the
# OUTER scope (not a join of the block's evaluated scope), so this still FIRES
# `for Array` — the exact opposite of the same shape in the METHOD body (case 8).
# The gitlab jira-tracker / ddl-lock survey rows have this shape.
def branch_contained_block_append(xs, ys, c)
  output = []
  xs.each do |_x|
    ys.each do |y|
      output << y if c
    end
  end
  output.frobnicate_zzz
end

# (3) `[]=` on a `{}` seed widens to Hash and stays Hash across further index
# assignments — FIRES `for Hash`.
def hash_index_assign(v)
  project = {}
  project[:a] = v
  project[:b] = v
  project.frobnicate_zzz
end

# (4) pre-widened `case`: every clause's join edge already agrees — FIRES
# `for Array`. The contrast with case 9 is the whole slice.
def prewidened_case_append(x)
  output = []
  output << 'a'
  case x
  when 1 then output << 'b'
  when 2 then output << 'c'
  end
  output.frobnicate_zzz
end

# (5) a multi-write seeds each target with its own tuple; mutating one leaves
# the other a bare tuple, and BOTH dispatch as a collection — FIRES twice.
def multi_write_seeds
  a, b = [], []
  a << 1
  a.frobnicate_zzz
  b.frobnicate_yyy
end

# --- negative controls -------------------------------------------------------

# (6) straight-line rebind: SILENT on both tools.
def rebind_kills(x)
  output = []
  output << 'a'
  output = x
  output.frobnicate_zzz
end

# (7) branch rebind: SILENT on both tools (the join sees two carriers).
def branch_rebind_kills(x, c)
  output = []
  output << 'a'
  output = x if c
  output.frobnicate_zzz
end

# (8) LOAD-BEARING: `if`-contained mutation on an UNWIDENED seed — SILENT on
# both tools. Compare case 1.
def unwidened_if_mutation(c)
  output = []
  output << 'a' if c
  output.frobnicate_zzz
end

# (9) LOAD-BEARING: the `case` twin of case 8 — SILENT on both tools. Compare
# case 4.
def unwidened_case_mutation(x)
  output = []
  case x
  when 1 then output << 'a'
  end
  output.frobnicate_zzz
end

# (10) a Dynamic carrier is never MINTED into a collection by a mutation —
# SILENT on both tools. This decline is what keeps rigor-rs out of the
# reference's own runtime-wrong `[]=`-on-a-String rows (spec bucket E).
def dynamic_seed(x)
  output = x
  output << 1
  output.frobnicate_zzz
end

# (11) a rebind INSIDE a block body kills the carrier — SILENT on both tools.
def block_rebind_kills(xs)
  output = []
  output << 'a'
  xs.each { |_x| output = nil }
  output.frobnicate_zzz
end

# (12) op-write (`+=`): the reference folds `Tuple + Tuple` and FIRES; stage 1
# declines op-writes, so this is an accepted COVERAGE GAP, never an FP.
def op_write_gap
  output = []
  output += [1]
  output.frobnicate_zzz
end

# (13) ivar carrier: the reference types `@h` across methods and FIRES; ivar
# flow is bucket B (its own future slice), so this is an accepted COVERAGE GAP.
class Collector
  def initialize
    @h = {}
  end

  def record(v)
    @h[:a] = v
  end

  def use
    @h.frobnicate_zzz
  end
end
