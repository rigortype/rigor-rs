# `next` / `break` as branch terminators (stage 3a-1 follow-up,
# docs/notes/20260807-narrowing-stage3-spec.md).
#
# The early-termination propagation (`eval_if:486`/`:495`) carries the surviving
# edge's guard map past a conditional whose other branch cannot fall through.
# `branch_terminates` accepted only `return` and a receiverless `raise`; the
# reference's `branch_unconditionally_exits?`
# (`statement_evaluator.rb:2836`) also accepts `next` and `break` — and it does
# so UNCONDITIONALLY, with no in-block gate and no loop-body special case. That
# is the whole slice: an argument-less `next`/`break` now terminates a branch.
#
# Every FIRING case is oracle-measured against the pinned reference (v0.3.1,
# fresh cwd, `--no-cache`, plugin path pinned); every SILENT control is measured
# silent there. The controls are what makes the block-boundary semantics safe:
# a fact minted behind a `next` lives to the END OF THE BLOCK and no further.

# --- the archetype: a guard skipped with `next` inside a block ----------------

# (1) `next unless` on an outer local (probe p1). The reduced form of
# gitlab-foss `lib/gitlab/sidekiq_config/cron_jobs.rb:58`.
def next_unless_outer_local(value, items)
  items.each do |item|
    next unless value.is_a?(String)

    value.frobnicate_aaa
  end
end

# (2) `break` behaves identically (p2).
def break_unless_outer_local(value, items)
  items.each do |item|
    break unless value.is_a?(String)

    value.frobnicate_bbb
  end
end

# (3) the guard may be on the BLOCK PARAMETER itself (p3).
def next_unless_block_param(items)
  items.each do |item|
    next unless item.is_a?(String)

    item.frobnicate_ccc
  end
end

# (4) `next if !guard` — the `!` swap puts the fact on the edge that survives
# (p11), and a compound predicate reaches it through the 3a-1 analyser (p17).
def next_if_bang_guard(value, items)
  items.each do |item|
    next if !value.is_a?(String)

    value.frobnicate_ddd
  end
end

def next_unless_compound(value, flag, items)
  items.each do |item|
    next unless flag && value.is_a?(String)

    value.frobnicate_eee
  end
end

# (5) the jump only has to be the branch's LAST statement (q6).
def next_after_logging(value, items, logger)
  items.each do |item|
    unless value.is_a?(String)
      logger.warn('skipping')
      next
    end

    value.frobnicate_fff
  end
end

# (6) the block need not iterate at all — the recognition is syntactic on both
# engines (q15/q16/r2/q18 measure `lambda`, `define_method`, `loop`, `times`).
def next_in_lambda(value)
  handler = lambda do |item|
    next unless value.is_a?(String)

    value.frobnicate_ggg
  end
  handler
end

# --- controls: the reference is SILENT, so a recording here is a FALSE POSITIVE

# (7) the use BEFORE the guard is unnarrowed (p4).
def use_before_guard(value, items)
  items.each do |item|
    value.before_zzz
    next unless value.is_a?(String)
  end
end

# (8) `next if guard` terminates the TRUTHY edge, and an atomic class guard
# contributes an EMPTY falsey map — nothing survives (p10).
def next_if_positive_guard(value, items)
  items.each do |item|
    next if value.is_a?(String)

    value.after_zzz
  end
end

# (9) a rebind kills the fact: inside the conditional's span (q3) and after the
# propagated guard (q17).
def rebind_in_guard_span(value, other, items)
  items.each do |item|
    next unless (value = other).is_a?(String)

    value.span_zzz
  end
end

def rebind_after_guard(value, other, items)
  items.each do |item|
    next unless value.is_a?(String)

    value = other
    value.rebound_zzz
  end
end

# (10) a fact minted inside a block NEVER escapes it — not past the block (p9,
# p9b), not out of a NESTED block (p13), and not past an inner `if` (q10).
def fact_dies_at_block_end(value, items)
  items.each do |item|
    next unless value.is_a?(String)
  end

  value.escaped_zzz
end

def fact_dies_at_inner_block_end(value, items, others)
  items.each do |item|
    others.each do |other|
      next unless value.is_a?(String)
    end

    value.inner_escaped_zzz
  end
end

def fact_dies_at_inner_if_end(value, flag, items)
  items.each do |item|
    if flag
      next unless value.is_a?(String)
    end

    value.past_if_zzz
  end
end

# --- declines: the reference FIRES, we stay silent (a strict subset) ----------

# (11) a `while`/`until` BODY is never descended (stage 3b-2), so a `next`
# inside one narrows nothing here.
def next_in_while_body(value, count)
  while count > 0
    next unless value.is_a?(String)

    value.while_zzz
  end
end

# (12) `next`/`break` WITH a value keep the recovered-children carrier and are
# not tagged as jumps.
def next_with_value(value, items)
  items.map do |item|
    next 0 unless value.is_a?(String)

    value.valued_zzz
  end
end

# (13) the reference's exit set also holds `throw`/`exit`/`abort`/`fail` and
# treats `redo` as a `Bot` branch; only `raise` is ported.
def throw_unless_guard(value, items)
  items.each do |item|
    throw :done unless value.is_a?(String)

    value.thrown_zzz
  end
end

# (14) BOTH branches jumping: the reference still propagates the truthy map
# (`eval_if:495` only needs a present then-branch); we require exactly one
# branch to terminate.
def both_branches_break(value, items)
  items.each do |item|
    if value.is_a?(String)
      break
    else
      break
    end

    value.both_zzz
  end
end

# (15) the carrier ALLOW-list (PR #72) still declines per local.
def coarse_carrier_under_next(left, right, items)
  value = left || right
  items.each do |item|
    next unless value.is_a?(String)

    value.coarse_zzz
  end
end
