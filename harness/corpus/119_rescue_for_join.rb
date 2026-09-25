# The reference JOINS a local written in a `rescue` modifier, a `for` index,
# or a `begin`/`rescue` arm with the state before the construct — nil-injected
# scope joins (`join_with_nil_injection`) — where the #154 port widened the
# local to `Dynamic[top]` (rigor-rs#167). One scope: every row uses its own
# locals. Measured against the pinned reference (e59b7b89, fresh cwd,
# `--no-cache`); every row below is a full-tuple match.

# --- rescue modifier --------------------------------------------------------

# (1) the issue row: `entry | write` — fires `for [5 | 6]`.
r1 = 5
(r1 = 6) rescue nil
[r1].frob

# (2) same constant collapses the union — fires `for [5]`.
r2 = 5
(r2 = 5) rescue nil
[r2].frob

# (3) the join renders inside a hash shape, a tuple, and a nested tuple —
# fires `for { a: 5 }`, `for [5, 1]`, `for [[5]]`.
r3 = 5
(r3 = 5) rescue nil
{ a: r3 }.frob
[r3, 1].frob
[[r3]].frob

# (4) the joined element feeds an argument-type-mismatch message —
# `expected Numeric, got [5]`.
r4 = 5
(r4 = 5) rescue nil
1.fdiv([r4])

# (5) the joined element survives a tuple `first` fold — fires `for 5`.
r5 = 5
(r5 = 5) rescue nil
[r5].first.frob

# (6) the rescue arm's own write joins the protected arm's — fires `for [6 | 7]`.
r6 = 5
(r6 = 6) rescue (r6 = 7)
[r6].frob

# (7) nested rescue modifiers join through — fires `for [5 | 6]`.
r7 = 5
((r7 = 6) rescue nil) rescue nil
[r7].frob

# (8) a rescue modifier in VALUE position binds its joined result —
# fires `for [6 | 7]`.
r8 = (6 rescue 7)
[r8].frob

# (9) a rescue modifier inside an interpolation is a carrier too —
# fires `for [5 | 6]`.
r9 = 5
"#{r9 = 6}" rescue nil
[r9].frob

# (10) union receiver: the method is absent on BOTH joined members —
# fires `for "s" | 1` (the must-still-fire control).
r10 = "s"
(r10 = 1) rescue nil
r10.frob

# (11) control: `upcase`/`downcase` exist on the String member — silent.
r11 = "s"
(r11 = 1) rescue nil
r11.upcase
r11.downcase

# (12) control: a HOMOGENEOUS union (`5 | 6`, one class) stays silent,
# and the #151 FP does not return — `w.upcase` must not fire `for 1`.
r12 = 5
(r12 = 6) rescue nil
r12.frob

# --- operator writes --------------------------------------------------------

# (13) `w op= v` folds a value-pinned receiver — fires `for [6]`.
o13 = 5
o13 += 1
[o13].frob

# (14) `w ||= v` unions the truthy fragment — `5 | 6`; a `nil` receiver
# contributes nothing — `6`.
o14a = 5
o14a ||= 6
[o14a].frob
o14b = nil
o14b ||= 6
[o14b].frob

# (15) `w &&= v` unions the FALSEY fragment — `6?` for a `nil` receiver,
# `6` for a truthy one.
o15a = nil
o15a &&= 6
[o15a].frob
o15b = 5
o15b &&= 6
[o15b].frob

# (16) operator writes inside a rescue modifier join the same way —
# fires `for [5 | 6]`.
o16 = 5
(o16 += 1) rescue nil
[o16].frob

# --- index writes -----------------------------------------------------------

# (17) a plain `recv[…] = v` index argument EVALUATES and binds —
# fires `for [7]` (both index arguments evaluate, the later arg wins).
i17 = 5
h17 = {}
h17[i17 = 6] = h17[i17 = 7]
[i17].frob

# (18) a compound index-write's ARGUMENT is dropped, not rebound —
# `h[w = 6] ||= 1` leaves `w` at `5` (the reference's `IndexWriteWidening`
# quirk). All three fires `for [5]`.
i18a = 5
h18 = {}
h18[i18a = 6] ||= 1
[i18a].frob
i18b = 5
h18[i18b = 6] += 1
[i18b].frob
i18c = 5
h18[i18c = 6] &&= 1
[i18c].frob

# (19) a write in the compound index-write's VALUE position binds —
# fires `for [6]`.
i19 = 5
h19 = {}
h19[:k] ||= (i19 = 6)
[i19].frob
i19b = 5
h19[:k] += (i19b = 6)
[i19b].frob

# --- `begin`/`rescue`/`else`/`ensure` ---------------------------------------

# (20) the primary body joins the entry scope — fires `for [5 | 6]`.
b20 = 5
begin
  b20 = 6
rescue
end
[b20].frob

# (21) a live rescue arm joins too — fires `for [6 | 7]` (the arm's entry
# is the scope BEFORE the body write).
b21 = 5
begin
  b21 = 6
rescue
  b21 = 7
end
[b21].frob

# (22) `ensure` runs after the join — fires `for [8]`.
b22 = 5
begin
  b22 = 6
rescue
  b22 = 7
ensure
  b22 = 8
end
[b22].frob

# (23) an arm that unconditionally EXITS contributes nothing —
# fires `for [6]`.
b23 = 5
begin
  b23 = 6
rescue
  raise "x"
end
[b23].frob

# (24) `else` runs on the primary path — fires `for [7 | 8]`.
b24 = 5
begin
  b24 = 6
rescue
  b24 = 7
else
  b24 = 8
end
[b24].frob

# (25) `begin` as a VALUE unions the primary and live-arm results —
# `6 | 7`, `6` (exiting arm), `7 | 8` (else replaces the primary tail),
# `6` (ensure discarded).
x25a = begin
  6
rescue
  7
end
[x25a].frob
x25b = begin
  6
rescue
  raise "e"
end
[x25b].frob
x25c = begin
  6
rescue
  7
else
  8
end
[x25c].frob
x25d = begin
  6
ensure
  9
end
[x25d].frob

# --- `for` ------------------------------------------------------------------

# (26) a `for` index joins `entry | element` — fires `for [5]`, `for [5 | 6]`.
f26a = 5
for f26a in [5]; end
[f26a].frob
f26b = 5
for f26b in [6]; end
[f26b].frob

# (27) an unbound index takes the element union nil-injected —
# fires `for [1 | 2 | nil]`.
for f27 in [1, 2]; end
[f27].frob

# (28) a literal range's element is the endpoint CLASS nominal —
# fires `for [5 | Integer]`; a non-collection degrades to
# `Dynamic[top]` — `for [5 | Dynamic[top]]`.
f28a = 5
for f28a in (1..3); end
[f28a].frob
f28b = 5
for f28b in 6; end
[f28b].frob

# (29) multi-target decomposes the tuple element — fires `for [1?]`,
# `for [2?]`.
for f29a, f29b in [[1, 2]]; end
[f29a].frob
[f29b].frob

# (30) a `HashShape` eachs `[key, value]` pairs — fires `for [:a?]`,
# `for [1?]`.
for f30a, f30b in { a: 1 }; end
[f30a].frob
[f30b].frob

# (31) a body write joins nil-injected — fires `for [7?]`; a `for *w`
# splat target leaves `w` alone — fires `for [5]`.
for f31 in [1]
  f31 = 7
end
[f31].frob
f31b = 5
for *f31b in [6]; end
[f31b].frob

# --- call arguments and blocks ----------------------------------------------

# (32) a write in a call's receiver or arguments EVALUATES and binds —
# `1.fdiv(c32 = 6)` leaves `c32` at `6` — fires `for [6]`.
c32 = 5
1.fdiv(c32 = 6)
[c32].frob

# (33) a block body write joins the nil-injected way — fires
# `for ["s" | 1]`; a block-PARAMETER name stays block-scoped — fires
# `for ["s"]` (rigor-rs#166 preserved).
c33 = "s"
[1].each { |e| c33 = 1 }
[c33].frob
c34 = "s"
[1].each { |c34| c34 = 1 }
[c34].frob

# --- inert carriers stay inert (#153 controls) --------------------------------

# (35) `defined?` / `BEGIN` / `super` writes neither bind nor widen —
# all three fire `for [5]`.
q35a = 5
defined?(q35a = 6)
[q35a].frob
q35b = 5
BEGIN { q35b = 6 }
[q35b].frob
q35c = 5
super(q35c = 6)
[q35c].frob
