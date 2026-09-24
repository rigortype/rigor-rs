# `class_eval` / `module_eval` / `class_exec` / `module_exec` / `instance_eval` /
# `instance_exec` evaluate their LITERAL block with `self` rebound to the
# receiver, so the block body is morally a class body and
# `call.unresolved-toplevel` (ADR-34) declines inside it — on a genuinely
# undefined name too.
#
# Upstream #1135 (`ee33407e` + `f918c6f0`, pin `e59b7b89`):
# `CheckRules#receiver_eval_block_ranges` collects the `BlockNode` range of every
# call NAMED in `RECEIVER_EVAL_CALL_NAMES`, and the rule declines any receiverless
# call whose offsets lie inside one. The test is name + offsets only: the receiver
# shape is irrelevant (constant, local, none), `{}` and `do ... end` alike, and a
# call inside a `def` or a nested block within the eval block is covered. The
# motive on the rigor-rs side: 9 standing-sweep false positives in rspec's
# `minitest_integration.rb` (`Minitest::Test.class_eval do include
# ::RSpec::Matchers ... end`), section (1).
#
# Every firing line and silent control is oracle-measured at the `e59b7b89` pin,
# one fresh temp cwd per case, `--no-cache`.

class Target; end

# --- STAYS SILENT: inside the literal eval block -----------------------------

# (1) the rspec shape — a constant-path receiver, `include` + a def.
Minitest::Test.class_eval do
  include ::RSpec::Matchers
  undefined_in_class_eval_block

  def assertions
    helper_inside_eval_def
  end
end

# (2) every selector, constant receiver, `do ... end`.
Target.module_eval do
  undefined_in_module_eval
end
Target.class_exec do
  undefined_in_class_exec
end
Target.module_exec do
  undefined_in_module_exec
end
Target.instance_eval do
  undefined_in_instance_eval
end
Target.instance_exec do
  undefined_in_instance_exec
end

# (3) a LOCAL receiver — no static identity needed (unlike `Class.new`).
target = Target
target.class_eval do
  undefined_via_local_receiver
end

# (4) brace blocks.
Target.class_eval { undefined_in_brace_block }
target.instance_exec { undefined_in_brace_instance_exec }

# (5) a nested block inside the eval block is still inside its range.
Target.class_eval do
  [1].each { undefined_in_nested_block }
end

# (6) NO receiver at all — the bare `class_eval` itself fires (next section),
# but its block body is still declined: the reference keys on the name only.
class_eval do
  undefined_in_receiverless_eval_block
end

# --- STILL FIRES (must-still-fire controls) ----------------------------------

# (7) a bare call at real toplevel.
undefined_at_real_toplevel

# (8) inside a toplevel `def` body — still toplevel for ADR-34.
def toplevel_helper
  undefined_in_toplevel_def
end

# (9) the eval call's ARGUMENTS keep the enclosing scope; only the block counts.
Target.class_eval(undefined_in_eval_argument) do
  1
end

# (10) the string form has no block; a block-pass is not a `BlockNode`.
Target.class_eval("1")
Target.class_eval(&undefined_block_pass_operand)

# (11) a same-named receiverless call AFTER the eval block is outside it.
undefined_after_eval_block
