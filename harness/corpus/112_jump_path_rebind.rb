# A local rebound on a `next` / `break` path (rigor-rs#133; upstream #1248 for
# `while` / `until` / `for`, #1215 for blocks).
#
# The reference joins every path that leaves a loop body or a block invocation
# — the fall-through AND each `next` / `break` — into the continuation, so a
# rebind on a jumping branch reaches the read after the construct. rigor-rs
# types a top-level read from a flat straight-line env that never saw a rebind
# nested in an `if`, a loop or a block, so it kept the FIRST binding and fired
# `for String`. `Typer::build_toplevel_check_env` now widens such a local
# instead: the rebound rows go silent and the no-rebind controls still fire.
#
# Every row sits at TOP LEVEL (inside a `def` the port reads no top-level env,
# so it is silent on both halves there), and every row uses its own locals: the
# file is one scope.
#
# Measured against the pinned reference (e59b7b89, Ruby 4.0, fresh cwd,
# `--no-cache`).

def work_133(x) = x

# --- `while` / `until` / `for`: a rebind on a `next` path (#1248) ------------

# (1) the issue's loop row. Reference: `w1` is `String | Integer` — silent.
w1 = String.new
i1 = 0
while i1 < 3
  i1 += 1
  if i1.odd?
    w1 = i1
    next
  end
end
w1.even?

# (2) control: the same loop, `next` with NO rebind — still fires `for String`.
w2 = String.new
i2 = 0
while i2 < 3
  i2 += 1
  next if i2.odd?
end
w2.even?

# (3) `until` twin of (1). Silent.
w3 = String.new
i3 = 0
until i3 >= 3
  i3 += 1
  if i3.odd?
    w3 = i3
    next
  end
end
w3.even?

# (4) `for` twin of (1). Silent.
w4 = String.new
for i4 in [1, 2, 3]
  if i4.odd?
    w4 = i4
    next
  end
end
w4.even?

# (5) control: `for` with `next` and no rebind — fires.
w5 = String.new
for i5 in [1, 2, 3]
  next if i5.odd?
end
w5.even?

# --- blocks: a rebind on a `next` path (#1215) -------------------------------

# (6) the issue's block row. Silent.
n6 = String.new
[1, 2, 3].each { |e| if e.odd?; n6 = e; next; end }
n6.even?

# (7) control: the block with `next` and no rebind — fires.
n7 = String.new
[1, 2, 3].each { |e| next if e.odd? }
n7.even?

# (8) a `next` inside `begin … ensure`: the `ensure` rebind is on the path that
# leaves. The reference reads `nil | String` and reports possible-nil — a row
# the port does not model (a gap); it must not report `undefined method … for
# nil` as it did before.
b8 = nil
[1, 2].each do |c|
  begin
    next if c.odd?
  ensure
    b8 = +"reset"
  end
end
b8.upcase

# --- flags that must not fold always-falsey -----------------------------------

# (9) `rescue …; failed = true; next` in a loop (#1248). Silent.
failed9 = false
i9 = 0
while i9 < 3
  i9 += 1
  begin
    work_133(i9)
  rescue StandardError
    failed9 = true
    next
  end
end
if failed9
  puts "failed"
end

# (10) control: the same loop with no flag write — `if failed10` folds.
failed10 = false
i10 = 0
while i10 < 3
  i10 += 1
  begin
    work_133(i10)
  rescue StandardError
    next
  end
end
if failed10
  puts "failed"
end

# (11) `found = nil; each { … found = x; break }` (#1215). Silent.
found11 = nil
[1, 2, 3].each { |x| if x > 1; found11 = x; break; end }
if found11
  puts "found"
end

# (12) control: `break` with no rebind — `if found12` folds always-falsey.
found12 = nil
[1, 2, 3].each { |x| break if x > 1 }
if found12
  puts "found"
end

# (13) `break(flag = true)` that only a LATER iteration reaches (#1248). Silent.
flag13 = false
i13 = 0
while i13 < 3
  i13 += 1
  break(flag13 = true) if i13 == 2
end
if flag13
  puts "flag"
end

# (14) control: a bare `break` — `if flag14` folds always-falsey.
flag14 = false
i14 = 0
while i14 < 3
  i14 += 1
  break if i14 == 2
end
if flag14
  puts "flag"
end

# (15) a block's `break(flag = true)` (#1215). Silent.
flag15 = false
[1, 2, 3].each { |x| break(flag15 = true) if x == 2 }
if flag15
  puts "flag"
end

# --- the same flat-env defect without a jump ----------------------------------

# (16) a conditional rebind in a plain `if`. Reference: `String | 1` — silent.
w16 = String.new
w16 = 1 if $stdin.tty?
w16.even?

# (17) an operator write: `1 + 1.5` is a Float, and `Float#nan?` exists.
w17 = 1
w17 += 1.5
w17.nan?

# (18) a straight-line rebind AFTER the nested one re-establishes the type:
# fires `for "t"`.
w18 = "s"
w18 = 1 if $stdin.tty?
w18 = "t"
w18.frobnicate_133
