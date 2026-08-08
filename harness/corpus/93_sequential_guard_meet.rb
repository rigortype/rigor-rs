# Sequential class guards on one local — the apply_guards sequential-guard
# meet (docs/notes/20260808-sequential-guard-meet.md).
#
# Two early-return guards in a row leave the reference's scope carrying the
# FIRST guard's class into the second, whose narrowing is
# `narrow_nominal_to_class` (`narrowing.rb:2381`): a disjoint pair collapses to
# `Bot` (silent — the pre-existing rigor-rs FP this slice closes), a SUBCLASS
# guard refines to the more specific class, a SUPERCLASS guard is a no-op, and
# `instance_of?` collapses on a bare name mismatch before the hierarchy.
#
# Every firing line is oracle-measured (pin v0.3.1, fresh cwd, --no-cache,
# plugin path pinned); every silent control is measured silent there.

# --- the closed FP: disjoint sequential guards are dead code ------------------

# (1) `return unless` pair (reference-silent; witnessed `for Hash` on master).
def disjoint_returns(value)
  return unless value.is_a?(String)
  return unless value.is_a?(Hash)

  value.frobnicate_aaa
end

# (2) the `raise` spelling.
def disjoint_raises(value)
  raise ArgumentError unless value.is_a?(String)
  raise ArgumentError unless value.is_a?(Hash)

  value.frobnicate_bbb
end

# (3) the `next` spelling inside a block.
def disjoint_nexts(items)
  items.each do |item|
    next unless item.is_a?(String)
    next unless item.is_a?(Hash)

    item.frobnicate_ccc
  end
end

# (4) `instance_of?` collapses on a name mismatch even for a SUBCLASS name.
def exact_subclass(value)
  return unless value.is_a?(Numeric)
  return unless value.instance_of?(Integer)

  value.frobnicate_ddd
end

# (5) a use BETWEEN the guards fires; the use after the collapse is dead.
def use_between(value)
  return unless value.is_a?(String)

  value.frobnicate_eee
  return unless value.is_a?(Hash)

  value.frobnicate_fff
end

# --- refinement: the reference fires and so must we ---------------------------

# (6) a subclass guard refines to the more specific class (`for Integer`).
def subclass_refines(value)
  return unless value.is_a?(Numeric)
  return unless value.is_a?(Integer)

  value.frobnicate_ggg
end

# (7) a superclass guard is a no-op — the carrier stays (`for Integer`).
def superclass_noop(value)
  return unless value.is_a?(Integer)
  return unless value.is_a?(Numeric)

  value.frobnicate_hhh
end

# (8) refinement through a NON-terminating branch edge.
def branch_refines(value)
  return unless value.is_a?(Numeric)

  value.frobnicate_iii if value.is_a?(Integer)
end

# (9) the ELSE edge of a disjoint branch keeps the incoming fact.
def else_edge_keeps(value)
  return unless value.is_a?(String)

  if value.is_a?(Hash)
    1
  else
    value.frobnicate_jjj
  end
end

# (10) a rebind between the guards resets the meet — the second guard mints
# fresh (`for Hash`).
def rebind_between(value, other)
  return unless value.is_a?(String)

  value = other
  return unless value.is_a?(Hash)

  value.frobnicate_kkk
end
