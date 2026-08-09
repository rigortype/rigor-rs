# An UNRESOLVED-constant-receiver binding is a narrowable carrier
# (`Typer::unresolved_constant_root`, 2026-08-09 slice; the gitlab-foss
# `lib/bulk_imports/object_counter.rb:52` census family).
#
# A local bound from a call whose receiver is a constant path that resolves to
# NOTHING — not an RBS class root, not an RBS object constant, not any
# project-defined name — is untyped on BOTH engines, and the reference narrows
# it (`narrow_class_other`). Declining it was pure coverage loss. A RESOLVABLE
# constant receiver still declines: the reference can hand it a precise return
# (`Float::INFINITY.abs`, `ENV.fetch`, an in-source `mk` whose tail is a
# `Logical`) and narrowing there is the carrier-fidelity FP all over again
# (fixture 85, shapes 22+).
#
# Every firing line is oracle-measured (pin v0.3.2 / c6b91b9e, fresh temp cwd,
# --no-cache, both reference libs pinned onto `-I`); every silent control is
# measured silent there.

# --- the allow family: root resolves to nothing, both engines narrow ---------

# (1) a bare unresolved constant receiver.
def unresolved_const(k)
  x = Gitlab.values_from_hash(k)
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (2) an unresolved constant PATH receiver.
def unresolved_path(k)
  x = Gitlab::Cache.values_from_hash(k)
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (3) a leading-`::` unresolved path.
def unresolved_colon(k)
  x = ::Gitlab.values_from_hash(k)
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (4) chained through the unresolved-receiver call.
def unresolved_chained(k)
  x = Gitlab.foo(k).bar
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (5) block-bearing.
def unresolved_block(k)
  x = Gitlab.foo(k) { |a| a }
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (6) safe-navigation.
def unresolved_safenav(k)
  x = Gitlab&.foo(k)
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (7) the census row's real shape: `class << self` nesting, guard pair, use in
# argument position of a merge.
module BulkImports
  class ObjectCounter
    class << self
      def summary(tracker)
        object_counters = Gitlab::Cache::Import::Caching.values_from_hash(counter_key(tracker))

        return unless object_counters.is_a?(Hash)
        return if object_counters.empty?

        empty_response.merge(object_counters.symbolize_keys.transform_values(&:to_i))
      end

      def counter_key(tracker)
        tracker
      end

      def empty_response
        {}
      end
    end
  end
end

# --- silent controls: a RESOLVABLE constant receiver still declines ----------

# (8) an RBS-known ROOT with a precise constant value — the pinned
# carrier-fidelity FP (fixture 85 shape 22). SILENT on both engines.
def resolved_root_declines(k)
  x = Float::INFINITY.abs
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (9) an RBS OBJECT CONSTANT — the reference types `ENV.fetch` `String`.
# SILENT on both engines.
def object_constant_declines(k)
  x = ENV.fetch(k)
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (10) an in-source class whose method's return tail is a `Logical` — the
# fixture-85 `fp2` shape reached through a constant receiver. SILENT on both.
class ZedNine
  def self.mk(k)
    unknown_zzz || {}
  end
end

def insource_logical_declines(k)
  x = ZedNine.mk(k)
  return unless x.is_a?(Hash)

  x.symbolize_keys
end

# (11) a project CONSTANT receiver with a precisely-typed value: the reference
# types `SPEC_FROZEN.dup` off the literal and collapses the disjoint guard.
# SILENT on both engines.
SPEC_FROZEN = [1, 2].freeze

def project_const_declines(k)
  x = SPEC_FROZEN.dup
  return unless x.is_a?(Hash)

  x.symbolize_keys
end
