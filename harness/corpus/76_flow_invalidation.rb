# Three flow facts the reference invalidates and rigor-rs did not, each measured
# as a false positive on rigor-survey. The NEGATIVE CONTROLS matter as much as
# the positives: each one is a shape the reference still folds, so widening it
# would be a coverage loss the other fixtures would not catch.

# --- (1) `flow.dead-assignment` does not look inside the parameter list -------
# The reference's DeadAssignmentCollector gathers writes from `def_node.body`
# only. rigor-rs lowers parameter DEFAULTS into the arena (so the call rules see
# `def f(t = Time.current)`), which put this write inside the def's span.
# rigor-survey `rspec-benchmark-0.6.0/lib/rspec/benchmark/complexity_matcher.rb:50`.
def in_range(start, limit = (not_set = true))
  [start, limit]
end

# NEGATIVE CONTROL: a dead write in the BODY still fires.
def body_dead_write(a)
  unused_here = a
  a
end

# --- (2) an argument the callee mutates in place widens the caller's local ----
# rigor-survey `rspec-core-3.13.6/lib/rspec/core/world.rb:179` — the array is
# handed to a method that shovels into it, so its `[]`-pinned length is stale.
def harness_fill(acc)
  acc << "x"
end

def harness_pure(acc)
  acc.size
end

def harness_second(flag, acc)
  acc.push(flag)
end

def announces
  found = []
  harness_fill found
  # SILENT: `found` was mutated through the argument.
  if found.length == 1
    1
  end
end

# NEGATIVE CONTROL: the callee only READS the argument, so the fold stands and
# the diagnostic must still fire on BOTH tools.
def announces_pure
  found = []
  harness_pure found
  if found.length == 1
    1
  end
end

# NEGATIVE CONTROL: position matters — `harness_second` mutates parameter 1, so
# passing the local at position 0 must NOT widen it.
def announces_wrong_position
  found = []
  harness_second found, 5
  if found.length == 1
    1
  end
end

# --- (3) a `rescue => e` inside a BLOCK body widens the outer local ----------
# rigor-survey `net-imap-0.6.4.1/lib/net/imap.rb:1470` — `error = nil`, the
# `send_command` block rescues into `error`, and `if error` was folded to nil.
def harness_yielder
  yield 1
end

def starttls_shape
  error = nil
  harness_yielder do |resp|
    resp
  rescue StandardError => error
    raise
  end
  # SILENT: the block may have bound `error`.
  if error
    raise error
  end
end

# NEGATIVE CONTROL: a METHOD-level `begin`/`rescue => e` is NOT a block, and the
# reference keeps folding it — so this one must still fire on both tools.
def method_level_rescue
  error = nil
  begin
    1
  rescue StandardError => error
    raise
  end
  if error
    2
  end
end
