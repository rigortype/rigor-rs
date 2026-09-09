# A constant-write meta-new block is the class body it is, so
# `call.unresolved-toplevel` cannot fire inside it.
#
# Upstream #590 / PR #619 (`b3d688f7`), shipped in `v0.3.8`. #319 had already
# taught the reference that `Class.new do … end`'s block is `class_eval`'d on the
# created class — but only at the positions `evaluate_block_if_present` reaches,
# which is a statement-level call and (via `sub_eval` of its rvalue) a
# local-variable write. A CONSTANT write had no `StatementEvaluator` handler at
# all: it fell to the pure-expression default, its block was never walked, and
# `ScopeIndexer.propagate` handed every node inside the ENCLOSING scope — a nil
# `self_type` at file top level, which is exactly what `Scope#toplevel?` keys on,
# so the rule fired inside every `def` of the body and on `attr_reader`, the very
# macro #319 silenced everywhere else. `b3d688f7` adds the handler and enters the
# rvalue block through the same `enter_meta_class_body` the #319 arm uses.
#
# At the `v0.3.4` pin rigor-rs matched the old behaviour (fixture 96 rows 70/74
# were MATCHES); at `v0.3.8` the same output is a false positive.
#
# The controls an over-broad port would silence, and both are pinned below:
#   * only the BLOCK BODY is a class scope — the ARGUMENTS keep the enclosing
#     scope, so `Class.new(parent_of(1)) do … end` still fires on `parent_of`;
#   * only a META-NEW selector's block is a class body — #316's DSL block
#     (`some_dsl_call do attr_reader :d end`) fires TWICE, on the call and on the
#     macro inside it.
#
# Every firing line and every silent control is oracle-measured at the `v0.3.8`
# pin (`ffb456b0`), one fresh temp cwd per case, `--no-cache`, both reference libs
# pinned onto `-I` (UPSTREAM.md hazard 1). Row ids are the ones in
# docs/notes/20260909-repin-v038-rules-families.md.

module Outer; end

# --- STAYS SILENT: the constant-write body is a class body -------------------

# d1 — `Class.new`, the commonest spelling (fixture 96 row 70).
Registry = Class.new do
  attr_reader :entries
end

# d2 — `Module.new` (fixture 96 row 74).
Coercible = Module.new do
  attr_reader :raw
end

# d3 — `Struct.new`: the member read inside the body is the struct's own reader,
# which is why upstream also had to register the members (`record_meta_members`)
# when it started entering these bodies as class bodies.
Line = Struct.new(:text) do
  def shout
    text.upcase
  end
end

# d4 — `Data.define`, the same.
Point = Data.define(:x) do
  def dbl
    x * 2
  end
end

# d8 — with an explicit superclass argument.
Base = Class.new(StandardError) do
  attr_reader :b
end

# d10' — the BODY of the argument-bearing form. Its argument fires (row d10
# below); its body does not.
Plain = Class.new(parent_of(1)) do
  attr_reader :p
end

# d5 — a constant-PATH write. Silent on BOTH sides, for different reasons:
# upstream's `meta_new_constant_body_context` falls back to the anonymous key
# (`ConstantPathWriteNode` has no constant-keyed registration) and still enters
# the body; rigor-rs lowers it to the recovery carrier and never had a carve-out
# for it. Pinned so the two cannot silently diverge.
Outer::Inner = Class.new do
  attr_reader :z
end

# d9 — nested in a module: the enclosing scope is not toplevel either way.
module Wrap
  Inner2 = Class.new do
    attr_reader :w
  end
end

# --- FIRES: only the BLOCK BODY is a class scope ------------------------------

# d10 — the ARGUMENT position keeps the enclosing (toplevel) scope.
# (measured on the `Plain = …` line above, at its `parent_of` call)

# d11 — the #316 DSL block: a block on a call that is NOT a meta-new selector is
# not a class body, so both the call and the macro inside it fire.
some_dsl_call do
  attr_reader :d
end

# --- COVERAGE GAP (oracle fires, rigor-rs silent) — recorded, not chased ------
#
# In both spellings the constant's rvalue is NOT the meta-new call — it is a
# `.freeze` send, or an `||=` operator write — so upstream's
# `meta_new_constant_body_context` declines and the reference still fires inside.
# rigor-rs's span scan sees the inner `Class.new` block regardless and stays
# silent. Silence is never a false positive, and closing this needs the rvalue
# SHAPE test upstream has, which is a separate slice.

# d6
Frozen = Class.new do
  attr_reader :f
end.freeze

# d7
Lazy ||= Class.new do
  attr_reader :l
end
