# Repeatable-body write back-edges — the reference's `BodyFixpoint`
# (body_fixpoint.rb): `eval_loop`'s `loop_pass_entry` for `while`/`until` and
# `write_back_block_captures` for a `:non_escaping` literal block
# (ClosureEscapeAnalyzer, closure_escape_analyzer.rb).
#
# A body that can run more than once presents `entry ∪ post` to its earlier
# reads on a later pass, so a write LATER in the body reaches reads before
# it: `x = nil; [1,2].each { |i| x.frob; x = "s" }` reads `nil | "s"`. The
# union is over the POST binding — a straight-line write shadows an earlier
# straight-line write to the same name (`x = "s"; x = 2.5` yields `1 | 2.5`,
# never `"s"`), and a receiver-side mutator is not a capture
# (`x.frob; x << "b"` keeps the `"a"` pin). On exit the captured rebinds
# write back the same union and a `while`/`until` narrows the outer env onto
# its EXIT edge — `until x.nil?` leaves `x` bound `nil`.
#
# A `for` (`eval_for` is a single pass) and a call whose block is not proven
# `:non_escaping` (`.map`/`.select` here, or any unknown callee) keep the
# single-pass reading; a local FIRST assigned inside the body is not
# overlaid (`loop_pass_entry`'s `body_first` exclusion).
#
# Every firing line and every silent row is oracle-measured at the
# `e59b7b89` pin, one fresh temp cwd per case, `--no-cache` (UPSTREAM.md
# hazard 1).

# --- (1) the back-edge union reaches earlier body reads -----------------------

# `nil | "s"` in the body — `possible-nil-receiver`, both engines.
def c1
  x = nil
  [1,2].each { |i| x.upcase; x = "s" }
end

# `"s" | 1` — the union fires `undefined-method` when the method is absent
# on every arm.
def c2
  x = 1
  [1,2].each { |i| x.frob; x = "s" }
end

# A later straight-line write supersedes an earlier one — `1 | 2.5`, never
# `"s"`.
def c3
  x = 1
  [1,2].each { |i| x.frob; x = "s"; x = 2.5 }
end

# A mutator is not a capture: `x << "b"` does not rebind `x`, so the earlier
# read keeps the `"a"` pin.
def c4
  x = "a"
  [1,2].each { |i| x.frob; x << "b" }
end

# `while`/`until` bodies run on the predicate's RUN edge — `while x.nil?`
# binds `x` to `nil` inside.
def c5
  x = nil
  while x.nil?
    x.upcase
    x = "s"
  end
end

# `until x.nil?` runs on the FALSEY edge — `x` reads `"s"` inside, so `frob`
# names it.
def c6
  x = nil
  until x.nil?
    x.frob
    x = "s"
  end
end

# `loop do` is a non-escaping `Kernel#loop` block — the same fixpoint.
def c7
  x = nil
  loop do
    x.frob
    x = "s"
  end
end

# --- (2) the exit write-back --------------------------------------------------

# `entry ∪ post` outlives the body: `nil | "s"` still fires
# `possible-nil-receiver` on the read after the block/loop.
def d1
  x = nil
  [1,2].each { |i| x = "s" }
  x.upcase
end

def d2
  x = nil
  while true
    x = "s"
  end
  x.upcase
end

# `until x.nil?` exits on the TRUTHY edge — `x` is `nil` afterwards and the
# read folds `for nil`.
def d3
  x = nil
  until x.nil?
    x = "s"
  end
  x.upcase
end

# The last write wins on exit too: `nil` post ∪ `String` entry —
# `possible-nil-receiver`.
def d4
  x = "a".upcase
  [1,2].each { |i| x = 1; x = nil }
  x.upcase
end

# --- (3) STAYS SILENT ---------------------------------------------------------

# `until x.nil?` body reads the `"s"` arm — `upcase` is defined there.
def e1
  x = nil
  until x.nil?
    x.upcase
    x = "s"
  end
end

# `while x.nil?` exits `x` non-nil — the post-loop read sees only `"s"`.
def e2
  x = nil
  while x.nil?
    x = "s"
  end
  x.upcase
end

# `for` keeps the single-pass reading (`eval_for` has no fixpoint): the
# body read still witnesses the entry `nil` pin, so this row FIRES
# `for nil` on both engines — a control that `while`/`until`-only
# machinery did not leak into `for`.
def e3
  x = nil
  for i in [1,2]
    x.upcase
    x = "s"
  end
end

# `map`/`select` are `:non_escaping` too — the same `nil | "s"` union — and
# `upcase` is defined on the non-nil arm, so `possible-nil-receiver` fires.
def e4
  x = nil
  [1,2].map { |i| x.upcase; x = "s" }
end

def e5
  x = nil
  [1,2].select { |i| x.upcase; x = "s" }
end

# A method undefined on EVERY arm of a nil-bearing union is silent on both
# engines: `undefined-method` defers a `nil` member to `possible-nil`, whose
# arm gate declines `frob` (missing on `String` as well).
def e5b
  x = nil
  [1,2].each { |i| x.frob; x = "s" }
end

# A last write that is NOT nil keeps `upcase` defined — `String | 1` is
# silent because `String` answers `upcase` (the union rule only fires when
# every arm lacks the method).
def e6
  x = "a".upcase
  [1,2].each { |i| x = nil; x = 1 }
  x.upcase
end

# --- (4) retry back-edges ------------------------------------------------------
#
# `eval_begin`'s `RetryWidening` collects each `retry` clause's writes up to
# the retry and unions them into the body's re-entry scope.

# No retrying-clause write — the body pin is undisturbed, `upcase` on nil
# fires on both engines.
def f1
  x = nil
  begin
    x.upcase
  rescue
    retry
  end
end

# `x = "s"` before `retry` converges the body read to `nil | "s"` — `frob`
# is undefined on `String`, so `undefined-method` fires on both engines.
def f2
  x = nil
  begin
    x.frob
  rescue
    x = "s"
    retry
  end
end

# The union reaches back across the begin body — a post-entry `x = 1`
# pin and the clause's `"s"` write converge to `"s" | 1`.
def f3
  x = 1
  begin
    x.frob
  rescue
    x = "s"
    retry
  end
end

# `upcase` on the `nil | "s"` union is a `possible-nil-receiver` on both
# engines — the fact survives the `nil | C` single-class arm.
def f4
  x = nil
  begin
    x.upcase
  rescue
    x = "s"
    retry
  end
end
