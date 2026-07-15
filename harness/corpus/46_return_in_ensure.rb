# `flow.return-in-ensure` fires on an explicit `return` lexically inside an
# `ensure` clause body — it discards the method's in-flight return value and
# swallows any in-flight exception. Purely syntactic with a frame-aware
# envelope. Anchored on the `return` keyword. Inside a class so the protected
# bodies (implicit-self calls) draw no other diagnostic.

class Worker
  # A bare `return` in the ensure of a `def` body.
  def perform
    do_work
  ensure
    return :done
  end

  # A `return` inside a PLAIN block still exits the method — fires.
  def each_item
    do_work
  ensure
    items.each { return }
  end

  # A `proc` block is deliberately NOT a barrier — fires.
  def with_proc
    do_work
  ensure
    proc { return 1 }
  end

  # Two returns in one ensure fire twice.
  def twice
    do_work
  ensure
    return 1
    return 2
  end
end

# Works at a top-level `begin`/`ensure` too (no enclosing def needed).
begin
  do_work
ensure
  return
end
