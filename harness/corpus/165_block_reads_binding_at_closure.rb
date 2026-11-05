# A file-scope block or lambda reads a captured local's binding at the closure,
# never a write later in the file (issue #473).
#
# The port typed a site inside a literal block / lambda at file scope from the
# END-of-file env, so `y = "s"; [1].each { y.upcase }; y = 1` fired
# `undefined method 'upcase' for 1` where the reference is silent, and a later
# `y = [1]` changed the message of a row the reference fires `for 1`.
#
# Must-still-fire controls: a block that fails on the binding it captured
# (`b`, `g`; their message must name that binding, not the later one), a
# nested block (`h`), a lambda called after the write (`i`), and a later
# write inside another block (`j`). A block that rebinds the captured local
# itself (`k`) stays silent on both engines.

a = "s"
[1].each { a.upcase }
a = 1

b = 1
[1].map { b.upcase }
b = [1]

c = "s"
f = -> { c.upcase }
c = 1
f.call

d = "s"
1.tap { d.upcase }
d = nil

e = "s"
[1].each { [2].each { e.upcase } }
e = 1

g = 1
[1].each { g.center }
g = "s"

h = 1
[1].each { [2].each { h.upcase } }
h = "s"

i = 1
l = -> { i.upcase }
i = "s"
l.call

j = 1
[1].each { j.upcase }
[2].each { j = "s" }

k = 1
[1].each { k.upcase; k = "s" }
