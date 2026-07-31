# A class whose RBS DECLARATION the reference loads but whose DEFINITION it
# cannot build: `RBS::DefinitionBuilder#build_instance` / `#build_singleton`
# raise, `RbsLoader` fail-softs to `nil`, and the dispatcher then degrades EVERY
# call on that class to `Dynamic[Top]`. The oracle is silent on such a class —
# on a real method and on a typo alike — so witnessing absence over rigor-rs's
# (non-colliding, therefore buildable) copy of the same signatures is a false
# positive.
#
# `BigMath` is the measured case: `DEFAULT_LIBRARIES` lists both `bigdecimal`
# and `bigdecimal-math`, `bigdecimal` resolves to the installed GEM's `sig/`
# (which also ships `big_math.rbs`), and the two `module BigMath` declarations
# collide into `DuplicatedMethodDefinitionError`. rigor-rs vendors only the
# `rbs` stdlib copy, so its index builds — hence the divergence.
# See docs/notes/20260731-bigmath-ingestion-asymmetry.md.

require "bigdecimal"
require "bigdecimal/math"
require "bundler"

# --- the class-method typo: fires against a buildable surface, silent here ---
BigMath.frobnicate(1)

# --- the CHAINED shape: `sqrt`'s declared `-> BigDecimal` must not resolve
# either, or the chained call fires "for BigDecimal" where the oracle, having no
# definition to dispatch through, produces Dynamic.
BigMath.sqrt(BigDecimal("2"), 10).frobnicate

# --- the sibling found by the same sweep: the `rbs` gem's own
# `sig/shims/bundler.rbs` collides with the reference's
# `data/vendored_gem_sigs/bundler/`.
Bundler.frobnicate

# --- NEGATIVE CONTROL 1: `Math` is the nearest neighbour in shape — a core
# module whose class methods are `def self?.` too — and it builds cleanly, so
# the typo MUST still be witnessed. Nothing here may be blanket-silenced for
# modules or for math-ish namespaces.
Math.frobnicate

# --- NEGATIVE CONTROL 2: a CHAINED call off a buildable module's class method.
# The fix empties the unbuildable classes' return slots, and this pins that the
# emptying is per-class and not a blanket retreat from singleton return typing:
# `Math.sqrt`'s `-> Float` must still resolve and still carry the chain. (The
# reference renders the receiver as its folded value `1.4142135623730951` where
# rigor-rs prints `Float` — a pre-existing constant-folding difference; parity is
# defined over `(rule, line, column)`, which matches.)
Math.sqrt(2.0).frobnicate
