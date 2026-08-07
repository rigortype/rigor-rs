# Compound predicate narrowing — `&&` / `||` / `!` and the both-direction
# early-return propagation (stage 3a-1,
# docs/notes/20260807-narrowing-stage3-spec.md).
#
# Stages 1-2 accepted exactly one predicate shape: the WHOLE predicate is
# `local.is_a?(C)`. 3a-1 makes the predicate analyser recursive, a port of the
# reference's `predicate_scopes` dispatch: `analyse_and` (`narrowing.rb:2631`)
# concatenates the truthy edges and JOINS the falsey ones, `analyse_or`
# (`:2640`) does the reverse, and `dispatch_unary_predicate` (`:1555`) SWAPS the
# pair for `!`. Because `!` can put a fact on the falsey edge, that edge is no
# longer categorically unnarrowed, and the termination propagation
# (`eval_if:486`/`:495`) runs in both directions.
#
# Every FIRING case is oracle-measured against the pinned reference (v0.3.1,
# fresh cwd, `--no-cache`, plugin path pinned); every SILENT control is measured
# silent there. The controls carry the slice: a compound predicate is where the
# "we narrow strictly less" argument is easiest to get backwards.

# --- `&&`: one recognised conjunct narrows the TRUTHY edge --------------------

# (1) left conjunct (probe c1a).
def and_left_conjunct(v)
  v.frobnicate_aaa if v.is_a?(String) && v.length > 2
end

# (2) right conjunct (c1b), and a middle one (c1c). The left conjunct must not
# be a NIL TEST: `v.nil?` pins `NilClass` on the truthy edge, the reference
# re-narrows that to `Bot` and stays silent — see control (23).
def and_right_conjunct(v)
  if v.frozen? && v.is_a?(String)
    v.frobnicate_bbb
  end
end

def and_middle_conjunct(v)
  if v.frozen? && v.is_a?(String) && v.length > 2
    v.frobnicate_ccc
  end
end

# (3) the `and` keyword is the same node.
def and_keyword(v)
  if v.is_a?(String) and v.length > 2
    v.frobnicate_ddd
  end
end

# (4) two DIFFERENT locals in one predicate: both narrow, per local.
def and_two_locals(v, w)
  if v.is_a?(String) && w.is_a?(Hash)
    v.frobnicate_eee
    w.frobnicate_fff
  end
end

# --- `!`: the edges swap ------------------------------------------------------

# (5) `if !guard … else USE end` (probe c4d).
def bang_else_edge(v)
  if !v.is_a?(String)
    1
  else
    v.frobnicate_ggg
  end
end

# (6) `unless !guard` falls out of the same swap (c4f), and `not` is the same
# node as `!`.
def unless_bang(v)
  unless !v.is_a?(String)
    v.frobnicate_hhh
  end
end

def not_keyword(v)
  if not v.is_a?(String)
    1
  else
    v.frobnicate_iii
  end
end

# (7) `!` over a whole `&&`: the falsey edge of the negation is the `&&`'s
# truthy edge.
def bang_over_and(v)
  if !(v.is_a?(String) && v.length > 2)
    1
  else
    v.frobnicate_jjj
  end
end

# --- termination propagation, BOTH directions ---------------------------------

# (8) a terminating THEN branch propagates the FALSEY map (probes c4a/c4b/f22) —
# the direction 3a-1 adds.
def return_if_bang(v)
  return if !v.is_a?(String)

  v.frobnicate_kkk
end

def raise_if_bang(v)
  raise "no" if !v.is_a?(String)

  v.frobnicate_lll
end

# (9) …through an `||` whose second disjunct is unrecognised (t_c1d_or).
def return_if_bang_or(v)
  return if !v.is_a?(String) || v.nil?

  v.frobnicate_mmm
end

# (10) a terminating ELSE branch propagates the TRUTHY map, now through a
# compound predicate (c1d).
def return_unless_and(v)
  return unless v.is_a?(String) && v.length > 2

  v.frobnicate_nnn
end

# --- `||`: the truthy edge is a JOIN ------------------------------------------

# (11) the same class on both disjuncts survives the join.
def or_same_class(v)
  if v.is_a?(String) || v.is_a?(String)
    v.frobnicate_ooo
  end
end

# (12) a `||` of two NEGATED guards concatenates on the falsey edge.
def or_falsey_concat(v)
  if !v.is_a?(String) || v.nil?
    1
  else
    v.frobnicate_ppp
  end
end

# --- SILENT controls: the reference narrows NOTHING here ----------------------

# (13) c1g — the falsey edge of a plain `&&` joins to nothing.
def control_and_falsey(v)
  if v.is_a?(String) && v.length > 2
    1
  else
    v.frobnicate_zzz
  end
end

# (14) f12 — an unrecognised disjunct kills the `||` truthy join.
def control_or_unrecognised(v)
  if v.is_a?(String) || v.is_a?(String) || v.nil?
    v.frobnicate_zzz
  end
end

# (15) a same-local `&&` collision on two DISJOINT classes: the reference
# re-narrows `Nominal[String]` to Hash and reaches `Bot`, so the branch is
# silent. The spec's "the right conjunct wins" rule would have fired `for Hash`
# here — this control is why it was corrected to the sequential R3 rule.
def control_same_local_collision(v)
  if v.is_a?(String) && v.is_a?(Hash)
    v.frobnicate_zzz
  end
end

# (16) both branches terminate, so the statements after are unreachable and the
# reference emits nothing there. Propagating either edge's map would be a live
# false positive.
def control_both_terminate(v)
  if !v.is_a?(String)
    return
  else
    return
  end

  v.frobnicate_zzz
end

# (17) the carrier ALLOW-list gate (PR #72) still applies PER LOCAL on the new
# falsey edge: a local bound from a `Logical` is coarse and never narrows.
def control_coarse_carrier(a, b)
  v = a || b
  if !v.is_a?(String)
    1
  else
    v.frobnicate_zzz
  end
end

# (18) a rebind after the propagated guard kills the fact.
def control_rebind_after_guard(v)
  return if !v.is_a?(String)

  v = 1
  v.frobnicate_zzz
end

# --- the disjoint-guard `Bot` composition (PR #73) ----------------------------

# (19) the falsey map carries the guard past the `return`, and the Array carrier
# collapses against Hash — the reference is silent and master was NOT. Its
# must-still-fire twin is (20).
def bot_bang_return
  v = [1, 2]
  return if !v.is_a?(Hash)

  v.frobnicate_zzz
end

# (20) control: the TRUTHY edge of `!guard` carries no fact, so the Array
# carrier survives and the call must still witness.
def bot_control_bang_then
  v = [1, 2]
  if !v.is_a?(Hash)
    v.frobnicate_qqq
  end
end

# (21) an `||` truthy join where BOTH disjuncts collapse is `Bot | Bot`.
def bot_or_both_collapse
  v = [1, 2]
  if v.is_a?(Hash) || v.is_a?(String)
    v.frobnicate_zzz
  end
end

# (22) control: one unrecognised disjunct empties the join, so nothing is
# suppressed and the call still witnesses.
def bot_control_or_cond
  v = [1, 2]
  if v.is_a?(Hash) || v.frozen?
    v.frobnicate_rrr
  end
end

# --- conjunct interference: predicates that pin a class we do not model -------

# (23) a NIL TEST on the same local. The reference's `analyse_nil_predicate`
# pins `NilClass` on the truthy edge, so the following class guard re-narrows
# `NilClass` to `Bot` and the branch is silent. Treating the `&&` operands as
# independent would witness `for String` here — a measured false positive, in
# both conjunct orders.
def control_nil_test_before_guard(v)
  if v.nil? && v.is_a?(String)
    v.frobnicate_zzz
  end
end

def control_nil_test_after_guard(v)
  if v.is_a?(String) && v.nil?
    v.frobnicate_zzz
  end
end

# (24) …but the NEGATED nil test is inert (its `NilClass` fact lands on the edge
# the `&&` does not concatenate), so this idiom keeps narrowing.
def not_nil_then_guard(v)
  if !v.nil? && v.is_a?(String)
    v.frobnicate_sss
  end
end

# (25) `C === local` pins a class the same way, non-mintably.
def control_case_equality_conflict(v)
  if String === v && v.is_a?(Hash)
    v.frobnicate_zzz
  end
end

# (26) a named-capture `=~` binds `v` with no arena-visible write, so the whole
# compound predicate declines — even where the reference agrees with the
# narrowing (a coverage cost paid for an invisible binding).
def control_named_capture_binding(s)
  if /(?<v>a)/ =~ s && v.is_a?(String)
    v.frobnicate_zzz
  end
end

# (27) control: `local =~ /re/` binds nothing and still narrows.
def match_operator_keeps_narrowing(v)
  if v =~ /a/ && v.is_a?(String)
    v.frobnicate_ttt
  end
end
