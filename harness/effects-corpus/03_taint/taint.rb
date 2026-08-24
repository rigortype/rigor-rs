# The EXHAUSTIVENESS bit and its causes. A method whose calls could not all be
# resolved is marked "and possibly more" WITH the reason, and never guessed at:
# an unresolved call taints the bit and produces no finding (ADR-103's
# discriminating criterion — what the analyzer could not prove is recorded,
# never judged).
#
# This is the lane where the port is allowed to be STRICTER than the oracle:
# more taint is sound, claimed exhaustiveness the oracle does not claim is not
# ([ADR-0043](../../../docs/adr/0043-effect-system-port-parity-model.md) § 2).

class Taint
  def initialize(collaborator)
    @collaborator = collaborator
  end

  # An untyped receiver: nothing to resolve the call against.
  def dynamic_receiver
    @collaborator.do_something
  end

  # `send` with a NON-LITERAL name cannot be resolved at all.
  def dynamic_send(name)
    @collaborator.send(name)
  end

  # `send` with a LITERAL name is an ordinary edge — it must NOT taint.
  def literal_send
    send(:known_target)
  end

  def known_target
    puts "resolved"
  end

  # A method on a class the project does not define and no signature covers.
  def unknown_constant_receiver
    SomeUndeclaredGem::Client.new.fetch
  end

  # A block handed to an opaque callable: the callee may invoke it or not, and
  # the analyzer cannot see the callee.
  def opaque_callable(&blk)
    @collaborator.each(&blk)
  end

  # `method_missing` makes the whole receiver's surface unknowable.
  class Ghost
    def method_missing(name, *args)
      super
    end

    def respond_to_missing?(_name, _priv = false)
      true
    end
  end

  def through_a_ghost
    Ghost.new.anything_at_all
  end

  # The control: fully resolved, no taint, empty summary. If this one is ever
  # marked non-exhaustive the taint has leaked.
  def fully_resolved(a)
    a.to_s.length
  end
end
