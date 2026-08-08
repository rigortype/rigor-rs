# Sequential class guards on a stage-3a-3 CHAIN address — the chain half of the
# apply_guards meet (docs/notes/20260809-chain-guard-meet.md).
#
# A chain address is `(root local, no-arg method)`: `h.last.is_a?(C)` narrows
# `h.last`, not `h`. Re-guarding the SAME address runs the identical
# `narrow_nominal_to_class` meet the LOCAL family runs — a disjoint pair (a `||`
# union of disjoint members included) collapses to `Bot` and stays collapsed, a
# SUBCLASS guard refines, a SUPERCLASS guard is a no-op, and `instance_of?`
# collapses on a bare name mismatch before the hierarchy.
#
# Every firing line is oracle-measured (pin v0.3.1, fresh cwd, --no-cache,
# plugin path pinned); every silent control is measured silent there.

# --- the closed FPs: the collapse sticks --------------------------------------

# (1) an `||` union whose every member is disjoint from the carrier. The mint
# gate skipped a 2-class guard entirely, so the stale `String` fact survived and
# witnessed `for String` on master; the reference meets per member to `Bot`.
def or_union_disjoint(h)
  return unless h.last.is_a?(String)
  return unless h.last.is_a?(Hash) || h.last.is_a?(Array)

  h.last.frobnicate_aaa
end

# (2) a THIRD guard cannot revive the collapsed address. The disjoint second
# guard used to REMOVE the fact ("absent" is not `Bot`) and the third re-minted
# `String`; the reference stays silent.
def third_guard_cannot_revive(h)
  return unless h.last.is_a?(String)
  return unless h.last.is_a?(Hash)
  return unless h.last.is_a?(String)

  h.last.frobnicate_bbb
end

# (3) the base disjoint pair (silent before and after this slice).
def disjoint_pair(h)
  return unless h.last.is_a?(String)
  return unless h.last.is_a?(Hash)

  h.last.frobnicate_ccc
end

# --- `instance_of?`: a bare name mismatch collapses before the hierarchy ------

# (4) disjoint names.
def exact_disjoint(h)
  return unless h.last.is_a?(String)
  return unless h.last.instance_of?(Hash)

  h.last.frobnicate_ddd
end

# (5) a SUBCLASS name still collapses — `exact` is tested first.
def exact_subclass(h)
  return unless h.last.is_a?(Numeric)
  return unless h.last.instance_of?(Integer)

  h.last.frobnicate_eee
end

# --- refinement: the reference fires and so must we ---------------------------

# (6) a subclass guard refines to the more specific class (`for Integer`).
def subclass_refines(h)
  return unless h.last.is_a?(Numeric)
  return unless h.last.is_a?(Integer)

  h.last.frobnicate_fff
end

# (7) a superclass guard is a no-op — the carrier stays (`for Integer`).
def superclass_noop(h)
  return unless h.last.is_a?(Integer)
  return unless h.last.is_a?(Numeric)

  h.last.frobnicate_ggg
end

# (8) refinement through a NON-terminating branch edge (`for Integer`).
def branch_refines(h)
  return unless h.last.is_a?(Numeric)

  h.last.frobnicate_hhh if h.last.is_a?(Integer)
end

# (9) an `||` union with a LIVE member: `Bot | String` is the carrier, so the
# reference fires `for String`.
def or_union_mixed(h)
  return unless h.last.is_a?(String)
  return unless h.last.is_a?(Hash) || h.last.is_a?(String)

  h.last.frobnicate_iii
end

# --- must-still-fire controls -------------------------------------------------

# (10) a single guard on the address still mints (`for String`).
def single_guard(h)
  return unless h.last.is_a?(String)

  h.last.frobnicate_jjj
end

# (11) a rebind of the ROOT between the guards resets the address — the second
# guard mints fresh (`for Hash`).
def rebind_root_between(h, other)
  return unless h.last.is_a?(String)

  h = other
  return unless h.last.is_a?(Hash)

  h.last.frobnicate_kkk
end
