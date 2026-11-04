# A wrapped `private def` / `protected def` carries that visibility
# (upstream #1819/#1911/#1633, v0.4.4).
#
# The reference's visibility table used to record `private def foo` at the
# body's running default, so a parent's `private def foo` read as public and a
# child's `private; def foo` was a "reduction". v0.4.4 records the wrapped def
# at the CALL's visibility, applies `self.private` / `send(:private)` and
# `private %i[a b]`, and re-marks an `attr_*` member. Every override row below
# the controls fired at v0.4.3 and is silent now, except `B12`: the table is
# last-write-wins per name (a hash upstream), and the port's first-write-wins
# body table fired there on its own.
#
# Must-still-fire controls: a plain public parent under a bare `private` child,
# a wrapped `private def` child of a public parent, a `public def` under a bare
# `private` parent, and a `self.private` reduction.

class BaseC
  def foo = 1
end
class SubC < BaseC
  private
  def foo = 2
end

class BaseW
  def foo = 1
end
class SubW < BaseW
  private def foo = 2
end

class B6
  private
  public def foo = 1
end
class S6 < B6
  private
  def foo = 2
end

class B7
  def foo = 1
end
class S7 < B7
  self.private
  def foo = 2
end

class B1
  private def foo = 1
end
class S1 < B1
  protected
  def foo = 2
end

module M2
  private def foo = 1
end
class S2
  include M2
  private
  def foo = 2
end

class B3
  protected def foo = 1
end
class S3 < B3
  private
  def foo = 2
end

class B4
  private def foo(a) = a
  private def bar = 1
end
class S4 < B4
  private
  def foo(a) = a
  def bar = 2
end

class B8
  private def foo = 1
end
class S8 < B8
  def foo = 2
  private :foo
end

class B9
  def foo = 1
  def bar = 1
  private %i[foo bar]
end
class S9 < B9
  private
  def foo = 3
  def bar = 3
end

class B10
  def foo = 1
  send(:private, :foo)
end
class S10 < B10
  private
  def foo = 3
end

class B11
  def x = 1
  private attr_reader :x
end
class S11 < B11
  private
  def x = 2
end

class B12
  def foo = 1
  def foo = 2
  private :foo
end
class S12 < B12
  private
  def foo = 3
end

# A same-file reopening that re-marks the child's visibility is silent on the
# reference; the per-body table cannot see it, so the port declines.
class B13
  def foo = 1
end
class S13 < B13
  private def foo = 2
end
class S13
  public :foo
end

# Control: a reopening that leaves `foo` alone still fires.
class B14
  def foo = 1
end
class S14 < B14
  private def foo = 2
end
class S14
  def bar = 1
end
