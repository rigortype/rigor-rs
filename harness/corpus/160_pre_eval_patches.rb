# `.rigor.yml` `pre_eval:` monkey-patches (ADR-17; upstream #1708/#2098/#2107,
# rigor-rs#472 item 1, #479 item 4). A `def` the patch file adds to a core
# class answers the class, its descendants (a patch on `Object`, `Kernel`,
# `Comparable`), a module the patch file includes or prepends (behind an
# idempotence guard too), an alias and a block-taking patch, so none of
# these calls is an undefined method.
1.zz
"x".bar { 1 }
rand(3).oz
Integer.oz
Integer.mk
kk
1.cc
"a".cc
1.inc_m
1.5.gm
"x".ali
"x".ali2
x = rand(2).zero? ? 1 : "a"
x.oz
# Must-still-fire controls: a private patch and a top-level call of a
# protected one refuse an explicit receiver; a patch on `Hash` does not
# reach `Array`; an unpatched name, and an unpatched top-level call.
1.kp
1.np
[1].only_hash
1.nope
"x".nope
Integer.nope
x.nope
unknown_top_level_call
