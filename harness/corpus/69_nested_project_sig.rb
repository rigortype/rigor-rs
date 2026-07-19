# ADR-0042 (Slice 4): a NESTED project-sig class. The oracle witnesses typos
# through the QUALIFIED path only (`Outer::Inner`), merging reopens across sig
# files, and keeps the bare short name (`Inner`) silent. rigor-rs now MATCHES:
# the qualified registry + `is_qualified_project_sig_class` gate witness
# `Outer::Inner.new.spni` and stay silent on the valid `spin`/`brake` (merged
# across outer_a/outer_b.rbs) AND on bare `Inner` (which resolves to nothing).

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
