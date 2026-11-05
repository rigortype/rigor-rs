# A project sig/ overloading reopen (`def m: ... | ...`) of a plugin member
# (rigor-rs#482). RBS prepends the reopen's overloads to the plugin's, so the
# reference types `"abc".camelize` as the project's `Integer` and accepts
# `underscore(1, 2)` (envelope 0..2). The port combines the two flat slots:
# the arity envelopes union, and a return the two sides disagree on declines.
#
# Must-still-fire controls: a plugin-only member (`dasherize`), the merged
# envelope's own arity row, and core rows.
"abc".camelize.even?
"abc".underscore(1, 2)
"abc".underscore(1, 2, 3)
"abc".dasherize.zork
"q".zork
1.zork
