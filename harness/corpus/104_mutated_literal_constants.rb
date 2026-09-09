# A literal-shape constant the FILE ITSELF mutates stops folding (upstream #540
# / `fc3b8b42`, shipped in `v0.3.8`).
#
# `ScopeIndexer` recorded `LN_SUPPORTED = [true]` as the closed tuple while a
# sibling method wrote `LN_SUPPORTED[0] = false`, so reads folded through a shape
# the program had already outgrown and the flow rules fired constants against
# working code — rake 13.4.2's `lib/rake/file_utils.rb:116` (`if LN_SUPPORTED[0]`)
# is exactly that, and it was a false positive on the standing sweep.
#
# ONE whole-file census, scope-INSENSITIVE (blocks, method bodies and the top
# level all count; only the lexical class/module prefix is tracked) collects every
# mutation whose receiver is a constant read or constant path: the
# `Index{Or,And,Operator}Write` family, an attribute or index writer call, and a
# send named in `MutationWidening`'s `ARRAY_MUTATORS` / `HASH_MUTATORS`. A BARE
# receiver contributes every lexical-resolution candidate. Matched entries are
# wrapped in `Dynamic`, so reads stay honest without licensing the negative
# rules; unmutated constants (VERSION strings, frozen tables) keep their fold,
# and the scope is SAME-FILE only.
#
# Every firing line and every silent control below is oracle-measured at the
# `v0.3.8` pin (`ffb456b0`), one fresh temp cwd per case, `--no-cache`, both
# reference libs pinned onto `-I` (UPSTREAM.md hazard 1).

# --- STAYS SILENT: the file mutates the constant -----------------------------

# (g2) the rake shape: an `[]=` write in a sibling method.
LN = [true]
def g2_mutate
  LN[0] = false
end
def g2_read
  puts "yes" if LN[0]
end

# (g10) a named in-place mutator from `ARRAY_MUTATORS`.
CLEARED = [1, 2]
def g10_mutate
  CLEARED.clear
end
def g10_read
  puts "two" if CLEARED.size == 2
end

# (g11) the census is scope-insensitive: a mutation inside a top-level BLOCK
# counts just as much as one in a method body.
M = [true]
[1].each { M[0] = false }
def g11_read
  puts "yes" if M[0]
end

# (g12) a QUALIFIED-path mutation (`Outer::T[0] = false`) against the bare read
# inside the module — the census records the full name as written, which is the
# key the write accumulator holds.
module Outer
  T = [true]
  def self.read
    puts "yes" if T[0]
  end
end
Outer::T[0] = false

# (g13) an `||=` INDEX write. Prism spells it `IndexOrWriteNode`, not a call, so
# rigor-rs collects the census during LOWERING (the owned arena has no variant
# for it). Same for `&&=` (g14) and `+=` (g15).
ORW = [true]
def g13_mutate
  ORW[0] ||= false
end
def g13_read
  puts "yes" if ORW[0]
end

# (g14) `&&=`.
ANDW = [true]
def g14_mutate
  ANDW[0] &&= false
end
def g14_read
  puts "yes" if ANDW[0]
end

# (g15) `+=` against a hash value.
OPW = { a: true }
def g15_mutate
  OPW[:a] += 1
end
def g15_read
  puts "yes" if OPW[:a]
end

# (g16) an ATTRIBUTE writer (`C.first = v`) — Prism's `attribute_write?`, a
# mutation whatever the method is called.
ATTR = [true]
def g16_mutate
  ATTR.first = 2
end
def g16_read
  puts "yes" if ATTR[0]
end

# (g17) a mutation in ANOTHER class BODY of the same file still counts (the
# census tracks only the lexical prefix, and a bare name contributes the
# top-level candidate).
J4 = [true]
class J4Holder
  J4[0] = false
end
def g17_read
  puts "yes" if J4[0]
end

# (g18) a class-scoped constant mutated from inside its own class.
class G18Holder
  G18 = [true]
  def self.mutate
    G18[0] = false
  end
  def self.read
    puts "yes" if G18[0]
  end
end

# (g19) a BARE-name mutation inside a class contributes EVERY lexical candidate,
# so it also widens the top-level twin (`G19Holder::G19`, then `G19`).
class G19Holder
  def self.mutate
    G19[0] = false
  end
end
G19 = [true]
def g19_read
  puts "yes" if G19[0]
end

# --- STILL FIRES: the fold survives -----------------------------------------
#
# Each of these is a control a naive "any call on a constant widens it" fix
# would silence.

# (g3) a FROZEN table keeps its fold (`.freeze` is not a mutator).
FROZEN = [true].freeze
def g3_read
  puts "yes" if FROZEN[0]
end

# (g8) an UNTOUCHED constant keeps its fold.
UNTOUCHED = [true]
def g8_read
  puts "yes" if UNTOUCHED[0]
end

# (g20) a NON-mutating call (`each`) is not in either mutator table.
READONLY = [true]
def g20_read
  READONLY.each { |x| x }
  puts "yes" if READONLY[0]
end

# (g21) the mutation of the TOP-LEVEL `SAME` does not reach the module-scoped
# `Sibling::SAME`: a bare-name census entry names `SAME`, and this read resolves
# to `Sibling::SAME`.
module Sibling
  SAME = [true]
  def self.read
    puts "yes" if SAME[0]
  end
end
SAME = [true]
SAME[0] = false

# (g22) a scalar constant is untouched by an unrelated mutation elsewhere.
VERSION_S = "1.0"
def g22_read
  puts VERSION_S.frobnicate_g22
end

# --- BOTH SILENT: pinned, so a later change that starts firing is visible ----

# (g5) `<<` widens, and `.empty?` folds on neither engine anyway.
PUSHED = []
PUSHED << 1
def g5_read
  puts "empty" if PUSHED.empty?
end

# (g7) the `ISPELL_STATUS` shape from the upstream commit message.
H = {}
def g7_mutate
  H[:k] = 1
end
def g7_read
  puts "empty" if H.empty?
end

# --- REGISTERED COVERAGE GAPS (the reference fires, rigor-rs does not) -------

# (g23/g24) upstream's `Dynamic[T]` keeps T's pinned value visible to the `==`
# constant fold, so `WIDENED[k] == <literal>` still folds to `true` there and
# reports always-truthy; rigor-rs's `Type::Dynamic(inner)` is opaque to that
# fold, so the widened read goes quiet. Under-emission, never a false positive —
# and the bare-read spelling (g2 above) is the one the sweep site uses.
EQFOLD = [1]
def g23_mutate
  EQFOLD[0] = 9
end
def g23_read
  puts "one" if EQFOLD[0] == 1
end

HEQFOLD = { a: 1 }
def g24_mutate
  HEQFOLD[:b] = 2
end
def g24_read
  puts "one" if HEQFOLD[:a] == 1
end
