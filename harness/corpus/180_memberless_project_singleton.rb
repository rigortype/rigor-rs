# upstream #2438 / rigor-rs#908 — `open_project_singleton?`: a MEMBER-LESS
# project `sig/` declaration leaves its class's singleton surface to the
# source, and with no `class`/`module` body inside `paths:` the surface is
# open — `C.typo` on it must NOT be witnessed (the reference's
# `memberless_project_class? && open_source_class?` decline). A sig class
# with at least one member (instance method, `def self.x`, mixin) is
# witnessable again, and the typo fires.
#
# NOT covered here (a separate mint-level coverage gap): a member-less sig
# class the source ALSO declares (`class Foo::Bar; end` + `sig class
# Foo::Bar`) — the reference still fires `singleton(Foo::Bar)` there
# because the source chain is closed, while rigor-rs's
# `constant_shadowed` mint reads the receiver as an instance and the call
# never reaches the singleton surface.

# Member-less sig decls with no project class body ⇒ open ⇒ silent.
Empty.bogus
Empty::Deep.bogus

# The member-less sig's OWN written superclass names a non-project class
# ⇒ open ⇒ silent (upstream `open_tail?`).
My::Sub.bogus

# Member-less MODULE decls read open the same way ⇒ silent.
Mixin::Thing.bogus

# Members close the member-less decline ⇒ witness ⇒ fires.
Full::Klass.bogus
SelfMeth::Klass.bogus
