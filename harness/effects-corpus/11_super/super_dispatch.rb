# `super` is a DISPATCH, and the effect model has to treat it as one
# (upstream #446 / PR #453, shipped in v0.3.5 and reached by the v0.3.8 repin).
#
# Upstream's scan emits an edge carrying the ENCLOSING unit's own class and
# selector with `super_call: true`, and the propagator resolves it against the
# merged ancestry ABOVE that class: the parent's labels join the child's, the
# parent's taint bit ANDs into it, and a `super` nothing in the project answers
# seeds `unresolved-super(<selector>)`.
#
# rigor-rs has no ancestry at all — nothing records `superclass` or `include` —
# so its collector takes upstream's "cannot say" branch at every `super`. That
# is the sound direction under
# [ADR-0043](../../../docs/adr/0043-effect-system-port-parity-model.md) § 2
# (more taint under-claims; claiming exhaustiveness across an unread parent body
# does not), and it is what the rows below pin: the ORACLE's answer is the gold,
# and the port is graded a subset of it.
#
# Every unit here is named for the shape it holds. The last one is the control:
# no `super`, so nothing about this port may touch it.

# The parent is IN the project and carries an effect. Upstream contributes it to
# every child that `super`s into it, so the oracle's row for the children below
# is where a future ancestry slice gets its target.
class Base
  def initialize(name)
    @name = name
  end

  def emit
    puts "base"
  end

  def self.build
    $registry = :built
  end

  def wrapped
    File.read("/etc/hosts")
  end

  def interpolated
    ENV.fetch("BASE", nil)
  end
end

module Decorating
  def decorated
    ENV.fetch("DECOR", nil)
  end
end

# A `super` the project's OWN ancestry answers. The ORACLE's rows are pinned in
# the comments — verified with `rigor effects --full --format=json` at the
# v0.3.8 pin — because they are the target a future ancestry slice aims at, and
# every one of them is exhaustive: upstream contributes the parent's labels and
# adds no cause at all. rigor-rs reads every row here as
# `effects: [] / exhaustive: false` with `unresolved-super(<selector>)`, i.e.
# UNDER on both lanes, which is the whole measured cost of having no ancestry.
class Resolvable < Base
  include Decorating

  # Bare `super` (zsuper) into the superclass.
  # ORACLE: effects ["io.output.stdout"], exhaustive true.
  def emit
    super
  end

  # `super(args)`. The parent is `Base#initialize`; the child's own `@decorated`
  # write proves `mutate.self` on BOTH engines, so this is the one resolvable
  # row where the port keeps a label and differs only in the bit.
  # ORACLE: effects ["mutate.self"], exhaustive true.
  def initialize(name)
    super(name)
    @decorated = true
  end

  # `super()` with an explicit empty argument list.
  # ORACLE: effects ["io.fs.read"], exhaustive true.
  def wrapped
    super()
  end

  # A `super` into an INCLUDED module rather than a superclass: `include M` puts
  # `M#m` between the class and its superclass, which is where `super` looks.
  # ORACLE: effects ["global.read"], exhaustive true.
  def decorated
    super
  end

  # Inside a string interpolation, and RESOLVABLE — the parent is
  # `Base#interpolated`. Pinned beside `Shapes#in_interpolation` so the two
  # directions of the same shape sit next to each other.
  # ORACLE: effects ["global.read"], exhaustive true.
  def interpolated
    "v3:#{super}"
  end

  # Singleton side. `super` here walks the singleton ancestry, and `Base.build`
  # is on it.
  # ORACLE: effects ["global.write"], exhaustive true.
  def self.build
    super
  end
end

# A `super` NOTHING in the project answers — the parent is a gem, Ruby's core,
# or a module prepended at run time. This is the case `unresolved-super` exists
# for, and it is the one row where the port's cause and the oracle's coincide
# exactly.
class Orphan
  def orphaned
    super
  end

  def self.orphaned_singleton
    super
  end
end

# The shapes the walk has to reach a `super` THROUGH. Each is a separate unit so
# a shape that stops tainting is one row, not a smear across the file — and none
# of these selectors is on `Base`, deliberately: the `super` is UNRESOLVABLE, so
# the oracle taints, and an engine whose walk never reached the `super` would
# claim exhaustiveness the oracle does not. That is an OVER, which is what makes
# this class a gate rather than a note.
class Shapes < Base
  # Inside a `rescue` body.
  def in_rescue
    raise "boom"
  rescue RuntimeError
    super
  end

  # Inside an `ensure` body.
  def in_ensure
    :ok
  ensure
    super
  end

  # Inside a string interpolation.
  def in_interpolation
    "v3:#{super}"
  end

  # Inside a block. The block's HOST is a catalogued constant-receiver call
  # rather than `[1, 2].each`, so the only taint either engine has a producer
  # for is the `super` — which is what makes a walk that stops at a block an
  # OVER here instead of a silent MATCH.
  def in_block
    File.open("/etc/hosts") { super }
  end

  # Inside a block nested in a block.
  def in_nested_block
    File.open("/etc/hosts") { Dir.glob("*") { super } }
  end

  # With a block of its own.
  def with_block
    super do |resource|
      resource
    end
  end

  # With keyword arguments.
  def with_kwargs
    super(key: :value, other: 1)
  end

  # A `super` in a CONDITIONAL arm is still a `super` in the unit.
  def in_condition(flag)
    if flag
      super
    else
      :skipped
    end
  end
end

# A `super` inside a NESTED `def` belongs to the nested unit, not to the
# enclosing one. `Nesting#outer` must stay free of `unresolved-super(outer)`,
# and `Nesting#inner` must carry `unresolved-super(inner)`.
class Nesting < Base
  def outer
    def inner
      super
    end
  end
end

# The CONTROL: a subclass of the same parent with no `super` anywhere. Its row
# must not move by one field.
class NoSuper < Base
  def plain
    @plain = 1
  end
end
