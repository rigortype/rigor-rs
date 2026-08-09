# Fact retention across a conditional JOIN — `retain_joined_facts`
# (docs/notes/20260809-join-wipe-retention.md).
#
# `class_flow_if`'s `join_cenv` used to blanket-wipe every `Narrowed` local and
# every chain fact at each conditional merge, so a fact minted by an EARLIER
# statement died at ANY later intervening `if`/`unless`/`case` — terminating or
# not, same local or a different one, in `case` and in expression position too.
# The reference's `Scope#join` keeps a local's type whenever both edges agree on
# it, and a fact established BEFORE the conditional is on both edges by
# construction.
#
# Every firing line is oracle-measured (pin v0.3.2 / c6b91b9e, one fresh temp
# cwd per case, --no-cache, both reference libs pinned onto `-I`); every silent
# control is measured silent there.

# --- retention: the fact survives an intervening conditional ------------------

# (1) a guard on a DIFFERENT local no longer kills the first one's fact.
def guard_on_another_local(value, other)
  return unless value.is_a?(String)
  return unless other.is_a?(Hash)

  value.frobnicate_aaa
end

# (2) a wholly unrelated NON-terminating `if`.
def unrelated_if(value, flag)
  return unless value.is_a?(String)

  if flag
    marker = 1
    marker
  end

  value.frobnicate_bbb
end

# (3) the same with an `else` branch. Prism models the `else` as its own node
# and the arena lowers it to a clause-less `BeginRescue` carrier, whose
# blanket-wipe used to cost the falsey edge every fact it carried.
def unrelated_if_else(value, flag)
  return unless value.is_a?(String)

  if flag
    marker = 1
  else
    marker = 2
  end
  marker

  value.frobnicate_ccc
end

# (4) an intervening conditional whose branches BOTH terminate: the reference
# does not prune the statements after it, and the fact rides through.
def both_branches_terminate(value, flag)
  return unless value.is_a?(String)

  if flag
    return
  else
    return
  end

  value.frobnicate_ddd
end

# (5) EXPRESSION position — a ternary on an assignment RHS. The early-return
# propagation is statement-only, but the retention is not.
def expression_position_ternary(value, flag)
  return unless value.is_a?(String)

  picked = flag ? 1 : 2
  picked

  value.frobnicate_eee
end

# (6) an intervening `case` on an unrelated subject.
def intervening_case(value, subject)
  return unless value.is_a?(String)

  case subject
  when Integer
    marker = 1
    marker
  end

  value.frobnicate_fff
end

# (7) the CHAIN twin: a stage-3a-3 chain address survives the same merge.
def chain_across_if(collection, flag)
  return unless collection.last.is_a?(String)

  if flag
    marker = 1
    marker
  end

  collection.last.frobnicate_ggg
end

# (8) the CENSUS shape, reduced from gitlab-foss
# `lib/bulk_imports/object_counter.rb:52`: a second guard on the SAME local that
# is not a class guard at all (`empty?` contributes no guard map), so the fact
# has nothing to meet against and must simply survive.
def non_narrowing_second_guard(counters)
  return unless counters.is_a?(Hash)
  return if counters.empty?

  counters.frobnicate_hhh
end

# --- FP controls: every one measured reference-SILENT -------------------------

# (9) a REBIND of the target inside one branch. The reference fires a real
# union (`for 1 | String`) off the rebound value; representing that union is a
# separate gap, so this stays silent.
def rebind_in_branch(value, flag)
  return unless value.is_a?(String)

  if flag
    value = 1
  end

  value.frobnicate_iii
end

# (10) a `case`/`in` pattern clause is not descended, so its rebind is invisible
# to the edge evidence — the span kill is what holds the line here.
def case_in_rebinds_target(value, subject)
  return unless value.is_a?(String)

  case subject
  in Integer
    value = 1
  else
    value = 2
  end

  value.frobnicate_jjj
end

# (11) a plain CALL on a chain ROOT inside a branch invalidates the address
# (`invalidate_chain_after_call`). It is not a recorded write, so only the edge
# disagreement catches it.
def chain_root_call_in_branch(collection, flag)
  return unless collection.last.is_a?(String)

  if flag
    collection.size
  end

  collection.last.frobnicate_kkk
end

# (12) the conditional's OWN guard target: a disjoint re-guard AFTER an
# intervening `if` must still reach `Bot`. On master the wiped env let the
# second guard mint `Hash` fresh and witness — a live false positive.
def disjoint_reguard_after_if(value, flag)
  return unless value.is_a?(String)

  if flag
    marker = 1
    marker
  end

  return unless value.is_a?(Hash)

  value.frobnicate_lll
end

# (13) …and the refining twin still fires, `for Integer`.
def refining_reguard_after_if(value, flag)
  return unless value.is_a?(Numeric)

  if flag
    marker = 1
    marker
  end

  return unless value.is_a?(Integer)

  value.frobnicate_mmm
end

# (14) the guard's own `if` with BOTH branches terminating: there is no pre-join
# fact to put back, so the declined propagation is not resurrected.
def own_guard_both_terminate(value)
  if !value.is_a?(String)
    return
  else
    return
  end

  value.frobnicate_nnn
end
