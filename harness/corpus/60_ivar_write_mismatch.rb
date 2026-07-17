# `def.ivar-write-mismatch` fires when one class's instance methods assign the
# SAME instance variable two DIFFERENT concrete classes. Byte-for-byte against
# the oracle on (rule, line, column) — anchored on the offending write's `@x`
# name token. Covers both increments: (a) rescue-bound exception typing and
# (b) the `Integer()`/`Float()`/`String()` NOMINAL fold.

# --- FIRES ----------------------------------------------------------------

# String then Integer.
class StringThenInt
  def build
    @x = "hello"
    @x = 42
  end
end

# bool (true) then String — TrueClass/FalseClass fold to "bool", so a real
# bool→String drift still trips.
class BoolThenString
  def build
    @x = true
    @x = "s"
  end
end

# A leading `@x = nil` placeholder is skipped; the String canonical then fires
# against the Integer write.
class LeadingNilThenConflict
  def build
    @x = nil
    @x = "s"
    @x = 5
  end
end

# Increment (a): `rescue StandardError => error` binds `error` to StandardError.
class RescueSingleClass
  def run
    @error = "boom"
  rescue StandardError => error
    @error = error
  end
end

# Increment (a): a bare `rescue => error` binds `error` to StandardError.
class RescueBare
  def run
    @error = "boom"
  rescue => error
    @error = error
  end
end

# Increment (b): `Float(non_constant)` types Float; the rescue write is Integer.
class KernelFloatConversion
  def initialize(kwargs)
    @upload_duration = Float(kwargs[:upload_duration])
  rescue ArgumentError, TypeError
    @upload_duration = 0
  end
end

# A module's instance method counts just like a class's.
module ModuleMismatch
  def build
    @x = "s"
    @x = 5
  end
end

# A class reopened in the SAME file merges into one group; the second body's
# write conflicts with the first.
class Reopened
  def a
    @x = "s"
  end
end

class Reopened
  def b
    @x = 5
  end
end

# --- STAYS SILENT ---------------------------------------------------------

# The boolean-flag idiom: false then true, both "bool".
class BoolFlagIdiom
  def build
    @on = false
    @on = true
  end
end

# The nullable-slot idiom: a typed value then cleared back to nil.
class ClearToNil
  def build
    @cache = "value"
    @cache = nil
  end
end

# Operator / or writes are not plain ivar writes, so they are never collected.
class OpWrites
  def build
    @x = "s"
    @x ||= 5
    @x += 1
  end
end

# `self.x =` is a `x=` method call, not an ivar write.
class SelfSetter
  def build
    self.x = "s"
    self.x = 5
  end
end

# A multi-class `rescue A, B => e` binds `e` to a union with no single concrete
# class, so the bound-var write is unresolvable and the group stays silent.
class RescueMultiClass
  def run
    @error = "boom"
  rescue TypeError, ArgumentError => error
    @error = error
  end
end

# The same ivar name in DIFFERENT classes never compares across them.
class SharedNameA
  def build
    @shared = "s"
  end
end

class SharedNameB
  def build
    @shared = 5
  end
end

# A nested `def` is a barrier — its write belongs to the inner unit.
class NestedDefBarrier
  def outer
    @x = "s"
    def inner
      @x = 5
    end
  end
end

# A singleton `def self.x` body's ivars live on the class object, not instances.
class SingletonDef
  def self.build
    @x = "s"
    @x = 5
  end
end
