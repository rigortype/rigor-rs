# Singleton methods the program gives a receiver local (upstream #2086 /
# issue #1722, v0.4.5; `def o.m` is issue #1120).
#
# `def o.m` (that name), `o.extend(M)`, a call on `o.singleton_class`,
# `class << o`, `o.define_singleton_method(:m)` (that literal name) and an
# `o.instance_eval` that writes a `def` give the one object `o` holds methods,
# so `call.undefined-method` declines on `o` in the same local scope, wherever
# the opening sits; `t = o.clone` shares them (`dup` does not).
#
# Must-still-fire controls: another local, another literal name, a `dup`, an
# `instance_eval` that defines nothing, and the same local name in a nested
# `def` scope.

module M
  def m = 1
end

a = "a"
def a.da = 1
a.da

s = "s"
s.extend(M)
s.m
[1].each { s.anything }

v = "v"
v.define_singleton_method(:vv) { 1 }
v.vv

w = "w"
w.singleton_class.class_eval { def ww = 1 }
w.ww

c = "c"
class << c
  def cc = 1
end
c.cc

z = "z"
z.instance_eval { def zz = 1 }
z.zz

k = s.clone
k.m

o = "o"
o.m
v.vx
d = s.dup
d.m
f = "f"
f.instance_eval { @x = 1 }
f.zz
a.db

def nested
  s = "n"
  s.m
end
