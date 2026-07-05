# call.possible-nil-receiver NEGATIVES — the ADR-0039 shape-tier FP guard.
#
# Expected reference diagnostics: NONE. Both tools stay silent.
#
# Every array here is one the reference types as a `Tuple` (a static shape), NOT
# `Nominal[Array]` — so its `[](Range)` slice is a NON-nil sub-Tuple and no
# possible-nil fires. This is exactly the FP class the ADR-0038 Slice-1 Array
# source hit and that the survey `fp_audit` was BLIND to (real code rarely has the
# pattern). rigor-rs's syntactic Array.new-provenance gate declines all of them:
# small constant `Array.new(≤16)`, array literals, and `.map`/shape-preserving
# results carry no provenance, so the slice source never fires.
def shape_negatives
  small = Array.new(10) { |i| i }   # Array.new(n ≤ 16) ⇒ Tuple in the reference
  s1 = small[0..5]
  s1.size

  literal = [1, 2, 3]               # array literal ⇒ Tuple
  s2 = literal[0..1]
  s2.size

  mapped = [4, 5, 6].map { |x| x }  # shape-preserving method ⇒ Tuple
  s3 = mapped[0..1]
  s3.size

  filled = Array.new(3, 0)          # two-arg small ⇒ Tuple
  s4 = filled[0..1]
  s4.size
end
