# `Class.new do ... end` and friends evaluate their block as the created class's
# OWN body (`class_eval` semantics), so `self` inside it is that class and
# `call.unresolved-toplevel` cannot fire there.
#
# Upstream #319 / `189c498b`, shipped in `v0.3.4`: the reference had always known
# this through its `ConstantWriteNode` branch (the constant supplied the name the
# discovery tables are keyed by), and away from that position the body fell
# through to the enclosing — at file top level, TOPLEVEL — scope. At the `v0.3.2`
# pin rigor-rs matched that; at `v0.3.4` the same output became 48 false positives
# across the standing sweep (dependabot-core `base_spec.rb` x40, concurrent-ruby
# `erlang_actor_spec.rb` x8 — RSpec's `Class.new(described_class) do ... end` and
# `Module.new do ... end` idioms).
#
# Every firing line and every silent control below is oracle-measured at the
# `v0.3.4` pin (`b10bd5df`), one fresh temp cwd per case, `--no-cache`, both
# reference libs pinned onto `-I` (UPSTREAM.md hazard 1).

# --- STAYS SILENT: the block body is a class body ---------------------------

# (1) local-variable rvalue — the position #319 fixed.
observer_class = Class.new do
  attr_reader :name

  def initialize(name)
    @name = name
  end
end

# (2) with a superclass: still a class body.
error_class = Class.new(StandardError) do
  attr_writer :code
end

# (3) `Module.new` — the same `class_eval` semantics.
mixin = Module.new do
  attr_accessor :flag

  def helper
    some_class_level_macro
  end
end

# (4) `Struct.new` / `Data.define`: the block is `class_eval`'d on the generated
# class, so the reference lists them in the same `META_NEW_SELECTORS` table.
pair = Struct.new(:a, :b) do
  another_macro
end

point = Data.define(:x) do
  yet_another_macro
end

# (5) bare expression position — no assignment at all.
Class.new do
  attr_reader :anonymous
end

# (6) instance-variable rvalue.
@cached_class = Class.new do
  attr_reader :cached
end

# --- FIRES: the reference's own asymmetry ------------------------------------

# (7) a CONSTANT-assigned body still fires. `ScopeIndexer` keys it by the
# constant, and `StatementEvaluator` never routes a constant-write rvalue through
# the block-body narrowing #319 added, so `self` there is still `Dynamic[top]`
# and `Scope#toplevel?` still holds. Measured firing at the `v0.3.4` pin.
Registry = Class.new do
  attr_reader :entries
end

Coercible = Module.new do
  attr_reader :raw
end

# --- FIRES: only the BLOCK BODY is a class scope -----------------------------

# (8) the ARGUMENTS keep the enclosing scope.
scoped = Class.new(parent_class_lookup) do
  attr_reader :scoped
end

# (9) and so does everything after the block.
trailing_unresolved_call
