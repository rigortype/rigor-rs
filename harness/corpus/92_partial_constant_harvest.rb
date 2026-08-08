# Partially-literal constant harvesting.
#
# Slice A (per-FILE constant-value consumption): the reference rebuilds its
# in-source constant-value table per file, so a constant's value is only
# consumed at use sites in the SAME file. This fixture is single-file by
# construction (the harness checks one file at a time), so it pins the
# POSITIVE half — every same-file spelling that must keep folding after the
# gate. The cross-file SILENCE is pinned by the Rust unit tests
# `constant_value_is_consumed_only_in_the_assigning_file`,
# `qualified_constant_value_is_consumed_only_in_the_assigning_file` and
# `toplevel_constant_value_is_still_per_file` in
# `crates/rigor-infer/src/source_index.rs`.

TOPLEVEL_LIST = [1, 2].freeze

class PartialConstHost
  # A toplevel constant read from inside a class body in the SAME file still
  # folds in both engines.
  def toplevel_read
    TOPLEVEL_LIST.frobnicate_zzz
  end
end

module PartialConstNs
  class Inner
    OWN_LIST = %w[a b].freeze
    OWN_HASH = { a: 1 }.freeze

    # Bare read in the defining namespace, same file.
    def bare_read
      OWN_LIST.frobnicate_zzz
    end

    # Fully-qualified spelling of the same constant, same file, same namespace
    # (stage 2e). Both engines resolve it.
    def qualified_read
      PartialConstNs::Inner::OWN_HASH.frobnicate_zzz
    end
  end
end

# --- Slice B: partially-literal containers harvest as INERT bare nominals ----
#
# The reference never declines a partially-literal container: it types the hole
# (`->(_x){…}` as `Proc`) and keeps a full `HashShape` / `Tuple`. rigor-rs mints
# a BARE `Nominal[Hash]` / `Nominal[Array]` (`args: []`) instead. The message
# text therefore diverges (`for Hash` vs `for { c: Proc }`) — the harness keys
# on (rule, line, column) only (`harness/lib.rb#diag_key`), so this is not a
# registrable divergence, and neither is it one for `fp_audit.py`.
#
# Every rigor-rs row below is oracle-matched; the reference additionally fires
# on the element-typed projections rigor-rs is inert for (coverage gaps, listed
# in the harness report — they are the whole point of NOT typing elements).

class PartialContainers
  LAMBDA_HASH = { c: ->(_x) { 1 } }.freeze
  UNFROZEN_LAMBDA_HASH = { c: ->(_x) { 1 } }
  DYNAMIC_ARRAY = [1, unknown_partial_zzz, 2].freeze
  SPLAT_HASH = { a: 1, **unknown_partial_zzz }.freeze
  DYNAMIC_KEY_HASH = { a: 1, unknown_partial_zzz => 2 }.freeze
  SPLAT_ARRAY = [*unknown_partial_zzz].freeze
  INTERPOLATED_ARRAY = ["a", "b#{1}"].freeze
  NESTED = { a: [->() { 1 }] }.freeze

  # The intended surface: the direct receiver. Class-only lookup, so the bare
  # nominal witnesses exactly what the reference's shape does.
  def direct_receivers
    LAMBDA_HASH.frobnicate_zzz
    UNFROZEN_LAMBDA_HASH.frobnicate_zzz
    DYNAMIC_ARRAY.frobnicate_zzz
    SPLAT_HASH.frobnicate_zzz
    DYNAMIC_KEY_HASH.frobnicate_zzz
    SPLAT_ARRAY.frobnicate_zzz
    INTERPOLATED_ARRAY.frobnicate_zzz
    NESTED.frobnicate_zzz
  end

  # Projections. `keys`/`values`/`to_a`/`invert`/`merge`/`transform_values` and
  # the size family resolve through the generic RBS and fire in BOTH engines
  # (the reference just renders a sharper type). The value-pinned projections
  # (`[]`, `fetch`, `first`, block params) are inert here and only the reference
  # fires — deliberately, since typing the elements would out-precise the oracle
  # at exactly the sites it declines (probe z2).
  def projections
    LAMBDA_HASH[:c].frobnicate_zzz
    LAMBDA_HASH.fetch(:c).frobnicate_zzz
    LAMBDA_HASH.keys.frobnicate_zzz
    LAMBDA_HASH.values.frobnicate_zzz
    LAMBDA_HASH.each { |_k, v| v.frobnicate_zzz }
    LAMBDA_HASH.to_a.frobnicate_zzz
    LAMBDA_HASH.invert.frobnicate_zzz
    LAMBDA_HASH.merge(a: 1).frobnicate_zzz
    LAMBDA_HASH.transform_values { |v| v }.frobnicate_zzz
    LAMBDA_HASH.length.frobnicate_zzz
    DYNAMIC_ARRAY[1].frobnicate_zzz
    DYNAMIC_ARRAY.first.frobnicate_zzz
    DYNAMIC_ARRAY.size.frobnicate_zzz
    DYNAMIC_ARRAY.compact.frobnicate_zzz
    NESTED[:a].frobnicate_zzz
  end

  # Arity resolves off the generic RBS in both engines.
  def arity
    LAMBDA_HASH.keys(1, 2)
    DYNAMIC_ARRAY.first(1, 2)
  end

  # possible-nil / always-truthy / argument-type stay silent on a collection
  # carrier in both engines.
  def quiet_surfaces
    LAMBDA_HASH[:absent_zzz].upcase
    DYNAMIC_ARRAY[9].upcase
    "abc".start_with?(LAMBDA_HASH)
    "abc".start_with?(DYNAMIC_ARRAY)
    return 1 if LAMBDA_HASH
    return 2 if DYNAMIC_ARRAY

    3
  end

  # `raise` on a container is parity-positive: a container is never an
  # Exception, so this cannot fire where the reference is silent.
  def raise_container
    raise LAMBDA_HASH
  end
end
