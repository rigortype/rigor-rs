# C1 (constant-shadow gate): a bare constant read types to the core-RBS
# `Singleton(C)` — so a class-method typo (`Time.current`) is witnessed — UNLESS
# the project itself defines `C` where Ruby's LEXICAL constant lookup would
# resolve the read to the project definition. The pre-C1 gate suppressed on the
# bare NAME project-wide, so ONE nested `module Time` silenced Time witnessing
# everywhere in the batch; C1 makes the gate lexically precise.
#
# Byte-for-byte against the oracle on (rule, line, column). The whole file is
# analyzed at once, reproducing the batch shape in a single fixture.

# --- STAYS SILENT: the nested definition IS lexically visible ---------------

module Gitlab
  module Database
    module Partitioning
      # A project `module Time` nested here shadows the core `Time` for every
      # use LEXICALLY inside `Gitlab::Database::Partitioning::*`.
      module Time
        class BaseStrategy
          def run
            # Resolves to Partitioning::Time (the enclosing project module),
            # NOT core Time — `.current` must NOT witness the core singleton.
            Time.current
          end
        end
      end

      class Manager
        def run
          # Still lexically inside `...::Partitioning`, where `Time` names the
          # nested module — stays silent.
          Time.current
        end
      end
    end

    # --- FIRES: the nested definition is NOT lexically visible here ----------

    class SchemaChecker
      def check
        # `Gitlab::Database::SchemaChecker` does not enclose the nested
        # `Partitioning::Time`, so `Time` is the core class — `.current`
        # (ActiveSupport, absent from core RBS) witnesses on singleton(Time).
        Time.current
      end
    end
  end
end

# A use at TOPLEVEL: the nested module is not visible, so `Time` is core.
Time.current
