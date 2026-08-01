# The `Object#`-level conversion-function family — Ruby's `Integer()` / `Array()`
# / `Nokogiri()` idiom, declared in RBS as `def self?.Name:` on `Kernel` or as a
# `class Object` reopen. Two independent mechanisms meet on these names, and this
# fixture pins BOTH against the oracle.
#
# 1. INGESTION. The reference loads `data/vendored_gem_sigs/` unconditionally, so
#    `Object#Nokogiri` is on the oracle's surface. rigor-rs vendors that tree
#    into `crates/rigor-index/vendor/rbs/overlay/`; without it the receiver form
#    is witnessed absent AND the toplevel form raises
#    `call.unresolved-toplevel` — four diagnostics the oracle never emits.
#
# 2. ARITY. The reference's `arity_eligible?` (`check_rules.rb`) refuses to
#    compute an arity envelope for any method with a REQUIRED KEYWORD in any
#    overload. `Kernel#Integer` / `Float` / `Rational` / `Complex` each carry an
#    `(…, exception: bool) -> …` overload, so the oracle never arity-checks them;
#    `Kernel#Array` / `Hash` / `String` carry none, so it does.
#
# See docs/notes/20260801-nokogiri-ingestion-asymmetry-closed.md.

# --- 1. ingestion: present on BOTH surfaces, so neither engine witnesses ------
"abc".Nokogiri("<p/>")
Nokogiri("<p/>")

# --- NEGATIVE CONTROL: an absent capitalized method IS witnessed. Nothing here
# may be blanket-silenced for `Object`-level or capitalized method names.
"abc".Zzzzz("x")

# --- 2. arity: a required-keyword overload disables the envelope --------------
"abc".Integer
"abc".Float
"abc".Rational
"abc".Complex

# --- POSITIVE CONTROL: the same call shape on a sibling conversion function
# with NO required keyword. The eligibility gate is per-method, not a blanket
# retreat from arity-checking the family.
"abc".Array
"abc".Hash
"abc".String
