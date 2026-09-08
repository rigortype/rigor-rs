# A value typed as a mixin MODULE, and a value typed `Class` or `Module`, have no
# enumerable method surface, so `call.undefined-method` cannot fire on them.
#
# Upstream #739 / PR #741 (`3636649f`) and #742 / PR #743 (`23341a87`), both
# shipped in `v0.3.8`. A parameter typed `Taggable` in RBS means "something whose
# class includes Taggable", not "something whose methods are Taggable's": the
# includer contributes an arbitrary surface, and nothing there can prove a method
# absent. The rule used to RECOGNISE such a receiver and merely retry the lookup
# against `Object` — enough for `self.inspect` / `self.class` and nothing else.
# The same argument covers the two generic metaclasses: a value typed `Class` is
# SOME class object, and `def self.included(base)` receives the includer, so
# `base.class_attribute :x` is a call on whatever included the module.
# `unenumerable_receiver?` = `METACLASS_ARMS ∪ unbounded_receiver_surface?` now
# answers both, and `module_mixin_receiver?` the instance-side module half.
#
# At the `v0.3.4` pin rigor-rs matched the old behaviour (fixture 91's q5 row was
# a MATCH); at `v0.3.8` the same output is a false positive.
#
# SCOPE, and each half of it is a control below, because the naive over-broad fix
# passes the positives and fails these:
#   * the SINGLETON side keeps firing — a namespace module's `module_function` /
#     `def self.` surface is real and enumerable (rows c4/c5/c11). Declining every
#     `Singleton` receiver silences all three.
#   * a subclass-ordering guard keeps its ORIGINAL carrier — `h.is_a?(Enumerable)`
#     on an Array still witnesses `for Array` (row x11). Declining "any guard that
#     names a module" silences it.
#   * only `call.undefined-method` moved: `v.hexdigest(1, 2, 3)` behind a
#     `Digest::Instance` guard still reports `call.wrong-arity` on the ORACLE
#     (row x1) — rigor-rs is silent there for an unrelated, pre-existing reason,
#     so it is a coverage gap and is recorded, not chased.
#
# Every firing line and every silent control is oracle-measured at the `v0.3.8`
# pin (`ffb456b0`), one fresh temp cwd per case, `--no-cache`, both reference libs
# pinned onto `-I` (UPSTREAM.md hazard 1). Row ids are the ones in
# docs/notes/20260909-repin-v038-rules-families.md.

require "digest"

# --- STAYS SILENT: an instance-side MODULE receiver --------------------------

# c1 — a qualified stdlib module (the fixture-91 q5 row's family).
def c1(v)
  return unless v.is_a?(Digest::Instance)

  v.frobnicate_c1
end

# c2 — a top-level core module.
def c2(v)
  return unless v.is_a?(Enumerable)

  v.frobnicate_c2
end

# c3 — likewise.
def c3(v)
  return unless v.is_a?(Comparable)

  v.frobnicate_c3
end

# c10 — `Kernel`, the module every object already includes.
def c10(v)
  return unless v.is_a?(Kernel)

  v.frobnicate_c10
end

# x8 — the `case/when` spelling of the same guard.
def x8(v)
  case v
  when Comparable then v.frobnicate_x8
  end
end

# x10 — the `kind_of?` spelling.
def x10(v)
  return unless v.kind_of?(Kernel)

  v.frobnicate_x10
end

# x12 — `instance_of?`. The oracle reaches silence by a DIFFERENT route here (an
# exact-class guard against a module is `Bot`, so the truthy edge is empty); the
# decline covers it either way.
def x12(v)
  return unless v.instance_of?(Comparable)

  v.frobnicate_x12
end

# --- STAYS SILENT: a `Class` / `Module` value --------------------------------

# c7
def c7(v)
  return unless v.is_a?(Class)

  v.frobnicate_c7
end

# c8
def c8(v)
  return unless v.is_a?(Module)

  v.frobnicate_c8
end

# x9 — the `case/when` spelling.
def x9(v)
  case v
  when Class then v.frobnicate_x9
  end
end

# x3 / x4 — the SINGLETON reads of the two metaclass constants. The reference's
# `unenumerable_receiver?` sits ABOVE the instance/singleton split, so these
# decline too — the one place the singleton side is touched at all.
def x3
  Class.frobnicate_x3
end

def x4
  Module.frobnicate_x4
end

# c6 — `v.class.typo` on a mixin-typed `v`. Upstream keys this on the call-site
# SYNTAX (`mixin_self_class_receiver?`) because `Singleton[M]` is also what a
# namespace module's own `M.helper` produces. rigor-rs needs no port: `.class` on
# a Dynamic receiver yields no witnessable carrier. Silent on both, pinned here so
# a future `.class` typing slice cannot open it silently.
def c6(v)
  return unless v.is_a?(Digest::Instance)

  v.class.frobnicate_c6
end

# --- STAYS SILENT: the method is PRESENT -------------------------------------

# x6 — an Object-inherited method, the shape the OLD `Object` retry existed for.
def x6(v)
  return unless v.is_a?(Digest::Instance)

  v.inspect
end

# x7 — a method the module itself declares.
def x7(v)
  return unless v.is_a?(Enumerable)

  v.each_slice(2)
end

# x13 — a method `Module` itself declares.
def x13(v)
  return unless v.is_a?(Module)

  v.ancestors
end

# c13 — a PROJECT module. Silent on both for the older ADR-0033 provenance
# reason (an in-source-only name is never a witnessing surface), so it is not
# evidence for this family — pinned as the "no new door" control.
module ProjMix
  def pm; end
end

def c13(v)
  return unless v.is_a?(ProjMix)

  v.frobnicate_c13
end

# --- FIRES: the singleton surface is real and enumerable ---------------------
#
# These three are what an over-broad `Singleton`-wide decline would silence.

# c4
def c4
  Digest::Instance.frobnicate_c4
end

# c5
def c5
  Comparable.frobnicate_c5
end

# c11
def c11
  String.frobnicate_c11
end

# --- FIRES: an ordinary class receiver is untouched ---------------------------

# c9 — the instance-side control: a CLASS guard still witnesses.
def c9(v)
  return unless v.is_a?(String)

  v.frobnicate_c9
end

# x11 — a module guard the environment can ORDER against the carrier keeps the
# carrier's own bound (`Array < Enumerable`), so the receiver here is `Array`,
# not `Enumerable`, and the witness stands. The control an "any module named in a
# guard" test would silence.
def x11
  h = Array.new
  h.frobnicate_x11 if h.is_a?(Enumerable)
end

# --- COVERAGE GAP (oracle fires, rigor-rs silent) — recorded, not chased ------
#
# c12: `v.hexdigest.frobnicate_c12` behind a `Digest::Instance` guard. The oracle
#   types the module method's `String` return and witnesses on THAT; rigor-rs does
#   not thread a return type off a narrowed module receiver.
# x1:  `v.hexdigest(1, 2, 3)` behind the same guard reports `call.wrong-arity` on
#   the oracle. rigor-rs runs no arity check on a narrowed receiver at all (the
#   `String` control x2 is equally silent), so this is NOT evidence that the
#   arity rule needs the decline — it is the pre-existing narrowed-arity gap.
def c12(v)
  return unless v.is_a?(Digest::Instance)

  v.hexdigest.frobnicate_c12
end

def x1(v)
  return unless v.is_a?(Digest::Instance)

  v.hexdigest(1, 2, 3)
end

def x2(v)
  return unless v.is_a?(String)

  v.upcase(1, 2, 3)
end
