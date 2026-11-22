# rigor-rs#775 — a method whose overloads disagree on the return class holds
# nothing in the flat `method_return` slot (`Integer#/` is `(Integer) ->
# Integer | (Float) -> Float | (Rational) -> Rational | (Complex) -> Complex`,
# the bigdecimal reopen prepending a `(BigDecimal)` arm), so `x / 3` on a
# NON-constant receiver used to type `Dynamic[top]`. The reference's
# `OverloadSelector` still answers from the argument types: pass 0
# (`ProvenOverload`) takes the first overload in declared order whose every
# engaged parameter is a lone class naming its argument's own class, and the
# no-match fallback takes the first block-free overload in receiver-affinity
# order.
#
# The `2 ** 20000` receiver is where it surfaced: `**` widened the fold's
# answer to `Nominal[Integer]` (the reference's `WidenedResult`), so the next
# `/` had no value to fold and the slot had no return to give.
#
# Rows this slice answers, matching the reference:
x = 2 ** 20000 / 3; x.zz
x = 2 ** 20000 * 3; x.zz
x = 2 ** 20000 / 10 ** 4096 / 2 ** 4294967296; x.zz
x = 2 ** 20000 - 5; x.zz
x = 2 ** 20000 + 5; x.zz
x = 2 ** 20000 % 3; x.zz
x = 2 ** 20000 / 3.0; x.zz
x = 3 / (2 ** 20000); x.zz
x = 2 ** 20000; y = x / "a"; y.zz
x = 2 ** 20000; y = x + "a"; y.zz
x = 2 ** 20000; y = x / x; y.zz
x = 2 ** 20000; y = x ** 2; y.zz
x = 2 ** 20000; y = x / 3; z = y / 2; z.zz
y = rand(3); z = y / 3; z.zz
y = rand(3); z = y / "a"; z.zz

# Declined residual rows (the reference fires, the port stays silent — each a
# named gap this slice deliberately leaves):
# - `(±1) ** negative`, the issue's noted op-write fold residual:
x = 1; x **= -3; x.zz
x = -1; x **= -3; x.zz
# - an all-`Constant` call the reference folds to a literal — the port's fold
#   is the narrower one, so it never mints the nominal the row would print:
x = 1; y = x / 3.0; y.zz
x = 2.5 ** 2; y = x / 3; y.zz
# - a `BigInt` fold gap (rigor-rs#695):
x = 10 ** 400; y = x / 3; y.zz
# - argument shapes outside `Constant` / bare `Nominal`:
x = 2 ** 20000; y = x / (1..3); y.zz
x = 2 ** 20000; y = x / Rational(3,1); y.zz
# - the unported strict / affinity passes (`Float#/`'s `(Numeric)` arm wins on
#   the reference where pass 0 declines):
y = rand; z = y / 3; z.zz
# - returns this slice does not translate — a union, a tuple, `bool`:
x = 2 ** 20000; y = x.pow(3); y.zz
x = 2 ** 20000; y = x.divmod(3); y.zz
x = 2 ** 20000; y = x < 3; y.zz
# - a `Constant` receiver whose every argument is also pinned, where the
#   reference's selector still answers (`x / "a"` -> Integer by fallback):
x = 1; y = x / "a"; y.zz

# Must-still-fire controls: the widened answer itself, the op-write and
# dot-call forms it already propagated through, and a plain fold.
x = 2 ** 20000; x.zz
x = 2; x **= 20000; x.zz
x = 2 ** 20000; y = x.div(3); y.zz
x = 1; y = x / 3; y.zz
