# ADR-0042 (Slice 5): qualified-key routing for the RETURN-lookup family. The
# existence gates were routed in Slices 2-4; the return lookups still opened
# with the short-key map, so every NAMESPACED receiver's return missed to
# Dynamic and the chained typo went unwitnessed (the measured gitlab
# `Digest::SHA256.hexdigest(...).first(N)` family). The superclass links
# resolve namespace-aware over the qualified registry (`Digest::SHA256 <
# Digest::Base < Digest::Class`), and a reference the resolution cannot pin
# down DECLINES — never guesses.

require "digest"

# Singleton return, multi-hop inheritance: `Digest::Class.hexdigest -> String`
# reached from `Digest::SHA256`. The oracle types String and fires the typo.
Digest::SHA256.hexdigest("x").frobnicate_zzz

# The measured gitlab archetype: `Array#first` on the String return.
Digest::SHA256.hexdigest("x").first(7)

# One-hop rung (`Digest::SHA2 < Digest::Class` directly).
Digest::SHA2.hexdigest("x").frobnicate_zzz

# Instance side (`Digest::Instance#hexdigest` through the ABSOLUTE
# `include ::Digest::Instance` on `Digest::Class`): the oracle fires. The
# INDEX resolves this (unit-tested), but the engine's instance tier does not
# yet consult the RBS return family for a source-registry nominal receiver —
# a known coverage gap (missing, never an FP), recorded here on purpose.
Digest::SHA256.new.hexdigest.frobnicate_zzz

# Top-level-name control: short-key routing is untouched and must keep firing.
Time.now.utcc

# NEGATIVE control (the slice's FP risk): the sig declares
# `Digest::MakerZzz.makec: () -> Class` — a return LEAF whose namespace walk
# holds BOTH `Digest::Class` and top-level `Class`. The oracle resolves
# `Digest::Class` (whose instances respond to `update`) and stays SILENT;
# rigor-rs DECLINES the ambiguous leaf (Dynamic) and must stay silent too.
# Guessing top-level `Class` would fire `undefined method 'update'` — an FP.
Digest::MakerZzz.makec.update("payload")

# Qualified project-sig existence witnessing (Slice 4) still fires on a typo.
Digest::MakerZzz.new.makecc
