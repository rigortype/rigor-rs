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
