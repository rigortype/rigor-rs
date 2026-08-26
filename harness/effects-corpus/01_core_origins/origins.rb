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

  # NOT `mutate.local` — measured, this is `effects: []` + `exhaustive: false`
  # + `causes: [["unknown-ownership", null]]`, and the fixture is correct as
  # recorded. The method's TAIL is a bare `buffer` read, and `LocalOwnership`'s
  # `trailing_reads` counts that as an escape (a body whose value is a local
  # hands it to the caller), so the frame does not own it after all.
  #
  # `mutate.local` IS reachable — `s = +""; s.upcase!; nil` proves it, and a
  # `; nil` tail is the whole difference — but no method in this corpus produces
  # one. Growing that coverage is ADR-0043 slice 2's corpus work, not a change
  # to this method: what it pins is the CONSERVATIVE direction of the ownership
  # rule, which is worth a fixture of its own.
  def owns_what_it_mutates
    buffer = []
    buffer << 1
    buffer
  end

  # NOT `mutate.instance` either, and for a different reason: `list` is an
  # untyped parameter, so the typer names no receiver class, so `mutating?`
  # declines `<<` — it is a mutator only on a KNOWN Array / Hash / String,
  # because `n << 2` is a bit shift and `io << "x"` is output. Measured:
  # `effects: []` + a `dynamic-receiver` cause. `list[0] = 1` would reach the
  # `mutate.instance` case, since `[]=` claims a write on every receiver.
  def mutates_its_argument(list)
    list << 1
    list
  end
end
