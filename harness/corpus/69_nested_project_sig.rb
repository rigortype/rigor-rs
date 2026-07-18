# ADR-0042 gate (oracle matrix s5/s6): a NESTED project-sig class. The oracle
# witnesses typos through the QUALIFIED path only (`Outer::Inner`), merging
# reopens across sig files, and keeps the bare short name (`Inner`, which
# resolves to nothing at runtime) silent. rigor-rs's short-key registration
# is blind on the qualified path (documented gap = the migration target); the
# bare-name door is pinned SILENT on both engines (the s5 mirror-image FP was
# closed by gating the witness on `knows_toplevel_class`).

module Outer
  class Inner
    def spin
      1
    end

    def brake
      2
    end
  end
end

# Declared across TWO sig files (spin in outer_a.rbs, brake in outer_b.rbs):
# the oracle merges the reopen — both resolve, no fire.
Outer::Inner.new.spin
Outer::Inner.new.brake

# Typo through the qualified path: the oracle fires `for Outer::Inner`.
Outer::Inner.new.spni

# Bare short-name access: silent on BOTH engines (nothing defines toplevel
# `Inner`; the reference never resolves it, and rigor-rs must not either).
Inner.new.spni
