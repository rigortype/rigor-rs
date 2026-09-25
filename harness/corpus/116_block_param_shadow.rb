# A write to a name an enclosing block or lambda BINDS is not a rebind of the
# top-level local it shadows (rigor-rs#166, a #148 coverage regression).
#
# `Typer::build_toplevel_check_env` widens every top-level local that a nested
# construct rebinds, because a block may run and rebind a CAPTURED local. Its
# collector used to exclude only `def` / `class` / `module` scopes, so a write
# to a block parameter or `;`-declared block-local wrongly widened the outer
# local. The shadow set is Prism's `locals` for the block/lambda node — every
# parameter form, block-locals, and names first assigned in the body, but NOT
# a captured outer local — so a write to a bound name stays a block-scoped
# write (the FIRE rows keep the reference's `for "s"`), while a write to a
# captured name still counts as a rebind (the DECLINED rows: the reference
# joins `for "s" | 2`, the port declines — that join is #152's deep work).
#
# Every row sits at TOP LEVEL with its own locals (the file is one scope).
#
# Measured against the pinned reference (e59b7b89, Ruby 4.0, fresh cwd,
# `--no-cache`).

# --- FIRE: a write to a block-bound name does not widen the outer local -----

# (1) required param. Reference and port: `for "s"`.
w1 = "s"
[1].each { |w1| w1 = 2 }
w1.lenght

# (2) `;`-declared block-local.
w2 = "s"
[1].each { |x2; w2| w2 = 2 }
w2.lenght

# (3) destructured param `(a, w)`.
w3 = "s"
[1].each { |(a3, w3)| w3 = 2 }
w3.lenght

# (4) splat param `*w`.
w4 = "s"
[1].each { |*w4| w4 = 2 }
w4.lenght

# (5) keyword + `&` block param.
w5 = "s"
[1].each { |k5: 1, &w5| w5 = 2 }
w5.lenght

# (6) the write sits in an INNER block; the name the inner block binds
# shadows there.
w6 = "s"
[1].each { |x6| [2].each { |w6| w6 = 2 } }
w6.lenght

# (7) the write sits in an inner block to a name the OUTER block binds —
# shadowed by the enclosing `locals`.
w7 = "s"
[1].each { |w7| [2].each { w7 = 2 } }
w7.lenght

# (8) lambda param `->(w)`.
w8 = "s"
-> (w8) { w8 = 2 }
w8.lenght

# (9) optional lambda param `->(w = 1)`.
w9 = "s"
->(w9 = 1) { w9 = 2 }
w9.lenght

# (10) `proc` / `lambda` / `do…end` blocks bind the same way.
w10 = "s"
proc { |w10| w10 = 2 }
w10.lenght

w11 = "s"
lambda { |w11| w11 = 2 }
w11.lenght

w12 = "s"
[1].each do |w12|
  w12 = 2
end
w12.lenght

# (13) every parameter form at once.
w13 = "s"
[1].each { |w13, o13 = 1, *r13, p13, kw13:, okw13: 2, **o13b, &b13| w13 = 2 }
w13.lenght

# (14) a write inside a lambda nested in a block, to the block's param.
w14 = "s"
[1].each { |w14| -> { w14 = 2 } }
w14.lenght

w15 = "s"
[1].each { |x15| ->(w15) { w15 = 2 } }
w15.lenght

# (16) shadowed-name writes of every collected shape: multi-write, op-write,
# a `rescue => w` binding, and a `for w` index (a `for` binds in its
# enclosing scope — here the block's).
w16 = "s"
[1].each { |w16, y16| w16, y16 = 1, 2 }
w16.lenght

w17 = "s"
[1].each { |w17| w17 += 1 }
w17.lenght

w18 = "s"
[1].each { |w18| begin; raise; rescue => w18; end }
w18.lenght

w19 = "s"
[1].each { |w19| for w19 in [2]; end }
w19.lenght

# --- DECLINED: a write to a CAPTURED name is still a rebind -----------------
# The reference joins `"s" | 2`; the port widens instead (#152's deferred
# deep work). These rows are the controls that prove the shadow set is not
# over-suppressing.

# (20) plain captured write.
w20 = "s"
[1].each { |x20| w20 = 2 }
w20.lenght

# (21) a numbered-param block does not bind `w`; the write is captured.
w21 = "s"
[1].each { _1; w21 = 2 }
w21.lenght

# (22) no parameter list at all — `w` is captured.
w22 = "s"
[1].each { w22 = 2 }
w22.lenght

# (23) `it` is a captured local when it already exists outside — the implicit
# `it` param binds nothing a write can target, so `it = 2` inside the block
# still rebinds the outer `it` (Prism leaves it out of the block's `locals`).
it = "s"
[1].each { it = 2 }
it.lenght

# (24) a `for` index binds in the enclosing scope — at top level that IS the
# outer local.
w24 = "s"
for w24 in [1]
  w24 = 2
end
w24.lenght

# (25) a write inside a lambda nested in a block, to a name NEITHER binds —
# captured all the way through. The reference is silent here too (a lambda's
# body is not invoked in place), so both engines decline.
w25 = "s"
[1].each { |x25| -> { w25 = 2 } }
w25.lenght

# --- heredoc bodies run in their opener's scope, not the block's span -------
# A `#{…}` interpolation write evaluates where the heredoc OPENER sits, but
# its body lines follow after — so it can lie inside an enclosing block's
# SPAN while belonging to the outer scope. Shadowing must therefore be
# decided by structure (reachability from the block's own body), not by span
# containment (the #166 review's h1–h9 false positives). The reference joins
# these writes (`for 2`); the port widens instead — declined, like the other
# captured-rebind rows.

# (26) the heredoc opener is an ARGUMENT of the block's own call.
w26 = "s"
[1].each_slice(<<~H26.size) do |w26|
#{w26 = 2}
H26
end
w26.lenght

# (27) the opener is a sibling statement; the lambda's braces wrap the body.
w27 = "s"
puts(<<~H27); ->(w27) {
#{w27 = 2}
H27
}
w27.lenght

# (28) an xstring heredoc the same way.
w28 = "s"
puts(<<~`H28`); [1].each { |w28|
#{w28 = 2}
H28
}
w28.lenght

# (29) FIRE control: the opener IS inside the block body, so `#{w29 = 2}`
# evaluates inside the block — a block-scoped write, not a rebind. Both
# engines fire `for "s"`.
w29 = "s"
[1].each { |w29| puts <<~H29
#{w29 = 2}
H29
}
w29.lenght

# (30) twin FIRE control through an ordinary interpolated string inside the
# body: `#{w30 = 2}` is evaluated in the block's scope, so the write is
# block-local and both engines fire `for "s"`.
w30 = "s"
[1].each { |w30| puts "#{w30 = 2}" }
w30.lenght
