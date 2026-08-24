# Effect PROPAGATION — labels are transitive across the project's own call
# graph, and the project is a CLOSED WORLD: a self-call joins the summary of
# every project-known override, not just the statically nearest one (ADR-103
# WD4). This fixture is where the difference between "direct" and "transitive"
# is observable: the snapshot records the former, the report prints the latter.

class Pipeline
  def run
    load_input
    transform
    emit
  end

  # Two hops down to an effect: `run` must carry `io.fs.read` even though it
  # never touches a file itself.
  def load_input
    parse(File.read("input.txt"))
  end

  def parse(text)
    text.split(",")
  end

  # A pure leaf. `run`'s summary must not gain anything from this edge.
  def transform
    [1, 2, 3].map { |n| n * 2 }
  end

  def emit
    puts "done"
  end
end

# --- overrides: the closed world joins every one --------------------------

class Sink
  def deliver(_payload)
    raise NotImplementedError
  end
end

class StdoutSink < Sink
  def deliver(payload)
    puts payload
  end
end

class FileSink < Sink
  def deliver(payload)
    File.write("out.txt", payload)
  end
end

class Dispatcher
  def initialize(sink)
    @sink = sink
  end

  # The receiver is typed `Sink`, so a call through it joins BOTH overrides'
  # summaries — the union, not the base's empty one.
  def dispatch(payload)
    @sink.deliver(payload)
  end
end

# --- a block literal always joins its enclosing method --------------------

class Deferred
  # Containment (WD4): the block's body joins `schedule`'s summary whether the
  # block is invoked now, later, or never. There is no edge INTO a deferred body
  # and no effect variable; the code is what counts, not the clock.
  def schedule
    later = proc { puts "ran" }
    later
  end

  def each_with_effect(items)
    items.each { |i| puts i }
  end
end

# --- recursion must terminate the fixpoint --------------------------------

class Recursive
  def walk(node)
    puts node
    walk(node) if node
  end

  def mutual_a
    mutual_b
  end

  def mutual_b
    Time.now
    mutual_a
  end
end
