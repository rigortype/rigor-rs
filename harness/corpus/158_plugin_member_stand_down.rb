# A bundled plugin's member the project's own sig/ also declares (upstream
# v0.4.7, #2424): the project declaration binds and the plugin's stands down,
# where v0.4.6 failed both classes' builds. `String#titlecase` and
# `Hash#deep_merge` answer the project's `Integer`.
#
# Must-still-fire controls: a plugin-only member (`dasherize`) and core rows.
"abc".titlecase.even?
"abc".titlecase.zork
{ a: 1 }.deep_merge({}).even?
"abc".dasherize.zork
1.zork
[1].zork
"x".upcase.zork
