# Literal `==`/`!=` predicate narrowing — the reference's
# `analyse_equality_predicate` (narrowing.rb) ported into the per-read flow
# env's `flow_narrow_condition`.
#
# The reference trusts a literal NODE operand of class String, Symbol,
# Integer, `true`, `false`, or `nil` — Float is deliberately not trusted —
# and narrows on BOTH edges of `==`/`!=` in either operand order. `nil`,
# `true` and `false` extract from a mixed domain; a String/Symbol/Integer
# literal narrows only an already-FINITE trusted literal domain — a broad
# `String`, `Integer`, `Dynamic` or `Top` carrier is never manufactured into
# a literal by the comparison.
#
# Every firing line and every silent row is oracle-measured at the
# `e59b7b89` pin, one fresh temp cwd per case, `--no-cache` (UPSTREAM.md
# hazard 1).

# --- (1) `==`/`!=` extract a singleton from the pin --------------------------

# `x == nil` folds `true` for a `nil` pin: `undefined-method` on the nil
# receiver AND `flow.always-truthy-condition` on the predicate, on both
# engines.
def a1
  x = nil
  if x == nil
    x.upcase
  end
end

def a2
  x = true
  if x == true
    x.frob
  end
end

# The same narrow through a ternary, an `unless` modifier, and a negation.
def a3
  x = nil
  y = x == nil ? x.upcase : 1
end

def a4
  x = nil
  x.upcase unless x == nil
end

def a5
  x = nil
  if !(x == nil)
    x.upcase
  end
end

# `x == "s"` on a `nil` pin folds `false` — `always-falsey-condition`, both
# engines.
def a6
  x = nil
  if x == "s"
    x.upcase
  end
end

# --- (2) STAYS SILENT: the carriers the reference declines --------------------

# A broad carrier is never manufactured into a literal by the comparison:
# `gets` is `String?`, not a finite literal domain, so `x == 1` narrows
# nothing and `x.frob` witnesses nothing.
def b1
  x = gets
  if x == 1
    x.frob
  end
end

def b2
  x = gets
  if 1 == x
    x.frob
  end
end

# `Float` is not a trusted equality literal — `x == 1.5` narrows nothing.
def b3
  x = gets
  if x == 1.5
    x.frob
  end
end

# A non-literal operand narrows nothing — `x == y` is a local read, not a
# literal node.
def b4
  x = gets
  y = 1
  if x == y
    x.frob
  end
end

def b5
  x = gets
  if x == :sym
    x.frob
  end
end

def b6
  x = gets
  if x != "s"
    x.frob
  end
end

# --- (3) `v&.m` safe-nav predicates --------------------------------------------
#
# A truthy `v&.m` proves the receiver non-nil (`analyse_safe_nav_receiver`):
# the `==`/`!=`/`nil?` method narrowing NEVER applies to a safe-nav call —
# `x&.==(1)` only shows the comparison ran, i.e. `x` was non-nil.

# `x&.==(1)` on a `nil | 1` union narrows `x` to `1` — `frob` names it.
def c1
  x = rand(2) == 0 ? nil : 1
  x.frob if x&.==(1)
end

# `x&.nil?` on `nil | "s"` narrows `x` to `"s"` on the truthy edge — `upcase`
# is defined there, so this stays silent.
def c2
  x = rand(2) == 0 ? nil : "s"
  x.upcase if x&.nil?
end

# …and `frob` on the `"s"` arm fires `undefined-method` on both engines.
def c3
  x = rand(2) == 0 ? nil : "s"
  x.frob if x&.nil?
end
