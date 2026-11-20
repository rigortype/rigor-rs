# rigor-rs#672 — a `::`-rooted class-guard operand names the TOP level only.
# The lowering keeps `rooted` as a flag while `name` renders unrooted, so the
# dead `starts_with("::")` sniff let `::Stat` inside `class File` walk the
# lexical ladder to `File::Stat`. The reference keeps `::Stat` top-level and
# declines when nothing owns it (upstream #614): all three rows silent.
class File
  def rooted_isa(x)
    x.frobnicate_r1 if x.is_a?(::Stat)
  end

  def rooted_when(x)
    case x
    when ::Stat
      x.frobnicate_r2
    end
  end
end

module Gem
  def rooted_kind_of(x)
    return unless x.kind_of?(::Version)
    x.frobnicate_r3
  end
end

# Must-still-fire controls: the UNROOTED spelling walks the lexical ladder to
# `File::Stat`, and a rooted operand the top level does know (`::String`)
# narrows as before.
class File
  def unrooted_isa(x)
    x.frobnicate_r4 if x.is_a?(Stat)
  end
end

def rooted_known(x)
  x.frobnicate_r5 if x.is_a?(::String)
end

# The slot-record half of the same split: `is_a?` declines a rooted unknown
# operand outright (no scopes — `h[:a]`'s record survives and the else arm
# fires), while `===` still narrows to the unknown `NoSuchZz` nominal, so
# the guard's record cover drops the record (silent).
h = {a: 1}
h[:a] ||= "s"
if h.is_a?(::NoSuchZz)
  1
else
  h[:a].frobnicate_r6
end

if ::NoSuchZz === h
  1
else
  h[:a].frobnicate_r7
end

# Review (PR #708): a rooted operand naming an ASSIGNED class —
# `discovered_classes` counts `K = Struct.new`/`Class.new`/`Data.define`,
# so `is_a?(::Foo)` narrows rather than declining (only `foo_b` fires).
Foo = Struct.new(:a)

def rooted_assigned_isa
  x = [1]
  if x.is_a?(::Foo)
    x.frobnicate_r8
  else
    x.frobnicate_r9
  end
  x.frobnicate_r10
end

# … and a rooted `===`/`when` over a SHAPED carrier whose bare name collides
# with a nested RBS leaf (`Stat` vs `File::Stat`): the operand is
# unorderable, so only the else arm fires — the call after stays silent.
class File
  def rooted_when_shaped
    x = [1]
    case x
    when ::Stat
      x.frobnicate_r11
    else
      x.frobnicate_r12
    end
    x.frobnicate_r13
  end

  def rooted_caseeq_shaped
    x = [1]
    if ::Stat === x
      x.frobnicate_r14
    end
    x.frobnicate_r15
  end
end

# Review (PR #708, round 3): `record_class_new_constant_decl` also counts
# `Foo ||= …`, `::Foo = …`, `M::Foo = …` and writes inside blocks/modifiers —
# the `collect_class_decls` every-child descent (rigor-rs#672).
Ored ||= Struct.new(:a)

def rooted_or_write
  x = [1]
  x.frobnicate_r16 unless x.is_a?(::Ored)
end

module M672
  ::Pathy = Class.new
end

def rooted_path_write
  x = [1]
  x.frobnicate_r17 unless x.is_a?(::Pathy)
end

[1].each { NestedW = Struct.new(:a) }

def rooted_block_write
  x = [1]
  x.frobnicate_r18 unless x.is_a?(::NestedW)
end

ModW = Class.new if true

def rooted_modifier_write
  x = [1]
  x.frobnicate_r19 unless x.is_a?(::ModW)
end

# Review (PR #708, round 4): `self::`-anchored meta-new writes resolve
# against the enclosing `self_prefix` — lexical by default, the eval
# receiver inside `class_eval`, the written name inside a
# `K = Class.new do` block (`self_anchored_decl_prefix`).
module SelfM
  self::Anch = Struct.new(:a)
end

def rooted_self_write
  x = [1]
  x.frobnicate_r20 unless x.is_a?(::SelfM::Anch)
end

EvalR.class_eval do
  self::Ev = Class.new
end

def rooted_eval_self
  x = [1]
  x.frobnicate_r21 unless x.is_a?(::EvalR::Ev)
end

BlkR = Class.new do
  self::InB = Struct.new(:a)
end

def rooted_block_self
  x = [1]
  x.frobnicate_r22 unless x.is_a?(::BlkR::InB)
end

# Review (PR #708, round 6): a rooted `when`/`is_a?` operand whose leaf
# exists only NESTED (`class A; class M` — never `::M`) must take the
# same path as an undeclared one — `knows_class`'s leaf key made `::M`
# hit `A::M`'s registration.
class W1A
  class W1M
  end
end

def rooted_when_nested_leaf
  x = [1]
  case x
  when ::W1M then x.frobnicate_r23
  else x.frobnicate_r24
  end
end

class W1B
  def rooted_when_nested_leaf_in_class
    x = [1]
    case x
    when ::W1M then x.frobnicate_r25
    else x.frobnicate_r26
    end
  end
end
