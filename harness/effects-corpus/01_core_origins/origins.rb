# Effect ORIGINS — the two sources upstream ADR-103 WD3 admits: rows of the
# hand-audited effect catalogue (`data/effects/core.yml`) and the small set of
# Ruby constructs. Every method here is DIRECT: it calls nothing in the project,
# so its summary is its own contribution and nothing else, which is what makes
# this fixture a collector test rather than a propagation test.

class Origins
  # --- catalogue rows -------------------------------------------------------

  def write_stdout
    puts "hello"
  end

  def read_clock
    Time.now
  end

  def read_file(path)
    File.read(path)
  end

  def write_file(path, body)
    File.write(path, body)
  end

  def read_env
    ENV["HOME"]
  end

  def random_number
    rand(10)
  end

  # A constructor call whose arguments make it effect-free: `Time.new(2020, 1, 1)`
  # builds a fixed time where bare `Time.new` reads a clock. Upstream's catalogue
  # is argument-aware here; this line is the fixture that says so.
  def fixed_time
    Time.new(2020, 1, 1)
  end

  # --- constructs -----------------------------------------------------------

  def ivar_write
    @cached = 1
  end

  def ivar_memo
    @memo ||= 2
  end

  def gvar_read
    $stdout
  end

  def gvar_write
    $PROGRAM_STATE = :running
  end

  def cvar_write
    @@count = 0
  end

  def subprocess
    `echo hi`
  end

  # --- the empty case -------------------------------------------------------

  # Pure by construction: reads its arguments, allocates, returns. The summary
  # must be the empty set AND exhaustive — the only shape an envelope of `pure`
  # can ever be satisfied by.
  def pure_arithmetic(a, b)
    (a + b) * 2
  end

  # Mutating an object the frame OWNS (fresh, unescaped) is `mutate.local`,
  # which every envelope tolerates (WD4). Distinguishing it from `mutate.arg` is
  # the ownership half of the model.
  def owns_what_it_mutates
    buffer = []
    buffer << 1
    buffer
  end

  # Mutating an ARGUMENT is not the same thing.
  def mutates_its_argument(list)
    list << 1
    list
  end
end
