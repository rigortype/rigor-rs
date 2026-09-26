# `HashLookupMutation` (`hash_lookup_mutation.rb`, pin `e59b7b89`): the three
# lookup mutators `Hash#default=` / `Hash#default_proc=` / `Hash#compare_by_identity`
# rewrite a literal-seeded HashShape binding. `default=` / `default_proc=` OPEN
# the shape — declared keys keep their value pins, but a key outside `pairs`
# may hit the configured default, so it reads `untyped` (the motivating row:
# `counts = { a: 1 }; counts.default = 0; counts[:b] + 1` is SILENT, not
# `for nil`). `compare_by_identity` leaves an identity-stable shape alone
# (Symbol / true / false / nil / fixnum keys read exactly as before) and only
# widens one holding identity-sensitive keys (String / heap Float / bignum)
# to `Hash[K, V | untyped]` — the `untyped` arm keeps a literal String-key
# read from folding, since a fresh `"...".` literal may miss by object id.
#
# Every line below is oracle-measured at the pin, one fresh temp cwd,
# `--no-cache`.

# --- STAYS SILENT: opened / identity-widened reads can't fold ----------------

# (1) the motivating row — an opened shape's missing key reads `untyped`.
counts = { a: 1 }
counts.default = 0
counts[:b] + 1

# (2) `default_proc =` opens too — even when assigned `nil`.
dproc = { a: 1 }
dproc.default_proc = nil
dproc[:b] + 1

# (3) every lookup surface on the open shape defers: `dig`, `values_at`,
# `fetch` (a miss declines — Ruby raises KeyError), `key?` (an open shape's
# extras could hold the key), and the `default` reader itself.
opened = { a: 1 }
opened.default = 0
opened.dig(:b).upcase
opened.values_at(:b).first.upcase
opened.fetch(:b).upcase
opened.key?(:b).upcase
opened.default.upcase
opened.default_proc.upcase

# (4) a String key is a fresh object per literal: after `compare_by_identity`
# the read may miss by identity, so the shape degrades to
# `Hash[String, Dynamic[top] | Integer]` and both reads stay silent.
sensitive = { "k" => 1 }
sensitive.compare_by_identity
sensitive["k"].upcase
sensitive["z"].upcase

# (5) identity-sensitive non-String keys (a heap Float) widen the same way.
floated = { 1.5 => "x" }
floated.compare_by_identity
floated[1.5].upcase

# (6) `compare_by_identity` on an OPEN shape holding a sensitive key degrades
# fully to `Hash[untyped, untyped]` — `widen_hash_shape` on an open shape loses
# every bound, so even the declared key's read stops folding.
reopened = { "k" => 1 }
reopened.default = 0
reopened.compare_by_identity
reopened["k"].upcase
reopened[:a].upcase

# (7) a lookup mutation inside a block body applies to the OUTER binding
# (`widen_after_block`), so the missing-key read is untyped.
blocked = { a: 1 }
[1].each { blocked.default = 0 }
blocked[:b] + 1

# (8) a BRANCH-contained mutation may not run: the join of the open and closed
# edges declines, and reads past it stay silent on every key.
branched = { a: 1 }
branched.default = 0 if rand > 0
branched[:a].upcase
branched[:b] + 1

# (9) non-shape receivers are untouched by the mutation model — `default=` on
# a String binding has no HashShape to open, so the binding stays pinned (and
# the call itself witnesses `default='` absent on String, on both engines).
nstr = "s"
nstr.default = 0
nstr.upcase

# --- FIRES on both sides (must-still-fire controls) --------------------------

# (10) declared keys keep their value pins on an open shape.
kept = { a: 1 }
kept.default = 0
kept[:a].upcase
kept.values_at(:a).first.upcase
kept.foo

# (11) `compare_by_identity` on an identity-STABLE shape (Symbol / nil /
# fixnum keys) leaves the closed shape untouched: reads fold as before.
stable = { k: 1 }
stable.compare_by_identity
stable[:k].upcase
stable["k"].upcase

stable2 = { nil => 1, 5 => "x" }
stable2.compare_by_identity
stable2[nil].upcase
stable2[5].frobnicate_zzz

# (12) the String-keyed widening still projects to Hash for dispatch —
# `foo` witnesses on the widened nominal.
swide = { "k" => 1 }
swide.compare_by_identity
swide.foo

# (13) the untouched literal behaves exactly as before: a missing key on a
# CLOSED shape folds `nil`.
plain = { a: 1 }
plain[:b] + 1

# (14) a safe-nav mutator call still opens the binding (`counts&.default = 0`
# runs `open_shape` whenever the receiver is non-nil).
navigated = { a: 1 }
navigated&.default = 0
navigated.foo
