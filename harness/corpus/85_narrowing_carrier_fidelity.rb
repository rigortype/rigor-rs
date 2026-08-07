# Class narrowing — CARRIER FIDELITY of the Dynamic-only gate
# (docs/notes/20260808-narrowing-carrier-fidelity-fp.md).
#
# The reference's `narrow_class_other` (`narrowing.rb:2425`) narrows a
# `Dynamic`/`Top` carrier and leaves every other carrier alone, so "rigor-rs
# narrows only Dynamic" is a SOUND-SUBSET rule exactly while `Dynamic` means the
# same thing in both engines. It does not: rigor-rs collapses a long tail of
# carriers to `Dynamic[top]` that the reference types precisely — a `||` union,
# a `Range`, a `Proc`, `self`, a `case`/`if` union, `defined?`, a loop's `nil` —
# and on every one of them our gate fired where theirs declined. Master emitted
# a diagnostic the reference does not, violating ADR-0002.
#
# The gate is now an ALLOW-list: a local narrows only when it is a parameter or
# its binding is a carrier measured `Dynamic` on BOTH engines. Every positive
# below FIRES on the pinned reference (fresh cwd, `--no-cache`, plugin path
# pinned); every negative control is SILENT on the reference and must be silent
# here. The `# gap:` controls are the price — the reference fires and rigor-rs
# declines, a strict subset, never an extra.

# --- positives: allow-listed carriers, Dynamic on BOTH engines ----------------

# (1) a method parameter — the ordinary narrowing this slice must NOT break.
def param_carrier(spec)
  spec.is_a?(Hash) ? spec.frobnicate_aaa : spec
end

# (2) a keyword / optional / rest parameter read into a local.
def kwarg_carrier(key: nil)
  h = key
  h.is_a?(Hash) ? h.frobnicate_bbb : h
end

# (3) an `@ivar` read — the reference types no instance variable.
def ivar_carrier
  h = @settings
  h.is_a?(Hash) ? h.frobnicate_ccc : h
end

# (4) a `$gvar` read.
def gvar_carrier
  h = $config_zzz
  h.is_a?(Hash) ? h.frobnicate_ddd : h
end

# (5) a call THROUGH a narrowable receiver: an untyped receiver resolves no
# method on either engine, so the result is untyped on both. Includes a chain,
# an index, safe-nav and a block-bearing call.
def call_carrier(spec)
  h = spec.lookup_zzz
  h.is_a?(Hash) ? h.frobnicate_eee : h
end

def call_chain_carrier(spec)
  h = spec.lookup_zzz.deref_zzz
  h.is_a?(Hash) ? h.frobnicate_fff : h
end

def call_index_carrier(spec)
  h = spec[0]
  h.is_a?(Hash) ? h.frobnicate_ggg : h
end

def call_safenav_carrier(spec)
  h = spec&.dup
  h.is_a?(Hash) ? h.frobnicate_hhh : h
end

def call_block_carrier(spec)
  h = spec.map { |x| x }
  h.is_a?(Hash) ? h.frobnicate_iii : h
end

def call_ivar_receiver_carrier
  h = @store.lookup_zzz
  h.is_a?(Hash) ? h.frobnicate_jjj : h
end

# (6) destructuring — it loses precision on BOTH sides, even off a `||` RHS.
def multiwrite_carrier(spec)
  _a, h = spec
  h.is_a?(Hash) ? h.frobnicate_kkk : h
end

def multiwrite_from_logical_carrier(spec)
  _a, h = (spec || {})
  h.is_a?(Hash) ? h.frobnicate_lll : h
end

# (7) the `raise`-guard early-return form over a parameter.
def raise_guard_param(spec)
  raise ArgumentError unless spec.is_a?(Hash)

  spec.frobnicate_mmm
end

# --- negative controls: coarse carriers, SILENT on the reference --------------

# (8) THE ARCHETYPE (gitlab-foss `lib/ci/inputs/base_input.rb:30`,
# `spec_hash = spec || {}`). `analyse_or` builds a UNION of the operand types,
# so the reference's Dynamic-only gate declines. Master fired here.
def logical_or_declines(spec)
  h = spec || {}
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (9) the same carrier reached through a `raise` guard rather than a ternary —
# the early-return propagation path.
def logical_or_raise_guard_declines(spec)
  h = spec || {}
  raise ArgumentError unless h.is_a?(Hash)

  h.frobnicate_zzz
end

# (10) `&&` — the same union.
def logical_and_declines(spec)
  h = spec && {}
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (11) `h ||= …` — an op-assign is a union on the reference too.
def op_assign_declines(spec)
  h = spec
  h ||= {}
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (12) a PROJECT METHOD whose return tail is a `||`
# (gitlab-foss `lib/gitlab/encrypted_configuration.rb:70`). The carrier gap
# travels through the call, so the guard must decline at the binding.
def make_options_zzz
  fetch_options_zzz || {}
end

def insource_logical_return_declines
  h = make_options_zzz
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (13) a loop AS AN EXPRESSION — the reference types it `nil`.
def loop_value_declines(cond)
  h = while cond
    break({})
  end
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (14) `begin`/`rescue` as an expression — a union of the body and the clauses.
def begin_rescue_value_declines(spec)
  h = begin
    spec
  rescue StandardError
    {}
  end
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (15) the `rescue` MODIFIER — the same union in one line.
def rescue_modifier_value_declines(spec)
  h = (spec rescue {})
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (16) a `case` with an `else` — a union the reference keeps and rigor-rs's
# `Algebra::join` collapses into `Dynamic[top]`.
def case_value_declines(cond, spec)
  h = case cond
      when 1 then spec
      else {}
      end
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (17) an `if`/ternary as an expression — the same collapse.
def if_value_declines(cond, spec)
  h = cond ? spec : {}
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (18) a `Range` — the reference types it `Range`.
def range_value_declines
  h = (1..2)
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (19) a lambda / proc — the reference types both `Proc`. Note `proc { }` is a
# receiverless call, which is why an implicit-self call cannot be allow-listed.
def proc_value_declines
  h = proc { |x| x }
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (20) `defined?` — the reference types it `String?`.
def defined_value_declines(spec)
  h = defined?(spec)
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (21) a Kernel method with a precise RBS return, reached receiverless.
def kernel_return_declines
  h = __method__
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# (22) a constant RECEIVER the reference resolves precisely.
def const_receiver_declines
  h = Float::INFINITY.abs
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# --- coverage controls: the reference FIRES, rigor-rs declines ----------------
# gap: `self` is not a narrowable receiver — the reference resolves an in-source
# method through it (control 24) and gets that method's real return, so the
# whole `self.` receiver shape is declined rather than split.
class CarrierFidelityHost
  def self_receiver_gap
    h = self.unknown_zzz
    h.is_a?(Hash) ? h.frobnicate_zzz : h
  end

  # (24) and this is why: `mk`'s return tail is a `||`, so narrowing `h` here
  # would be a live FP. SILENT on both engines.
  def self_receiver_logical_declines
    h = self.mk

    h.is_a?(Hash) ? h.frobnicate_zzz : h
  end

  def mk
    unknown_zzz || {}
  end
end

# gap: `yield` and `super` are untyped on both engines, but they lower into the
# same span-only carriers as `defined?` and a `*splat`, which are NOT. The
# carrier cannot be told apart in the arena, so all of them decline.
def yield_value_gap
  h = yield
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# gap: an implicit-self call — untyped on both engines when the method resolves
# nowhere, but `__method__`/`proc`/`binding` share the shape (control 19/21).
def implicit_self_gap
  h = totally_unknown_zzz
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end

# gap: a `case` with NO `else` — the reference collapses `untyped | nil` back to
# untyped and fires; we decline the whole `case` carrier.
def case_no_else_gap(cond, spec)
  h = case cond
      when 1 then spec
      end
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end
