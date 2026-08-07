# Class narrowing, stage 3b-1 — descend the unmodeled STATEMENT forms
# (docs/notes/20260807-narrowing-stage3-spec.md § "3b-1"). A fact established by
# the stage-1/2 machinery (here always `return unless v.is_a?(String)`) used to
# die the moment it reached a statement form the flow pass did not model: the
# pass widened and cleared every fact as its ADR-0038 decline backstop. This
# slice descends those forms instead. It MINTS NOTHING — every arm either
# records a use under a fact that already exists, or is a no-op.
#
# Every positive below was verified against the pinned reference (fresh cwd,
# `--no-cache`, plugin path pinned) before registration; the NEGATIVE CONTROLS
# pin the declines that keep the pass a sound subset — above all the `for` index
# rebind, which is invisible in the arena and where the reference is SILENT.

# --- positives ---------------------------------------------------------------

# (1) recovered op-assign carrier — the archetype (mastodon
# `activitypub/case_transform.rb:18`). `cache[v] ||= …` has no owned arena
# variant: it lowers into a `Statements` carrier whose recovered children are
# the bare reads `cache`, `v` and only THEN the call, so the facts died on the
# READS. FIRES `for String`.
def op_assign_index_carrier(v, cache)
  return unless v.is_a?(String)

  cache[v] ||= v.frobnicate_zzz
end

# (2) a use in the RESCUE-modifier / recovered-carrier family that carries a
# leading bare local read — the same inert-leaf arm as (1), reached through an
# attribute op-assign instead of an index one. FIRES `for String`.
def op_assign_attr_carrier(v, obj)
  return unless v.is_a?(String)

  obj.attr ||= v.frobnicate_yyy
end

# (3) fact SURVIVAL past an ivar write and a hash-index op-assign — neither
# statement can touch a local. FIRES `for String`.
def survives_inert_statements(v, cache)
  return unless v.is_a?(String)

  @seen = 1
  cache[:k] ||= 1
  v.frobnicate_www
end

# (4) literal containers on an assignment RHS: a hash literal nesting an array
# literal is pure descent. FIRES `for String`. (The container arms live in the
# EXPRESSION walker — a bare literal in statement position is still declined.)
def literal_container(v)
  x = nil
  return x unless v.is_a?(String)

  x = { key: [v.frobnicate_vvv] }
  x
end

# (5) `begin`/`rescue`: the use sits in the RESCUE clause body, which the arena
# flattens into the same `body` list (behind the clause's lowered exception
# constant). FIRES `for String`.
def rescue_clause_body(v)
  return unless v.is_a?(String)

  begin
    nil
  rescue StandardError
    v.frobnicate_uuu
  end
end

# (6) loop PREDICATE: evaluated once, before the body, in the enclosing scope.
# FIRES `for String`.
def loop_predicate(v)
  return unless v.is_a?(String)

  while v.frobnicate_ttt
    break
  end
end

# (7) statement-position `&&`: the operands are descended for uses of an OUTER
# fact (the fact is valid on both edges). Establishing a fact FROM the operands
# is stage 3a-2 and is NOT done — see control (12). FIRES `for String`.
def logical_operand_use(v, cond)
  return unless v.is_a?(String)

  cond && v.frobnicate_sss
end

# --- negative controls -------------------------------------------------------

# (8) `for v in …` REBINDS `v`, and the arena drops the index target entirely
# (`ast.rs:1501`) — the reference is SILENT here. `Node::Loop` cannot tell a
# `for` from a `while`, so NO loop body is descended: this control is why.
def for_index_rebind_declines(v, list)
  return unless v.is_a?(String)

  for v in list
    v.frobnicate_zzz
  end
end

# (9) `while` body: the reference FIRES here, rigor-rs declines as collateral of
# control (8) — an accepted coverage gap (stage 3b-2 lands an arena
# discriminator first), never an FP.
def while_body_declines(v, cond)
  return unless v.is_a?(String)

  while cond
    v.frobnicate_zzz
  end
end

# (10) survival PAST a `begin`/`rescue`: the reference keeps the fact, rigor-rs
# clears it — unprobed when the slice was specced, so declined. Coverage gap.
def survival_past_begin_declines(v)
  return unless v.is_a?(String)

  begin
    nil
  rescue StandardError
    nil
  end
  v.frobnicate_zzz
end

# (11) `rescue => v` rebinds the narrowed local with NO `LocalVariableWrite`
# node — invisible to the write machinery. The reference narrows it to the
# EXCEPTION class, so carrying the `String` fact in would be a live FP. The
# bound name is killed explicitly: SILENT on rigor-rs.
def rescue_bound_name_kills(v)
  return unless v.is_a?(String)

  begin
    nil
  rescue StandardError => v
    v.frobnicate_zzz
  end
end

# (12) a `while` PREDICATE mints nothing for the body — SILENT on both engines.
def while_predicate_mints_nothing(v)
  while v.is_a?(String)
    v.frobnicate_zzz
  end
end

# (13) `&&` MINTING stays out of slice (stage 3a-2): the reference fires here,
# rigor-rs declines. Coverage gap, never an FP.
def logical_minting_declines(v)
  v.is_a?(String) && v.frobnicate_zzz
end

# (14) a rebind hiding in the conditionally-executed right operand of a
# statement `&&` kills the fact by span — SILENT on both engines.
def logical_rebind_kills(v, cache, cond)
  return unless v.is_a?(String)

  cond && (v = cache)
  v.frobnicate_zzz
end

# (15) an ivar/gvar/cvar/constant write VALUE is not descended: the reference
# fires here, rigor-rs declines. This is the ONE arm the slice's sweep measured
# as surfacing a pre-existing carrier-fidelity gap — `narrow_class_other`
# narrows Dynamic/Top carriers only, and rigor-rs types a `||` (and any project
# method whose return ends in one) as Dynamic where the reference produces a
# union. Two live sweep FPs sat here. Coverage gap until the carrier side is
# fixed.
def ivar_write_value_declines(v)
  return unless v.is_a?(String)

  @cache = v.frobnicate_zzz
end
