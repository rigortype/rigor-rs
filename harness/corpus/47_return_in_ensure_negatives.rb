# `flow.return-in-ensure` decline cases — a `return` that belongs to an inner
# frame, or an ensure with no explicit return, stays silent.

class Worker
  # A nested `def` opens a new frame — its `return` is not the method's.
  def with_nested_def
    do_work
  ensure
    def helper; return 1; end
  end

  # A lambda (`lambda {}` / `-> {}`) and a `define_method` block are barriers.
  def with_lambda
    do_work
  ensure
    lambda { return 1 }
  end

  def with_arrow
    do_work
  ensure
    -> { return 1 }
  end

  def with_define_method
    do_work
  ensure
    define_method(:x) { return 1 }
  end

  # An ensure with no explicit `return` (implicit values are discarded safely).
  def clean_ensure
    do_work
  ensure
    cleanup
  end
end
