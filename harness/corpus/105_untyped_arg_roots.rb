# The #521 untyped-argument decline, for roots that are NOT a `def` local.
#
# Fixture 99 pins the first half of upstream #521 / PR #537 (`3d5dddbb`, the
# `v0.3.4 -> v0.3.8` re-pin): `Float`/`Integer`/`Array`/`rand` must not pin one
# overload when the argument is the literal untyped carrier. Its allow-list
# (`Typer::arg_is_reference_untyped`) only admitted a root that is a LOCAL of the
# enclosing `def`, which left two false positives standing on the standing
# sweep — gitlab-foss `lib/gitlab/ci/config/entry/pull_policy.rb:28`
# (`Array(@config).presence`, an ivar with no write in the file) and
# `lib/gitlab/filter_evaluator.rb:15` (`->(actual, expected) { Array(expected)
# .exclude?(actual) }`, a lambda parameter at class-body level).
#
# This fixture pins the four ROOT KINDS that closes them, each a port of what the
# reference actually types — NOT a guess:
#
#   ivar   `scope.ivar` reads the per-class table `build_class_ivar_index`
#          builds from `@x = …` writes in the class's `def` bodies. No entry ->
#          `Dynamic[Top]`; an entry whose every write is itself reference-untyped
#          stays untyped; but `contribute_read_before_write_nil!` folds
#          `Constant[nil]` into an entry the class reads before writing unless
#          `initialize` (or the class body) writes it — so an untyped write in a
#          non-ctor method still fires.
#   cvar   `build_class_cvar_index` collects `def`-body writes ONLY; a class-body
#          `@@n = nil` is walked past and never recorded.
#   gvar   `build_program_global_index` is program-wide: every `$x = …` counts.
#   proc   a `->` / `lambda {}` / `proc {}` / `Proc.new {}` parameter is untyped
#          like a method's. An ORDINARY block's parameter is NOT (the RBS yield
#          types it), and a `->` body's local writes never bind at all, while the
#          `lambda {}` spelling's do.
#   const  a name nothing resolves is untyped; a class, an RBS object constant, a
#          qualified path and anything the project writes all keep firing.
#
# Every firing line and every silent row below is oracle-measured at the `v0.3.8`
# pin (`ffb456b0`), one fresh temp cwd per case, `--no-cache`, both reference
# libs pinned onto `-I` (UPSTREAM.md hazard 1). Local names are unique per row on
# purpose: the write/guard scans are region-wide, not flow-ordered, so a reused
# name in a sibling row would refuse the test for a reason the row is not about.

# === IVAR ROOTS ==============================================================

# --- STAYS SILENT ------------------------------------------------------------

# (1) an ivar written only from an untyped ctor parameter: the class table's
# entry IS `Dynamic[Top]`, and the `initialize` write exempts it from the
# read-before-write nil contribution (row r1).
class R1IvarFromParam
  def initialize(config)
    @config = config
  end

  def v
    Array(@config).frobnicate_r1
  end
end

# (2) an ivar with NO write in the class at all — no entry, so no nil is
# contributed either (rows r2/r4). This is the gitlab-foss `pull_policy.rb:28`
# shape, `presence` included.
class R2IvarNoWrite
  def v
    Array(@config).frobnicate_r2
  end

  def w
    Float(@n).frobnicate_r4
  end

  def x
    Array(@config).presence
  end
end

# (3) the writes the reference's collector does NOT see either: a CLASS-BODY
# `@x = …` (row i1), an `@x ||= …` in a sibling `def` (row i2 — the collector
# recognises a plain `InstanceVariableWriteNode` only), and a write in a NESTED
# class, whose ivars belong to that class (row i9).
class I1ClassBodyWrite
  @cb = "s"

  def v
    Array(@cb).frobnicate_i1
  end
end

class I2OrWriteSibling
  def s
    @ow ||= "s"
  end

  def v
    Array(@ow).frobnicate_i2
  end
end

class I9Nested
  class Inner
    def s
      @deep = "s"
    end
  end

  def v
    Array(@deep).frobnicate_i9
  end
end

# (4) a call CHAIN over an untyped ivar is untyped too (row z8), and a top-level
# `def` never gets a class table at all (row i7).
class Z8Chain
  def initialize(c)
    @s8 = c
  end

  def v
    Array(@s8.to_s).frobnicate_z8
  end
end

def i7_toplevel_def
  Float(@zz7).frobnicate_i7
end

# (5) a top-level `@x = …` does not reach a `def` body (row t2) — unlike the
# same-body read pinned below.
@t2 = "s"
def t2_reader
  Float(@t2).frobnicate_t2
end

# (6) an ivar is never CLASS-GUARDED into a Nominal: `is_a?`, the inline `if`
# form, `case`/`when` and the `rand` fold are all reference-silent (rows
# i13/z9/n5/q4/q5), where the same guard on a def local fires `for String`.
class Z9Guarded
  def initialize(c)
    @s9 = c
  end

  def v
    return unless @s9.is_a?(String)

    Array(@s9).frobnicate_z9
  end

  def w
    case @s9
    when String then Array(@s9).frobnicate_q4
    end
  end
end

class Q5RandGuard
  def initialize(c)
    @g5 = c
  end

  def v
    return unless @g5.is_a?(Integer)

    rand(@g5).frobnicate_q5
  end
end

# (7) two untyped writes, one of them the ctor's, still union to the carrier
# (row n1).
class N1TwoUntypedWrites
  def initialize(c)
    @n1 = c
  end

  def s(d)
    @n1 = d
  end

  def v
    Array(@n1).frobnicate_n1
  end
end

# --- KEEPS FIRING ------------------------------------------------------------

# (8) ONE typed write makes the union discriminable again: a ctor literal
# (row r3), a typed sibling write beside an untyped ctor one (rows r5/z6), a
# defaulting write in the reader itself (row z10), and a ctor `nil` (row n2).
class R3IvarLiteral
  def initialize
    @lit = "x"
  end

  def v
    Array(@lit).frobnicate_r3
  end
end

class R5MixedWrites
  def initialize(c)
    @c5 = c
  end

  def reset
    @c5 = 1
  end

  def v
    Float(@c5).frobnicate_r5
  end
end

class Z6LaterTypedWrite
  def initialize(c)
    @c6 = c
  end

  def w
    @c6 = "later"
  end

  def v
    Array(@c6).frobnicate_z6
  end
end

class Z10DefaultedInReader
  def initialize(c)
    @s10 = c
  end

  def v
    @s10 = "x" if @s10.nil?
    Array(@s10).frobnicate_z10
  end
end

class N2CtorNil
  def initialize
    @n2 = nil
  end

  def v
    Array(@n2).frobnicate_n2
  end
end

# (9) an untyped write in a NON-ctor method is not exempt from the
# read-before-write nil contribution, so the entry is nil-bearing and the
# reference still fires — rows z7 and i12 (`@c12 = @d12` over an ivar nothing
# writes). These two are why the ctor write is a REQUIREMENT, not a bonus.
class Z7NonCtorWrite
  def s(c)
    @s7 = c
  end

  def v
    Array(@s7).frobnicate_z7
  end
end

class I12IvarFromIvar
  def s
    @c12 = @d12
  end

  def v
    Array(@c12).frobnicate_i12
  end
end

# (10) a write inside a BLOCK of a `def` is still a def-body write (row i11),
# a module body is a class body (row i8), a top-level `def`'s own write binds
# there (row i6), and a top-level write binds a top-level read (row t1).
class I11BlockWrite
  def s
    [1].each { @blk = "s" }
  end

  def v
    Array(@blk).frobnicate_i11
  end
end

module I8Module
  def s
    @m8 = "s"
  end

  def v
    Array(@m8).frobnicate_i8
  end
end

def i6_toplevel_write
  @x6 = "s"
  Float(@x6).frobnicate_i6
end

@t1 = "s"
Float(@t1).frobnicate_t1

# (11) a MULTI-ASSIGNMENT ivar target IS collected by the reference
# (`record_multi_write_ivars`), and the arena's `MultiTarget::Ignored` carries no
# name to match — so any unnameable target in the class refuses the whole test
# (row i5).
class I5MultiWrite
  def s
    @a5, @b5 = "s", 1
  end

  def v
    Array(@a5).frobnicate_i5
  end
end

# --- COVERAGE GAP (reference fires, rigor-rs is silent) ----------------------

# (12) `@x ||= …` followed by a read in the SAME `def`: the or-write binds
# flow-sensitively on the reference (`Array[Dynamic[top]]`), but Prism's
# `InstanceVariableOrWriteNode` has no owned arena variant here, so the write is
# invisible and the test admits the ivar. A recorded gap, not a divergence —
# closing it needs the ivar op-write lowered (row i3).
class I3OrWriteSameDef
  def s
    @o3 ||= "s"
    Array(@o3).frobnicate_i3
  end
end

# === CVAR AND GVAR ROOTS =====================================================

# --- STAYS SILENT ------------------------------------------------------------

# (13) a CLASS-BODY `@@n = …` is never recorded (row r14), and neither a
# never-written cvar (row c3) nor one written only from an untyped parameter
# (rows n3/n7) leaves the carrier.
class R14CvarClassBody
  @@n14 = nil

  def v
    Float(@@n14).frobnicate_r14
  end
end

class C3CvarAbsent
  def v
    Float(@@absent3).frobnicate_c3
  end
end

class N7CvarMixed
  @@c7 = nil

  def s(c)
    @@c7 = c
  end

  def v
    Float(@@c7).frobnicate_n7
  end
end

# (14) a gvar nothing writes (row g2), and one written only from an untyped
# parameter (row n4).
def g2_absent
  Float($nope_g2).frobnicate_g2
end

def n4_writer(c)
  $gv4 = c
end

def n4_reader
  Float($gv4).frobnicate_n4
end

# --- KEEPS FIRING ------------------------------------------------------------

# (15) a `def`-body cvar write IS recorded (row c1); a gvar write anywhere in the
# program counts, at top level (row r15) or inside a `def` (row g3).
class C1CvarDefWrite
  def s
    @@k1 = "s"
  end

  def v
    Float(@@k1).frobnicate_c1
  end
end

$g15 = nil
def g15_reader
  Float($g15).frobnicate_r15
end

def g3_writer
  $set_g3 = "s"
  Float($set_g3).frobnicate_g3
end

# === PROC-LIKE PARAMETERS ====================================================

# --- STAYS SILENT ------------------------------------------------------------

# (16) the four proc-like spellings, at class-body / top level where there is no
# enclosing `def` to supply the region (rows r6/r7/r8, and the gitlab-foss
# `filter_evaluator.rb:15` shape verbatim).
FA2_OPS = {
  'a' => ->(actual_r6, expected_r6) { Array(expected_r6).frobnicate_r6(actual_r6) },
  'b' => lambda { |x_r7| Float(x_r7).frobnicate_r7 },
  'c' => proc { |x_r8| Float(x_r8).frobnicate_r8 },
  'd' => Proc.new { |x_l4| Float(x_l4).frobnicate_l4 }
}.freeze

# (17) a `->` body's local writes NEVER bind on the reference — neither a
# parameter rebind (row r11) nor a fresh lambda-local (row p13) — while the
# `lambda {}` spelling's DO (row m13, which fires below).
def r11_lambda_rebind
  ->(y_r11) { y_r11 = 1; Float(y_r11).frobnicate_r11 }
end

def r12_lambda_param
  ->(z_r12) { Float(z_r12).frobnicate_r12 }
end

# --- KEEPS FIRING ------------------------------------------------------------

# (18) an ORDINARY block's parameter is typed from the RBS yield, so it is not
# reference-untyped — rows r9/r10 fire on the reference (rigor-rs is silent
# there: a pre-existing gap fixture 99's def-local rule already owned, recorded
# here so a change that starts CLAIMING these params is visible).
def r9_block_params
  [1, 2].each { |x_r9| Float(x_r9).frobnicate_r9 }
  [1, 2].map { |x_r10| Array(x_r10).frobnicate_r10 }
end

# (19) a `lambda {}` parameter REBOUND inside a `def` keeps its pin (row m13).
def m13_lambda_rebind
  lambda { |q_m13| q_m13 = 1; Float(q_m13).frobnicate_m13 }
end

# (20) a local CAPTURED from the enclosing scope is typed there, so a `->` body
# reading it must not be admitted (row l5).
w_l5 = "s"
L5_CAPTURE = -> { Float(w_l5).frobnicate_l5 }

# === CONSTANT ROOTS ==========================================================

# --- STAYS SILENT ------------------------------------------------------------

# (21) a bare name nothing resolves — not a class, not an RBS object constant,
# not written by the project (rows r17/k2).
def r17_unresolved
  Array(NO_SUCH_CONSTANT_R17).frobnicate_r17
end

# --- KEEPS FIRING ------------------------------------------------------------

# (22) every resolvable spelling: a project constant (rows k1/k5), one shadowed
# inside a class (row k8), a class object (row k3), and a QUALIFIED path, which
# is refused outright because the port has no table for class-scoped RBS
# constants (rows k4/k6).
K1_STR = "s"
K5_ARR = [1, 2]

def k1_reads
  Float(K1_STR).frobnicate_k1
end

def k5_reads
  Array(K5_ARR).frobnicate_k5
end

class K8Shadow
  K1_STR = 5

  def v
    Float(K1_STR).frobnicate_k8
  end
end

def k3_class_object
  Array(String).frobnicate_k3
end

def k4_core_path
  Float(Float::INFINITY).frobnicate_k4
end

def k6_namespaced
  Float(Errno::ENOENT).frobnicate_k6
end
