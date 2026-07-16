# `flow.always-truthy-condition` via the ADR-0038 interprocedural literal-tail
# fold: a predicate that calls a PROJECT method whose whole return provably joins
# to one scalar literal folds to that constant, so the `if`/`unless` fires.
# Byte-for-byte against the oracle on (rule, line, column = the predicate node).
# Fixtures are single-file, so the "cross-file" `Module.method` archetype is
# modelled here as a same-file cross-CLASS call (the fold is keyed by name, so
# same-file and cross-file resolve identically).

module Gitlab
  module Database
    # A bare-literal singleton return.
    def self.read_only?
      false
    end

    # A depth-2 interprocedural fold: the tail `!read_only?` resolves the
    # OWN-CLASS singleton `read_only?` (false) and inverts it -> true.
    def self.read_write?
      !read_only?
    end
  end
end

# A different class calls the module's singletons (the `Gitlab::Database.
# read_only?` archetype). `read_only? -> false` -> always falsey.
class GitAccess
  def check_read
    if Gitlab::Database.read_only?
      puts "read only"
    end
  end

  # `read_write? -> true` -> always truthy.
  def check_write
    if Gitlab::Database.read_write?
      puts "read write"
    end
  end
end

# A same-class IMPLICIT-SELF instance predicate: `flag -> false` -> always falsey.
class Widget
  def flag
    false
  end

  def check
    if flag
      puts "flagged"
    end
  end
end

# An `if`-expression assigned to a local still fires on a folded predicate.
class CountStrategies
  def strategies
    result = if Gitlab::Database.read_write?
      :primary
    else
      :replica
    end
    result
  end
end

# --- negatives (must stay silent) -------------------------------------------

# Cross-owner: `Unrelated.read_only?` where `Unrelated` does NOT define it (only
# `Gitlab::Database` does). Own-class resolution declines -> no diagnostic.
class Unrelated
  def probe
    if Unrelated.read_only?
      puts "never folds"
    end
  end
end

# A defensive-named predicate (`empty?`) is in the skip envelope even though it
# folds -> silent.
class Buffer
  def empty?
    false
  end

  def check
    if empty?
      puts "skip envelope"
    end
  end
end

# A folded predicate INSIDE a block/loop is suppressed (loop/block envelope).
class Looper
  def flag
    false
  end

  def run
    [1, 2].each do |i|
      if flag
        puts i
      end
    end
  end
end
