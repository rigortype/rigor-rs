# FP-audit regression guard (ADR-34): an implicit-self call inside a
# `class << X` SINGLETON-CLASS body is NOT toplevel — it is a class scope — so
# `call.unresolved-toplevel` must stay SILENT, matching the reference. Before the
# fix, rigor-rs treated the singleton-class body as toplevel and fired here (the
# net-ssh / algorithms FP cluster surfaced by the real-corpus audit). Expected
# diagnostic set: EMPTY.

class Widget
end

class << Widget
  def configure
    # An unresolved implicit-self call inside a singleton-class method body.
    some_unresolved_macro
  end

  # A bare Module-method call in the singleton-class body.
  private :configure
end
