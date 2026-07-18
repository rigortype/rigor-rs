# Kernel constant-folding (v0.3.0, ADR-0038 spec §3). The receiverless Kernel
# conversion functions fold to a value-pinned Constant when the template and
# every argument are constants: `format`/`sprintf` run a Ruby-sprintf interpreter,
# `String()` runs `to_s`, `Hash()` folds a HashShape identity / empty hash,
# `Integer()`/`Float()` parse per Ruby's grammar. The folded value flows into a
# chained call so an undefined method on it is witnessed byte-for-byte on
# (rule, line, column, message) exactly where the oracle witnesses it. Every fold
# here is oracle-verified; anything uncertain declines to a silent gap (never a
# false positive).

# --- format / sprintf ---

# `%d` integer: the fold renders `"1"`, so the chained call flags `for "1"`.
a = format("%d", 1)
a.frobnicate

# `%s` calls to_s on each arg; `%d` on a float truncates.
b = format("%s-%d", "x", 42)
b.frobnicate

# Flags / width / precision: zero-pad, left-justify, sign, hex, precision.
c = format("%05d|%-5d|%+d|%#x|%.3s", 42, 7, 3, 255, "hello")
c.frobnicate

# A no-argument template folds to itself; `%%` is a literal percent.
d = format("100%%")
d.frobnicate

# --- String() ---

# Every scalar kind folds via `to_s`: nil → "", a float keeps its `3.0` spelling.
e = String(nil)
e.frobnicate
f = String(3.0)
f.frobnicate

# --- Hash() ---

# A HashShape argument passes through unchanged (`for { a: 1 }`).
g = Hash({ a: 1 })
g.frobnicate
# nil and an empty array collapse to the empty HashShape (`for {}`).
h = Hash(nil)
h.frobnicate

# --- Integer() / Float() ---

# Ruby's Integer() grammar: radix prefixes, underscores, whitespace, a base arg.
i = Integer("0x1A")
i.frobnicate
j = Integer("1_000")
j.frobnicate
k = Integer("42", 16)
k.frobnicate

# Float() parses decimal / exponent forms.
l = Float("1e3")
l.frobnicate

# --- nominal fallback (fold declines → conversion-class witness, S1) ---

# A fold-time error (arg-type mismatch) declines the VALUE fold, but not the
# class: the oracle's literal-string lift and rigor-rs's nominal String agree.
m = format("%d", "not a number")
m.frobnicate

# An unparseable Integer() raises in Ruby; the value fold declines but the RBS
# envelope still pins the conversion class — witnesses on Integer.
n = Integer("abc")
n.frobnicate

# A splat arity is statically unknown, but format returns String REGARDLESS of
# arity, so the nominal String fallback still witnesses.
args = [1]
o = format("%d", *args)
o.frobnicate

# A file-wide `def sprintf` shadows the Kernel function, disabling the fold
# for that name across the whole file (conservative, FP-safe): the call now
# resolves to the user method, so no constant is witnessed.
q = sprintf("%d", 1)
q.frobnicate

def sprintf(fmt, *rest)
  fmt
end
