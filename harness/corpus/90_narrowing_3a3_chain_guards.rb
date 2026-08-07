# Single-hop chain guards, LOCAL roots (stage 3a-3,
# docs/notes/20260807-narrowing-stage3-spec.md).
#
# Stages 1-3a-1 narrow a BARE LOCAL under an `is_a?`/`kind_of?`/`instance_of?`
# guard. This slice adds the reference's second narrowing address: a stable
# single-hop chain `root.m` (`analyse_class_predicate_on_chain`,
# `narrowing.rb:1805` + `stable_chain_address`, `:1826`), keyed `(root local,
# method)` and consumed by a later read of the SAME address
# (`method_chain_narrowing_for`, `expression_typer.rb:1062`).
#
# Its invalidation is a STRICT SUPERSET of the reference's: the reference drops
# a chain narrowing only on a rebind of the root or a call whose RECEIVER is the
# root (`invalidate_chain_after_call`, `indexed_narrowing.rb:151`), while this
# slice also drops it on ANY mention of the root, at every point the per-local
# facts clear, and across a block boundary in either direction. Every one of
# those extra drops costs coverage the reference has and can never be an FP.
#
# Every FIRING line below is oracle-measured against the pinned reference
# (v0.3.1, fresh temp cwd, `--no-cache`, plugin path pinned); every SILENT
# control is measured silent there too.

# --- FIRING: the address is minted and read ----------------------------------

# (1) The archetype (probe c7a): an if-modifier guard on `h.last`, and the same
# address read as the receiver of the use.
def chain_if_modifier(h)
  h.last.frobnicate_aaa if h.last.is_a?(String)
end

# (2) The early-return spelling (probe h1) — the shape of dependabot-core
# `bundler/helpers/v2/lib/functions/version_resolver.rb:136`.
def chain_return_unless(h)
  return unless h.last.is_a?(String)

  h.last.frobnicate_bbb
end

# (3) The fact SURVIVES its own re-read (probe f11): the reference narrows both.
def chain_read_twice(h)
  return unless h.last.is_a?(String)

  h.last.frobnicate_ccc
  h.last.frobnicate_ddd
end

# (4) The chain guard as the RIGHT conjunct of an `elsif` (probe a_conj_elsif) —
# the shape of dependabot-core `bundler/helpers/v2/lib/functions/
# lockfile_updater.rb:241`.
def chain_elsif_conjunct(h, cond, other)
  if other
    1
  elsif cond && h.last.is_a?(String)
    h.last.frobnicate_eee
  end
end

# (5) `instance_of?` on the address, consumed on an index-write RHS (probe
# w2) — the shape of dependabot-core `bundler/helpers/v2/lib/functions/
# version_resolver.rb:48`.
def chain_instance_of_index_write(h)
  details = {}
  details[:sha] = h.last.frobnicate_fff if h.last.instance_of?(String)
  details
end

# (6) `kind_of?` is the same guard family.
def chain_kind_of(h)
  h.last.frobnicate_ggg if h.last.kind_of?(String)
end

# (7) The `!` swap puts the fact on the FALSEY edge (probe b_bang_else).
def chain_bang_else(h)
  if !h.last.is_a?(String)
    1
  else
    h.last.frobnicate_hhh
  end
end

# (8) An inert statement between the guard and the use does not disturb the
# address (probe c7h).
def chain_inert_between(h)
  return unless h.last.is_a?(String)

  x = 1
  h.last.frobnicate_iii + x.to_s
end

# (9) A call whose receiver is the ADDRESS — not the root — does NOT invalidate,
# on either engine (probe n_call_on_address).
def chain_call_on_address(h)
  return unless h.last.is_a?(String)

  h.last.strip
  h.last.frobnicate_jjj
end

# (10) Two distinct addresses off ONE root are independent (probe
# x_two_addresses_one_root).
def chain_two_addresses(h)
  if h.first.is_a?(String) && h.last.is_a?(Hash)
    h.first.frobnicate_kkk
  end
end

# (11) The outer call's own ARGUMENTS do not matter: the reference narrows the
# receiver EXPRESSION, not its caller (probe m_use_with_args).
def chain_use_with_args(h)
  return unless h.last.is_a?(String)

  h.last.frobnicate_lll(1)
end

# (12) A `||`-bound ROOT still narrows. The PR #72 carrier ALLOW-LIST is a
# per-LOCAL rule; the chain carrier is the DISPATCH RESULT off that union, which
# the reference narrows normally (probe k_root_or_union).
def chain_union_root(a, b)
  h = a || b
  h.last.frobnicate_mmm if h.last.is_a?(String)
end

# --- SILENT CONTROLS: the reference emits NOTHING here -----------------------

# (13) A call whose RECEIVER is the root invalidates the address (probe c7d).
# Recording here would be a live false positive.
def control_root_receiver_call(h)
  if h.last.is_a?(String)
    h.pop
    h.last.frobnicate_nnn
  end
end

# (14) A REBIND of the root (probe c7g). The reference DOES emit here, but a
# DIFFERENT diagnostic — it folds the rebound `[].last` and says `for nil`, not
# `for String` — so the write-kill must make this slice silent. The reference's
# own hit is a pre-existing coverage gap, unrelated to chains.
def control_root_rebind(h)
  if h.last.is_a?(String)
    h = []
    h.last.frobnicate_ooo
  end
end

# (15) Arguments on the HOP destroy the stable address, on both engines
# (probe c7e).
def control_args_on_hop(h)
  h.fetch(0).frobnicate_ppp if h.fetch(0).is_a?(String)
end

# (16) The `&&` FALSEY edge of an atomic chain guard is EMPTY (probe
# a_conj_else_ctl) — the reference narrows nothing in the `else`.
def control_and_falsey_edge(h, cond)
  if cond && h.last.is_a?(String)
    1
  else
    h.last.frobnicate_qqq
  end
end

# (17) A minted chain fact does NOT escape its branch (probe n_escape_after_if).
def control_no_escape_after_if(h)
  if h.last.is_a?(String)
    1
  end
  h.last.frobnicate_rrr
end

# (18) A sequential DISJOINT re-guard of one address is reference-SILENT: its
# scope carries `String` into the second guard and collapses to `Bot`. Minting
# `Hash` against an emptied env would be a live false positive — the pre-join
# re-seed in `class_flow_if` is what keeps this silent (probe
# d_seq_two_returns_disjoint).
def control_sequential_disjoint(h)
  return unless h.last.is_a?(String)
  return unless h.last.is_a?(Hash)

  h.last.frobnicate_sss
end

# (19) A PRECISE chain carrier: the reference collapses `h.last` (`Integer`)
# under a `String` guard and is silent. The Dynamic/Top carrier gate reaches the
# same silence by declining the mint (probe c_bot_precise_root).
def control_precise_carrier
  h = [1, 2]
  h.last.frobnicate_ttt if h.last.is_a?(String)
end

# (20) A two-hop chain is out of the single-hop envelope on both engines
# (probe m_two_hop).
def control_two_hop(h)
  h.first.last.frobnicate_uuu if h.first.last.is_a?(String)
end

# (21) The use BEFORE the guard is never narrowed (probe n_use_before_guard).
def control_use_before_guard(h)
  h.last.frobnicate_vvv
  return unless h.last.is_a?(String)

  nil
end
