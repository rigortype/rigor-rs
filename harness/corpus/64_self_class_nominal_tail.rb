# C3a (self.class nominal-return tail): `self.class` inside a lexical
# class/module types to the CLASS OBJECT (`Singleton(enclosing)`), and
# `name`/`to_s` on a class object returns the class name as a `String`. This
# lights the `self.class.name.demodulize` / `.underscore` idiom: the tail is a
# real `String`, so a non-String method on it witnesses `call.undefined-method`
# against the String RBS. Bonus: `name`/`to_s` on any core-RBS `Singleton`
# (`Time.name`) also returns `String`.
#
# Byte-for-byte against the oracle on (rule, line, column). `self.class` itself
# is NOT witnessed (a class object may carry arbitrary class methods), and the
# possible-nil channel must stay silent on the (non-nilable) String tail.
#
# NOTE: only DIRECT chains are exercised here. A `local = self.class.name`
# followed by `local.typo` inside a method body is a reference FIRE that rigor-rs
# MISSES — but for an orthogonal, pre-existing reason (method-body locals are not
# threaded into the flat rule-walk env), NOT the C3a mechanism. Left out so this
# fixture documents only C3a's proven surface.

# --- FIRES: `.name`/`.to_s` on `self.class` is a String; a non-String tail
#     method witnesses against String -------------------------------------

class Widget
  def bad_demodulize
    # `demodulize` is ActiveSupport (absent config-less) -> String typo fires.
    self.class.name.demodulize
  end

  def bad_via_to_s
    self.class.to_s.frobnicate
  end
end

module Outer
  class Runner
    def bad_underscore
      # Deeply nested enclosing class still resolves.
      "#{self.class.name.underscore}:queues"
    end
  end
end

# --- STAYS SILENT ---------------------------------------------------------

class Quiet
  def real_string_methods
    # `upcase`/`length` ARE on String -> no witness. This is also the
    # possible-nil negative: the String tail is non-nilable, so the possible-nil
    # channel must not fire even though `Module#name` is RBS-typed `String?`.
    self.class.name.upcase
    self.class.name.length
  end

  def self_class_bare
    # `self.class` itself is a class object; a typo on it is NOT witnessed
    # (the class may define arbitrary class methods) -> silent.
    self.class.frobnicate
  end
end

# A nested class whose written name SHADOWS a core class: `self.class` must NOT
# resolve to the core `Time` singleton (that would witness core class-method
# typos) — the enclosing PROJECT class is authoritative, so this stays silent.
module Shadowing
  class Time
    def bar
      self.class.frobnicate
    end
  end
end

# Toplevel: no enclosing class, so `self.class` declines -> silent.
self.class.name.frobnicate
