# A certainty-folded `if`/`unless` replays its LIVE arm as straight-line
# code, and a real `begin`/`rescue` statement inside that arm is itself an
# effective live statement: `eval_begin`'s exit scope joins the primary
# path's (the protected body's — or the `else` clause's, which replaces
# it) with every LIVE rescue arm's, nil-injecting a name an arm does not
# bind. `live_rescue_results` drops a rescue arm that unconditionally
# exits; the primary path contributes even when it ends in `raise`.
#
# A rescue arm runs after the body raised from any point in it, but
# `ClauseWidening` only rebinds a body-written name to `Dynamic[top]` in
# the arm's entry when the arm MENTIONS it or the body writes it more
# than once — a name written once and never mentioned leaves the arm at
# its entry binding, which the join pairs with the body's exit binding.
#
# Every row is oracle-measured at the current pin; #953 rows.

# --- FIRES on both sides: the arm joins ------------------------------------

# (1) the issue row: the rescue clause's write joins the body's scope.
x = "a"
if true
  x = "s"
  begin
    raise
  rescue
    x = "t"
  end
end
x.w

# (2) a write the protected body made BEFORE raising joins too.
x = "a"
if true
  begin
    x = "b"
    raise
  rescue
    x = "t"
  end
end
x.w

# (3) a body that completes normally joins the same way.
x = "a"
if true
  begin
    x = "b"
  rescue
    x = "t"
  end
end
x.w

# (4) an `else` clause REPLACES the primary path's contribution (the body
# writes never reach it on the normal path the else models).
x = "a"
if true
  begin
    x = "b"
    raise
  rescue
    x = "t"
  else
    x = "e"
  end
end
x.w

# (5) an `ensure` runs on the joined scope: a name it does not touch
# keeps the join; its own write binds.
x = "a"
if true
  begin
    x = "b"
  rescue
    x = "t"
  ensure
    y = "n"
  end
end
x.w
y.w

# (6) a mutator call in the live rescue arm mints the nominal in that
# arm's scope and joins with the body's path.
x = "a"
if true
  begin
    raise
  rescue
    x << "t"
  end
end
x.w

# (7) a rescue arm that unconditionally exits contributes NOTHING — the
# later live arm joins alone (the first clause is also shadowed, so the
# shadowed-rescue warning fires on both sides).
x = "a"
if true
  begin
    x = "b"
    raise
  rescue
    x = "t"
    raise
  rescue
    x = "u"
  end
end
x.w

# (8) two live clauses both join.
x = "a"
if true
  x = "s"
  begin
    raise
  rescue FooError
    x = "t"
  rescue
    x = "u"
  end
end
x.w

# (9) ClauseWidening: a name the body writes ONCE and the arm never
# mentions keeps its entry binding in the arm's scope — the join pairs
# it with the body's write.
x = "a"
if true
  begin
    x = "b"
  rescue
    nil
  end
end
x.w

# --- SILENT on both sides --------------------------------------------------

# (10) a name the body writes TWICE reads untyped in the arm's entry —
# the join declines.
x = "a"
if true
  begin
    x = "b"
    x = "c"
  rescue
    nil
  end
end
x.w

# (11) a name the arm MENTIONS reads untyped in its entry (even read
# only: `y = x`), and the arm's exit scope binds it untyped.
x = "a"
if true
  begin
    x = "b"
  rescue
    y = x
  end
end
x.w
y.w

# (12) a name bound ONLY in a rescue arm nil-injects at the join.
if true
  begin
    raise
  rescue
    y = "t"
  end
end
y.w

# (13) `rescue => e` binds the exception inside the arm only.
if true
  begin
    raise
  rescue => e
  end
end
e.w

# (14) a `retry` in the chain re-enters the `begin`: the retry edge's
# widening is unmodelled — every name the `begin` writes declines.
x = "a"
if true
  x = "s"
  begin
    raise
  rescue
    x = "t"
    retry
  end
end
x.w

# (15) must-still-fire control: when the `begin` never completes (body
# and every rescue arm exit), the folded arm's termination leaves the
# entry binding.
x = "a"
if true
  x = "s"
  begin
    raise
  rescue
    x = "t"
    raise
  end
end
x.w
