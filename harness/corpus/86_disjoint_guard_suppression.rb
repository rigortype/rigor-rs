# Disjoint-guard suppression — the OPPOSITE-direction carrier divergence
# (docs/notes/20260808-disjoint-guard-suppression.md).
#
# Fixture 85 covers the case where OUR carrier is coarser than the reference's.
# This one is the mirror: our carrier is PRECISE and the reference's is `Bot`.
# `narrow_nominal_to_class` / `narrow_shape_to_class` / `narrow_constant_to_class`
# (`narrowing.rb:2381,2404,2364`) collapse a guarded local whose class is
# DISJOINT from the guard class, and dispatch through `Bot` witnesses nothing —
# so the reference is silent on every call whose receiver is that local, for
# every rule. rigor-rs never narrowed a precise carrier, so `check_call` kept
# firing on the pre-guard type. Master emitted a diagnostic the reference does
# not, violating ADR-0002.
#
# Every SILENT case below is measured silent on the pinned reference (fresh cwd,
# `--no-cache`, plugin path pinned); every FIRING control is measured firing
# there and must keep firing here — silence is the fix in this slice, so the
# controls are the load-bearing half.

# --- silenced: the guard collapses the local to `Bot` -------------------------

# (1) the reported archetype: an Array-literal carrier under `is_a?(Hash)`.
def ternary_form
  h = [1, 2]
  h.is_a?(Hash) ? h.frobnicate_aaa : h
end

# (2) the `if` form, and the modifier form.
def if_form
  h = [1, 2]
  if h.is_a?(Hash)
    h.frobnicate_bbb
  end
end

def modifier_form
  h = { a: 1 }
  h.frobnicate_ccc if h.is_a?(Array)
end

# (3) `kind_of?`, and `instance_of?` — whose `exact:` path collapses on ANY name
# mismatch, a SUPERCLASS included.
def kind_of_form
  h = [1, 2]
  h.frobnicate_ddd if h.kind_of?(Hash)
end

def instance_of_superclass
  h = [1, 2]
  h.frobnicate_eee if h.instance_of?(Enumerable)
end

# (4) `C === local` — the reference routes case-equality through the same
# `class_predicate_scopes`.
def case_equality_form
  h = [1, 2]
  if Hash === h
    h.frobnicate_fff
  end
end

# (5) `case`/`when`, single condition and an ALL-disjoint multi-condition clause.
def case_when_form
  h = [1, 2]
  case h
  when Hash then h.frobnicate_ggg
  else 0
  end
end

def case_when_multi
  h = [1, 2]
  case h
  when Hash, Integer then h.frobnicate_hhh
  else 0
  end
end

# (6) the early-return propagation: the reference carries `Bot` past the guard.
def early_return_form
  h = [1, 2]
  return 0 unless h.is_a?(Hash)

  h.frobnicate_iii
end

# (7) the fact reaches a nested conditional and a nested block body, survives a
# mutator call (a mutation widens a carrier, and `Bot` has none), and survives a
# second guard on the same local.
def nested_reach
  h = [1, 2]
  if h.is_a?(Hash)
    [1].each { h.frobnicate_jjj }
    h.push(3)
    if true
      h.frobnicate_kkk
    end
  end
end

def double_guard
  h = [1, 2]
  if h.is_a?(Enumerable)
    if h.is_a?(Hash)
      h.frobnicate_lll
    end
  end
end

# --- controls: the reference FIRES and so must we -----------------------------

# (8) a NON-disjoint guard — a module the carrier includes. The reference keeps
# the shape and witnesses against it.
def supertype_guard_still_fires
  h = [1, 2]
  h.frobnicate_mmm if h.is_a?(Enumerable)
end

# (9) the guard's own class, and `instance_of?` with the exact class.
def same_class_guard_still_fires
  h = { a: 1 }
  h.frobnicate_nnn if h.is_a?(Hash)
end

def instance_of_exact_still_fires
  h = [1, 2]
  h.frobnicate_ooo if h.instance_of?(Array)
end

# (10) a guard class the core hierarchy cannot RESOLVE. `ClassOrdering::Unknown`
# does not suppress — on a NOMINAL carrier the reference does not collapse
# either, and this is the row that pins the decline.
def unknown_guard_class_still_fires
  h = Array.new
  h.frobnicate_ppp if h.is_a?(UnknownZzzClass)
end

# (11) the FALSEY edge is never narrowed (`narrow_nominal_not_class` preserves a
# disjoint nominal).
def falsey_edge_still_fires
  h = [1, 2]
  unless h.is_a?(Hash)
    h.frobnicate_qqq
  end
end

# (12) a `when` clause whose conditions do NOT all collapse — the reference
# unions the per-condition narrowings, so the Array arm survives.
def mixed_when_still_fires
  h = [1, 2]
  case h
  when Hash, Array then h.frobnicate_rrr
  else 0
  end
end

# (13) a use BEFORE the guard, and after the conditional joins.
def use_before_guard_still_fires
  h = [1, 2]
  h.frobnicate_sss
  0 if h.is_a?(Hash)
end

def use_after_join_still_fires
  h = [1, 2]
  if h.is_a?(Hash)
    0
  end
  h.frobnicate_ttt
end

# (14) a REBIND inside the branch kills the fact.
def rebind_still_fires
  h = [1, 2]
  if h.is_a?(Hash)
    h = [3, 4]
    h.frobnicate_uuu
  end
end

# (15) the suppression is per CALL NODE, not a blanket over the branch: a
# DIFFERENT local, and a call nested in the suppressed call's own arguments,
# both keep firing.
def other_local_in_branch_still_fires
  h = [1, 2]
  g = [3, 4]
  if h.is_a?(Hash)
    h.frobnicate_vvv
    g.frobnicate_www
  end
end

def argument_call_still_fires
  h = [1, 2]
  g = [3, 4]
  h.frobnicate_xxx(g.frobnicate_yyy) if h.is_a?(Hash)
end
