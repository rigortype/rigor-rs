# A loop or block body that mutates an element in place drops the literal
# shape of the collection it iterates (upstream #2285 / issue #2262, v0.4.6).
#
# A block param or `for` index (or a nested block's param, `_1`, `it`) that a
# shape mutator is called on — directly, through an index chain, or by an
# index store — and a `&:mutator` block-pass all widen the receiver's locals.
#
# Must-still-fire controls: a read-only body, a mutation of an unrelated local,
# an alias (`y = x; y << 2` is not followed), and an element passed to a call.
a = [[1]]
for x in a; x << 2; end
if a[0].size == 2 then 1 end
b = [[[1]]]
for x in b; x[0] << 2; end
if b[0][0].size == 2 then 1 end
n = [[[1]]]
n.each { |r| r.each { |c| c << 2 } }
if n[0][0].size == 1 then 1 end
s = [" s "]
s.each(&:strip!)
if s[0].size == 3 then 1 end
t = [[1]]
t.each(&:clear)
if t[0].size == 1 then 1 end
f = [[1]]
f.each { |x| x.push(2) }
if f[0].size == 1 then 1 end
g = [{ a: 1 }]
g.each { |x| x[:b] = 1 }
g[0][:b].succ
def m
  a = [[1]]
  a.each { |x| x << 2 }
  if a[0].size == 1 then 1 end
end
ctl = [[1]]
ctl.each { |x| x.size }
if ctl[0].size == 1 then 1 end
ctl2 = [[1]]
out = []
ctl2.each { |x| out << x }
if ctl2[0].size == 1 then 1 end
al = [[1]]
al.each { |x| y = x; y << 2 }
if al[0].size == 1 then 1 end
pc = [[1]]
pc.each { |x| out.push(x) }
if pc[0].size == 1 then 1 end
