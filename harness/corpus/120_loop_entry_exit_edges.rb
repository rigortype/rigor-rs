# `while`/`until` loop-entry and loop-exit edges (upstream `900bc64c`,
# rigor#1359, `eval_loop`'s `loop_pass_entry` / `narrow_loop_exit_edge`).
#
# The reference enters a `while` body on the predicate's TRUTHY edge and an
# `until` body on its FALSEY edge (a `begin … end while` body runs once
# unnarrowed), joins the body's end scope into the post-predicate scope, and
# narrows the join on the EXIT edge (falsey for `while`, truthy for `until`).
# No rigor-rs engine models either edge, so a predicate local whose edge does
# not keep its carrier is widened there instead (`loop_edges.rs`): inside the
# body on the entry edge, after the loop on the exit edge. Every row is
# oracle-measured at the `v0.4.0` pin.

# --- inside a `def` (the collection-shape pass) ---------------------------

# (1) an unorderable guard widens the entry edge, and the widened body end
# joins back in: silent after the loop. (The `v0.3.x` pins fired `a2`.)
def a
  h = Array.new
  while h.is_a?(UnknownZzzClass)
    h.frob_a1
  end
  h.frob_a2
end

# (2) MUST STILL FIRE: a disjoint guard makes the body unreachable (`Bot` is
# the join identity) and the falsey exit edge keeps `Array`.
def b
  h = Array.new
  while h.is_a?(String)
    h.frob_b1
  end
  h.frob_b2
end

# (3) `until`'s exit edge is the guard's TRUTHY edge: widened, silent. The
# port fired `e2` on every earlier pin; the reference never did.
def e
  h = Array.new
  until h.is_a?(UnknownZzzClass)
    h.frob_e1
  end
  h.frob_e2
end

# (4) MUST STILL FIRE: no predicate local, nothing narrows.
def f
  h = Array.new
  while rand > 0.5
    h.frob_f1
  end
  h.frob_f2
end

# (5) `until` on a subclass-or-equal guard: the entry edge is unreachable and
# the truthy exit edge keeps `Array` — MUST STILL FIRE.
def g
  h = Array.new
  until h.is_a?(Array)
    h.frob_g1
  end
  h.frob_g2
end

# (6) `while` on the same guard: the falsey exit edge is `Bot` — silent.
def i
  h = Array.new
  while h.is_a?(Array)
    h.frob_i1
  end
  h.frob_i2
end

# (7) a bare truthiness test: `while h` exits on `h`'s falsey edge, which an
# `Array` never takes — silent; `until h` exits on the truthy edge — fires.
def j
  h = Array.new
  while h
  end
  h.frob_j2
end

def k
  h = Array.new
  until h
  end
  h.frob_k2
end

# (8) `begin … end until`: the exit edge alone decides (widened, silent);
# `begin … end while` on the same guard keeps `Array` on its falsey exit
# edge and fires — the body ran unnarrowed.
def l
  h = Array.new
  begin
    h.frob_l1
  end until h.is_a?(UnknownZzzClass)
  h.frob_l2
end

def m
  h = Array.new
  begin
    h.frob_m1
  end while h.is_a?(UnknownZzzClass)
  h.frob_m2
end

# (9) a loop nested in a loop body: the inner exit edge still applies after
# the outer loop.
def n
  h = Array.new
  while rand > 0.5
    until h.is_a?(UnknownZzzClass)
    end
  end
  h.frob_n2
end

# (10) a `for` loop narrows nothing: MUST STILL FIRE.
def o
  h = Array.new
  for x in [1]
    h.frob_o1
  end
  h.frob_o2
end

# --- file scope (the check env) -------------------------------------------

# (11) body sites read the loop-entry edge: a disjoint guard's body is
# unreachable, an unorderable one widened, `until t3` never enters.
t1 = Array.new
while t1.is_a?(String)
  t1.frob_t1
end
t2 = Array.new
while t2.is_a?(UnknownZzzClass)
  t2.frob_t2
end
t3 = Array.new
until t3
  t3.frob_t3
end

# (12) after the loop, until the local's next write.
t4 = [1, 2]
until t4.is_a?(UnknownZzzClass)
end
t4.frob_t4a
t4 = Array.new
t4.frob_t4b

# (13) MUST STILL FIRE: a `begin … end while` body runs before the first test.
t5 = Array.new
begin
  t5.frob_t5a
end while t5.is_a?(String)
t5.frob_t5b

# (14) `flow.always-truthy-condition`: `while t6` exits on `t6`'s falsey
# edge, so the post-loop `if t6` does not fold.
t6 = "x"
while t6
end
if t6
  puts 1
end
