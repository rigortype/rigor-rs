# `is_a?` / `case-when` class narrowing (census mechanism 1, spec
# docs/notes/20260807-class-narrowing-slice-spec.md): a Dynamic local narrowed
# to `Nominal[C]` on the truthy edge of a class guard witnesses
# `call.undefined-method` against C. The NEGATIVE CONTROLS pin the FP-safety
# declines: a rebind invalidates, the falsey edge / post-`if` code is never
# narrowed without a terminating opposite branch, facts do not enter block
# bodies, and a multi-condition `when` declines (union narrowing is a
# follow-up).

# --- positives ---------------------------------------------------------------

# (1) `if` guard: FIRES `for Hash` inside the branch.
def narrow_if(value)
  if value.is_a?(Hash)
    value.frobnicate_zzz
  end
end

# (2) ternary: FIRES `for Hash` on the truthy arm; the falsey arm is untouched.
def narrow_ternary(rule)
  rule.is_a?(Hash) ? rule.frobnicate_zzz : rule
end

# (3) `unless` guard with a terminating branch: the truthy edge propagates past
# the conditional (early-return narrowing) — FIRES `for Hash`.
def narrow_unless_guard(value)
  return unless value.is_a?(Hash)

  value.frobnicate_zzz
end

# (4) `case`/`when` single-constant clauses: FIRES per clause, `for Hash` /
# `for String`.
def narrow_case(value)
  case value
  when Hash
    value.frobnicate_zzz
  when String
    value.frobnicate_yyy
  else
    value
  end
end

# --- negative controls -------------------------------------------------------

# (5) rebind-in-branch (from a param, keeping the fixture single-diagnostic):
# SILENT — rebinding invalidates the narrowing on both tools.
def rebind_in_branch(value, other)
  if value.is_a?(Hash)
    value = other
    value.frobnicate_zzz
  end
end

# (6) use AFTER the `if` without a terminating opposite branch: SILENT on both
# tools (the narrowing is branch-scoped).
def use_after_if(value)
  if value.is_a?(Hash)
    value
  end
  value.frobnicate_zzz
end

# (7) block-body use: rigor-rs declines (facts do not enter block bodies,
# ADR-0038 block-scope discipline) — an accepted coverage gap, never an FP.
def block_body_use(value)
  if value.is_a?(Hash)
    [1].each { |_i| value.frobnicate_zzz }
  end
end

# (8) multi-condition `when Hash, String`: rigor-rs declines (union narrowing
# is a follow-up) — an accepted coverage gap, never an FP.
def multi_condition_when(value)
  case value
  when Hash, String
    value.frobnicate_zzz
  else
    value
  end
end
