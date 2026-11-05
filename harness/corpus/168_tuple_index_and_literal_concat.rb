# Issue #479, items 1 and 2: two folds where the reference declines.
#
# 1. A constant index outside a Tuple's range. `ShapeDispatch#tuple_single_index`
#    declines it (`normalise_index` answers nil), so the RBS `Array#[] -> Elem`
#    tier types it as the element union, never as nil. The port folded it to
#    `Constant[nil]` and fired `for nil`. The alias rows of the issue
#    (`g = f; f[0] << 2; g[0][1].succ`) are this fold: the reference does not
#    follow the alias either. It is silent because `g[0][1]` types as `1`.
#    The port now declines. The reference's `for 1` rows stay port-silent:
#    the port does not project a declined Tuple read through the RBS tier.
# 2. `<<` / `concat` with one argument on literal-bearing operands. The
#    reference answers `literal-string & non-empty-string`
#    (`LiteralStringFolding.fold_concat`), which witnesses nothing. The port
#    answered the RBS `-> self` String. It now types the call untyped.

f = [[1]]
g = f
f[0] << 2
g[0][1].succ
g[0][0].zork               # control: in range, fires on both
if g[0].size == 2 then puts 1 end   # control: the reference keeps the alias's shape

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
