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
