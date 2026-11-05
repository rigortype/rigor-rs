# How the `pre_eval:` declarations resolve across files (ADR-17, rigor-rs#472):
# a later file's `private :dd` and `private def ee` win over the earlier
# public `def`s, a later `public :ff` opens an earlier `private def ff`,
# `module_function` leaves a private instance method and a public singleton
# copy, a mixin name resolves in the lexical scope it is written in, and a
# patched `raise` / `#exception` answer `call.raise-non-exception`.
"x".ff
"x".gg
Kernel.mf
mf
mpriv
1.mpub
1.ne
raise 1
raise :sym
# Must-still-fire controls.
"x".dd
"x".ee
1.mf
1.mpriv
1.5.ne
"x".nope
