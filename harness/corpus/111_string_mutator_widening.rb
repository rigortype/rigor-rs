# A receiver-mutating String method falsifies the literal a binding was pinned
# to, so a later fold on it must not fire. The reference widens a
# `Constant["ab"]` local to the bare `String` nominal under any of
# `StringMutation::MUTATORS` (`string_mutation.rb`); rigor-rs had no String
# table at all and kept folding — `flow.always-truthy-condition` false
# positives, invisible to the sweep.
#
# Pin `e59b7b89` made it worse in two ways, both retractions on the oracle side:
# `4a6b43f6` grew the table 26 -> 35 (`delete_prefix!`, `delete_suffix!`,
# `encode!`, `scrub!`, `unicode_normalize!`, `setbyte`, `bytesplice`,
# `append_as_bytes`), and the constant-mutation census (`mutating_receiver_of`)
# moved from the Array + Hash tables to `SHAPE_MUTATORS`, so a mutated String
# (or `compare_by_identity`'d Hash) CONSTANT stopped folding too. `495a7458`
# lists `Hash#shift` as a Hash mutator.
#
# Every line below is oracle-measured at the `e59b7b89` pin, one fresh temp cwd,
# `--no-cache`.

# --- STAYS SILENT: the mutation falsified the pinned literal ------------------

# (1) locals — a pre-existing String mutator and two the table gained.
upcased = "ab"
upcased.upcase!
if upcased == "ab"
  1
end

encoded = "ab"
encoded.force_encoding(Encoding::UTF_8)
if encoded == "ab"
  2
end

prefixed = "ab"
prefixed.delete_prefix!("a")
if prefixed == "ab"
  3
end

# (2) a mutation inside a block still reaches the enclosing binding.
suffixed = "ab"
[1].each { suffixed.delete_suffix!("b") }
if suffixed == "ab"
  4
end

# (3) constants — the census now reads `SHAPE_MUTATORS`.
UPCASED = "ab"
UPCASED.upcase!
if UPCASED == "ab"
  5
end

IDENTITY = { a: 1 }
IDENTITY.compare_by_identity
if IDENTITY[:a]
  6
end

# --- FIRES on both sides (must-still-fire controls) --------------------------

# (4) an unmutated constant still folds.
PLAIN = "ab"
if PLAIN == "ab"
  7
end

# (5) a NON-mutating sibling leaves the literal pinned.
kept = "ab"
kept.upcase
if kept == "ab"
  8
end

# (6) a LOCAL hash keeps its present keys through `compare_by_identity`
# (`HashLookupMutation`), so the read still folds — the reason that table is
# not in rigor-rs's local widening set.
lookup = { a: 1 }
lookup.compare_by_identity
if lookup[:a]
  9
end
