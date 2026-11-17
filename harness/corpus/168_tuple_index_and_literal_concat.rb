# Issue #479, items 1 and 2: two folds where the reference declines.
#
# 1. A constant index outside a Tuple's range. `ShapeDispatch#tuple_single_index`
#    declines it (`normalise_index` answers nil), so the RBS `Array#[] -> Elem`
#    tier types it as the element union, never as nil. The port folded it to
#    `Constant[nil]` and fired `for nil`. The alias rows of the issue
#    (`g = f; f[0] << 2; g[0][1].succ`) are this fold. As of v0.4.8 the
#    reference DOES follow the alias — #2677's alias-mutation floor stales a
#    copied literal when the object mutates through another name — so `g[0]`
#    reads the floored element union and `g[0][1].succ`, `g[0][0].zork` and
#    `g[0].size == 2` are all silent there. The port lacks the floor: rows 20
#    and 21 are the pinned #2677 repro (2 unregistered FPs until it lands;
#    see docs/notes/20261115-repin-v048.md). `g[0][1]` still types as `1` on
#    the port because it does not project a declined Tuple read through the
#    RBS tier.
# 2. `<<` / `concat` with one argument on literal-bearing operands. The
#    reference answers `literal-string & non-empty-string`
#    (`LiteralStringFolding.fold_concat`), which witnesses nothing. The port
#    answered the RBS `-> self` String. It now types the call untyped.

f = [[1]]
g = f
f[0] << 2
g[0][1].succ
g[0][0].zork               # v0.4.8: silent on the reference (#2677 floor); port FP
if g[0].size == 2 then puts 1 end   # v0.4.8: silent on the reference; port FP

h = [1]
h[1].succ
h[-2].succ
h[0].zork                  # control

a = "a"
a.concat("b").zzz
b = "a"
(b << "b").zzz
c = "a" * 3000
c.concat("b").zzz
"a".dup.concat("b").zzz
d = "a"
d.concat(1).zzz            # control: an Integer argument is not literal-bearing
e = "a"
e.concat("b", "c").zzz     # control: two arguments
s = "a"
s.replace("b").zzz         # control: `replace` is not in the literal tier
t = [1]
t.concat([2]).zzz          # control: Array#concat
