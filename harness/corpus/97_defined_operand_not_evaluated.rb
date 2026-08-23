# `defined?` inspects its argument STATICALLY and never runs it, so nothing under
# its operand is reachable code and no receiver-typing diagnostic may fire there.
#
# Upstream #318 / `9e55deae`, shipped in `v0.3.4`: the three engine-owned tree
# walks that drive diagnostics used to descend into a `DefinedNode`'s value like
# any other expression. At the `v0.3.2` pin rigor-rs matched that; at `v0.3.4`
# the same output became false positives — 2 on the standing sweep
# (dependabot-core's `if defined?(git_dir)` idiom, in `update_checker_spec.rb`
# and `latest_version_finder_spec.rb`) plus the receiver-typed shapes below.
#
# Every firing line and every silent control is oracle-measured at the `v0.3.4`
# pin (`b10bd5df`), one fresh temp cwd per case, `--no-cache`, both reference libs
# pinned onto `-I` (UPSTREAM.md hazard 1).

# --- STAYS SILENT: the operand is not evaluated ------------------------------

# (1) an undefined method on a KNOWN receiver type.
literal = "abc"
defined?(literal.frobdefined)

# (2) an implicit-self call as the operand — the dependabot-core shape.
if defined?(some_helper_method)
  nil
end

# (3) Ruby's BARE `defined?` binds lower than `&&`, so Prism hands back the whole
# right-hand side as the operand: all of it is dead code under any single-node
# reading, which is the false positive the upstream issue cites.
def bare_form_swallows_the_conjunction
  value = "abc"
  defined? value && value.frobbare
end

# (4) nested inside another expression the lowering does not own.
def wrapped_in_an_unhandled_node(other)
  [defined?(other.frobwrapped)]
end

# --- STAYS SILENT: a read under the operand is still a read ------------------

# (5) the reference's `DeadAssignmentCollector` does its OWN recursion, which
# still descends into a `DefinedNode` — #318 touched the three engine walks only.
# So the operand's local read keeps the assignment alive.
def only_read_inside_defined
  bound = 1
  defined?(bound)
end

# (6) and so does a read inside a call the operand suppresses.
def read_inside_a_suppressed_call
  bound = 1
  defined?(some_helper_method(bound))
end

# --- FIRES: outside the operand, everything is live code ---------------------

# (7) the PARENTHESISED guard leaves the second call outside the operand, where
# it is real, reachable code — this follows from the parse, not from the rule.
def parenthesised_form_leaves_the_second_call_live(value)
  defined?(value) && "lit".frobparened
end

# (8) an assignment with no read anywhere is still dead.
def genuinely_dead_assignment
  unused = 1
  nil
end
