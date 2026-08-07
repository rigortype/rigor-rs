# Partially-dynamic constant-value harvesting — probe matrix + inventory (2026-08-08)

Evidence for a future mini-spec. NO product code changed. Blocked rows: the two
`§7c` drops of the [collection-shape slice spec](20260807-collection-shape-slice-spec.md)
(2d `DEFAULT_ROUTING_PAYLOAD_HASH`, 2e `SEVERITY_PRIORITIES`) plus bucket-D
synergy. Candidate mechanism under test: mint a bare `Nominal[Hash]`/`Nominal[Array]`
(no element tracking) from a constant whose value is a literal CONTAINER with
non-literal elements.

Oracle recipe: pin `v0.3.1` (`c39e6675`), a fresh `mktemp -d` cwd per probe,
`--no-cache`, plugin path pinned — `ruby -I reference/rigor/lib -I
reference/rigor/plugins/rigor-rbs-inline/lib reference/rigor/exe/rigor check
--no-cache p.rb`. rigor-rs side: a freshly built `target/release/rigor check p.rb`.

## 1. Probe matrix

Witness method is `frobnicate_zzz` (or the real AS method where the row names one).
"silent" = no diagnostic at that site.

### 1a — hash literal with a lambda value (the routable_token shape)

| # | source | reference | rigor-rs |
|---|---|---|---|
| p1 | `H = { c: ->(_){1} }.freeze` ; `H.zzz` | fires, `for { c: Proc }` | silent |
| p1b | same, **no** `.freeze` | fires, `for { c: Proc }` | silent |
| p1c | `H.merge(x).zzz` in a def | fires, `for Hash[:c \| Dynamic[top], Dynamic[top] \| Proc]` | silent |
| p1d | `H.transform_values { \|v\| v }.zzz` | fires, `for { c: Proc }` | silent |
| p1e | `H.keys.zzz` | fires, `for [:c]` | silent |
| p1i | `H.values.zzz` | fires, `for [Proc]` | silent |
| p1f | `H[:c].call(1)` / `H[:c].zzz` | `call` OK; `zzz` fires `for Proc` | silent |
| p1g | `H.fetch(:c).arity` / `.zzz` | `arity` OK; `zzz` fires `for Proc` | silent |
| p1h | `H.each { \|k,v\| v.zzz }` | fires, `for Proc` | silent |
| y5 | `H.compact_blank` / `H.dig(:c)` / `H.zzz_absent` | fires on `compact_blank` + `zzz_absent`, `for { c: Proc }` | silent |
| s1 | `H.keys(1, 2)` | fires `wrong number of arguments to 'keys' on Hash` | silent |
| r2 | `raise H` | fires `raise-non-exception`, operand `{ c: Proc }` | silent |

**The framing is wrong for this shape.** The reference is not "declining a
partially-literal container" — it types `->(_){…}` as `Proc` and builds a FULL
`HashShape` with a `Proc` member. Its receiver is value-pinned with a typed hole,
not a bare nominal, and every projection (`[]`, `fetch`, `keys`, `values`, block
params) resolves through that member type.

### 1b — the chain shape (`%w[…].map.with_index.to_h.freeze`, codequality)

| # | source | reference | rigor-rs |
|---|---|---|---|
| p2a | `SEV = %w[a b].map.with_index.to_h.freeze` ; `SEV.zzz` | fires, `for Hash` (bare) | silent |
| p2c | same **without** `.freeze` | fires, `for Hash[Dynamic[top], Dynamic[top]]` | silent |
| p2b | `SEV.keys.index_with(0)` ; `SEV.keys.zzz` | both fire, `for Array[Dynamic[top]]` | silent |

`.freeze` on a NOMINAL erases its type args (`Hash[Dynamic,Dynamic]` → `Hash`);
on a shape it is pure identity (p1/p1b, p3a/p3f, z4). Both engines already treat
`.freeze` as identity in the harvest path.

### 1c — other partially-literal container shapes

| # | source | reference receiver rendering | rigor-rs |
|---|---|---|---|
| p3a | `A = [1, unknown_zzz, 2]` | `[1, Dynamic[top], 2]` | silent |
| p3f | same `+ .freeze`; also `A[1].zzz` | `[1, Dynamic[top], 2]`; `A[1]` **silent** | silent |
| p3b | `H = { a: 1, **OTHER }` (OTHER a literal const) | `Hash[:a, 1]` | silent |
| p3b2 | `H = { a: 1, **unknown_zzz }` | `Hash[:a, 1]` | silent |
| p3c | `H = { a: 1, unknown_zzz => 2 }` | `Hash[:a \| Dynamic[top], 1 \| 2]` | silent |
| p3d | `A = [*unknown_zzz]` | `Array[Dynamic[top]]` | silent |
| p3e | `H = { a: [->(){1}] }` ; `H[:a].zzz` | `{ a: [Proc] }`; `H[:a]` → `[Proc]` | silent |
| z1 | `A = ["a", "b#{1}"]` | `["a", String]` | silent |
| z2 | `OTHER = 5` ; `A = [1, OTHER]` | `[1, Dynamic[top]]` — a constant-read element is **Dynamic**, NOT folded | silent |
| z3 | `A = [1, "x".upcase]` | `[1, "X"]` — the reference constant-FOLDS the call | silent |
| z4 | `A = [].freeze` / `H = {}.freeze` | `[]` / `{}` | **parity** (both fire) |

A splat (`**`/`*`) degrades the container the way the reference's own union
machinery does; it never declines the constant.

### 1d — the two real census rows, verbatim files

| row | reference | rigor-rs |
|---|---|---|
| gitlab `lib/authn/token_field/generator/routable_token.rb` | `:61:14 compact_blank for Hash[:c \| Dynamic[top], Dynamic[top] \| String]` | silent |
| gitlab `lib/gitlab/ci/reports/codequality_reports.rb` | `:13:30 with_indifferent_access` + `:50:89 index_with for Array[Dynamic[top]]` | only `:13:30` |

**Harvest simulation** (replace the non-literal element/RHS with a literal, so C5
harvests):

| row | reference | rigor-rs |
|---|---|---|
| routable_token, `c: 1` instead of the lambda | `:61:14` fires | `:61:14` fires, `for Hash` |
| codequality, `SEVERITY_PRIORITIES = { info: 0, minor: 1 }.freeze` | `:50:89` fires `for [:info, :minor]` | `:50:89` fires `for Array` |

So both rows close **iff the constant harvests** — but see §3: only routable_token
is reachable by a bare nominal.

### 1e — FP-hazard series: what a bare nominal projects to, in each engine

Carriers used: rigor-rs mints a bare `Nominal[Hash]` from `{a: 1}.merge(x)` and a
bare `Nominal[Array]` from `[1].concat(x)` (the reference keeps element types on
the first and also reaches bare `Array` on the second).

| # | site | reference | rigor-rs |
|---|---|---|---|
| n1 | `h.zzz` (direct) | fires `for Hash[:a \| Dynamic[top], 1 \| Dynamic[top]]` | **fires** `for Hash` |
| n6 | `a.zzz` (direct) | fires `for Array` | **fires** `for Array` |
| n2 | `h[:c].call(1)` / `h[:c].zzz` | silent (`Dynamic` value) | silent |
| n3 | `h.keys.index_with(0)`, `h.keys.zzz`, `h.values.zzz` | all 3 fire (`Array[…]`) | **silent** |
| n4 | `h.each { \|k,v\| v.zzz }`, `h.fetch(:c).arity`, `h.fetch(:c).zzz` | silent | silent |
| n5 | `a = [1,2].sort` ; `a.zzz`, `a[0].zzz`, `a.first.zzz` | 3 fire (Tuple survives `sort`) | 1 fires (`for Array`) |
| k1 | `a.first.zzz`, `a.map{\|e\| e.zzz}`, `a.size.zzz` | only `a.size.zzz` fires | silent |
| y1 | `h.keys(1,2)` / `a.first(1,2)` (arity) | both fire | **silent** |
| y3 | `h[:zzz].upcase` / `a[0].upcase` (possible-nil) | silent | silent |
| y2 | `H = {a: 1}.freeze` ; `H[:zzz].upcase` | fires `for nil` | **parity** |
| t1/t2 | `if H` / `if h` (always-truthy) | silent on every collection carrier | silent |
| a1/a2 | `"abc".start_with?(H)` / `(h)` (ATM) | silent | silent |
| r1 | `raise H` on `{a: 1}` | fires | **parity** |
| r3 | `raise h` on a bare nominal | fires | silent |

**The headline safety result**: a bare `Nominal[Hash]`/`Nominal[Array]` with
`args: []` is **projection-inert in rigor-rs**. `fold_tuple_projection` /
`fold_hash_shape_projection` both match on `Type::Tuple` / `Type::HashShape` and
return `None` for a `Nominal` (`crates/rigor-infer/src/lib.rs:947-1200`), and the
RBS tier resolves nothing from an argument-less generic. The specific worry in the
brief — "would OUR `[]` projection on a bare Hash nominal witness `.call`?" —
is answered **no** (n2, n4, y1, y3, k1): every projection off the minted nominal
goes silent while the reference fires. Divergence is strictly UNDER-emission.

### 1f — reassignment and cross-file

| # | source | reference | rigor-rs |
|---|---|---|---|
| p4b | `H = {c: ->(){}}` ; `H = [1,2]` ; `H.zzz` | fires, `for [1, 2] \| { c: Proc }` — a UNION receiver dispatches | silent |
| p4c | use BETWEEN the two writes | fires at the use with the SAME union — constant typing is not flow-ordered | silent |
| p4a | constant written in `class K`, used in a `def` in the same file | fires | silent |
| x1 | `M::C::{H,A,L}` written in `a.rb`, used in `b.rb` (`check a.rb b.rb`) | **No diagnostics** — even for the fully-literal `L = [1,2].freeze` | **fires** on `L` (`b.rb:6:9 for [1, 2]`) |
| x2 | toplevel `TOPL = [1,2].freeze` in `a.rb`, `TOPL.zzz` in `b.rb` | **No diagnostics** | **fires** (`b.rb:1:6`) |
| x2′ | same two lines in ONE file (control) | fires | fires — parity |

## 2. rigor-rs inventory (read-only)

### 2a — where `const_lit_of` declines today

`crates/rigor-infer/src/source_index.rs:1859` — `const_lit_of(ast, node) -> Option<ConstLit>`.
Accepting arms: 7 scalar literals (`IntegerLit`/`FloatLit`/`StringLit`/`SymbolLit`/
`TrueLit`/`FalseLit`/`NilLit`) → `ConstLit::Scalar`; `ArrayLit` → `ConstLit::Tuple`;
`HashLit` → `ConstLit::Hash`; `Range` → `ConstLit::Range`; a zero-arg block-free
`.freeze` call → recurse on the receiver (identity).

Decline arms, exhaustively:

1. `ArrayLit` where ANY element's recursive `const_lit_of` is `None` (`?` at :1871)
   — declines the WHOLE constant.
2. `HashLit` with `all_assoc == false` (a `**` splat or a non-assoc element) (:1876).
3. `HashLit` where any key is not a static scalar (`const_shape_key_of` → `None`, :1882).
4. `HashLit` where any value's recursive `const_lit_of` is `None` (:1883).
5. The catch-all `_ => None` (:1905): every non-literal RHS — a call chain
   (`%w[].map…to_h`), a `ConstantRead`, a lambda/`->`/`proc`, an interpolated
   string, `Class.new(…)`, an ivar/local read, `if`/`begin` expressions.

Independent of `const_lit_of`, the collector/gate layer (`collect_literal_constants`
:1817 and the C5 block at :516-554) declines: a constant assigned MORE THAN ONCE
project-wide (`lit_multi`); a qualified name colliding with an override class or a
bare name colliding with a source class; a `ConstantWrite` not a direct child of
`Program`/`Statements`/`ClassDef`/`ModuleDef` (a write inside a `def` or an `if` is
never walked); and — note — `ConstantPathWrite` (`A::B::C = …`) is not a collector
arm at all.

Use-site gates: `SourceIndex::literal_constant` (:690, lexical-visibility filter,
innermost namespace wins) and `SourceIndex::qualified_literal_constant` (:727,
stage 2e, candidate keys from the lexical prefix runs, **ambiguity declines**, then
the same visibility filter).

### 2b — consumers of a harvested `ConstLit`

| consumer | site | what it does |
|---|---|---|
| `Typer::intern_const_lit` | `crates/rigor-infer/src/lib.rs:324` | re-interns into `Constant` / `Tuple` / `HashShape` / `Nominal[Range]` against the per-file interner |
| `ConstantRead` arm, C5 bare-name | `lib.rs:494` | first thing tried, BEFORE the C1 singleton gate |
| `ConstantRead` arm, 2e qualified twin | `lib.rs:502` | second, for `A::B::C::CONST` spellings |
| stage-2b `ENV` object-constant gate | `lib.rs:1691` | `literal_constant(name, prefix).is_none()` — a harvested constant DECLINES the `ENV` arm |
| `SourceIndex::project_writes_constant` | `source_index.rs:711` | built from `lit_first` keys BEFORE the literal gates, so it already sees the partially-literal constants; unaffected by widening the harvest |

There is no other consumer: `ConstLit` never escapes `SourceIndex` except through
`intern_const_lit`.

### 2c — where a bare `Nominal[Hash]`/`Nominal[Array]` would flow, and whether it is safe

| downstream | verdict | evidence |
|---|---|---|
| `fold_tuple_projection` (`lib.rs:947`) — `first`/`last`/`size`/`empty?`/`at`/`deconstruct` | **SAFE, inert** — matches `Type::Tuple` only, `_ => return None` | k1, n5 |
| `fold_hash_shape_projection` (`lib.rs:1146`) — `[]`/`fetch`/`dig`/`has_key?`/`values_at` | **SAFE, inert** — matches `Type::HashShape` only | n2, n4, y3 |
| `keys` / `values` / `merge` / `transform_values` block folds | **SAFE** — an argument-less generic resolves nothing; rigor-rs silent where the reference fires | n3, p1e, p1i, p2b |
| `call.undefined-method` (the target rule) | **the intended surface** — the class-only lookup is identical for `Nominal[Hash]` and `HashShape` (both project to the `Hash` descriptor) | n1, n6, y5, z4 |
| `call.wrong-arity` / ATM | **SAFE** — rigor-rs does not reach a bare nominal receiver (reference DOES, so this is coverage loss, not risk) | y1, s1, a1, a2 |
| `possible-nil` / `always-truthy` | **SAFE** — silent on collection carriers in both engines | t1, t2, y3 |
| `call.raise-non-exception` (`concrete_class_name`, `crates/rigor-rules/src/lib.rs:2427`) | **newly reachable**: `Nominal → its class name`, same as `HashShape → "Hash"`. Parity-positive (the reference fires on the shape, r1/r2); a container literal is never an Exception, so it cannot fire where the reference is silent | r1, r2, r3 |
| collection-shape stage-1 mutator widening | **not reachable** — that pass is a per-call `NodeId → "Array"/"Hash"` SNAPSHOT keyed on bare `LocalVariableRead` receivers; it never consults constant types and never produces a `Type` | §7b of the slice spec; h1-h7 (all silent) |
| message rendering | divergent but ACCEPTED — `fp_audit` keys on `(rule, path, line, column)` (`harness/fp_audit.py:6`), and §7c already accepts `for Array` vs `for Array[String]` | n1, n6, simulation table |

### 2d — blast radius

Prism scan of the 8 standing sweep corpora (`harness/sweep-corpora.yml`, 9204 `.rb`
files), classifying every `ConstantWrite`/`ConstantPathWrite` RHS after peeling
`.freeze`:

| class | count |
|---|---|
| scalar literal (harvests today) | 3172 |
| fully-literal container (harvests today) | 953 |
| **literal container with ≥1 non-literal element (would NEWLY harvest)** | **292** (149 hash, 143 array) |
| anything else (chains, `Class.new`, const reads, …) | 3424 |
| — of which literal-ROOTED call chains (the codequality shape, `.freeze`-only excluded) | 357 |

Per corpus (partial-array / partial-hash): gitlab-foss/lib 101/109, mail 25/10,
mastodon/app 11/15, net-ssh 0/10, concurrent-ruby 1/5, dependabot-core 4/0,
haml/lib 1/0, survey/Ruby 0/0.

292 is an **upper bound**: the scan does not apply C5's single-assignment,
class-collision, direct-child-of-a-class-body or lexical-visibility gates, and it
counts `ConstantPathWrite` which the collector does not walk. Read it as "+31% on
the container harvest, concentrated in gitlab-foss".

## 3. Envelope observations (free declines and forced conclusions)

1. **A bare nominal closes routable_token but NOT codequality.** The lambda-hash
   row only needs `DEFAULT_ROUTING_PAYLOAD_HASH` to be *a Hash*: `merge(Dynamic)`
   already folds to `Nominal[Hash]` in rigor-rs and `transform_values{}` +
   `compact_blank` already fire off it (§7c, confirmed by the simulation table).
   The codequality row needs (a) a CHAIN-valued constant — outside "literal
   container with non-literal elements" entirely — and (b) `Hash#keys` to project
   to `Array` off an argument-less nominal, which rigor-rs does not do (n3). Two
   mechanisms, not one; the spec's §7c grouping of them is misleading.
2. **Do NOT type the elements.** z2 is decisive: the reference types a
   constant-read element as `Dynamic[top]` even when that constant is itself a
   harvested literal, while rigor-rs's C5 would fold it. An element-typed harvest
   would make rigor-rs MORE precise than the reference at exactly the projection
   sites the reference declines (`A[1].zzz`) — an FP generator. The bare,
   element-free nominal is the FP-safe carrier precisely because it is inert.
3. **The witnessing set at the direct receiver is identical** for `Nominal[Hash]`
   and the reference's `HashShape`-with-holes: both dispatch to the `Hash`
   descriptor and the undefined-method lookup is class-only. Rendering diverges
   (`for Hash` vs `for { c: Proc }`); the gate does not compare it.
4. **`.freeze` is fully transparent** on every probed shape, and on a NOMINAL the
   reference additionally erases type args (p2a vs p2c) — so a bare-nominal mint
   is, for the `.freeze`d spelling that dominates real corpora, exactly what the
   reference itself carries for the chain case.
5. **Splats and dynamic keys never decline the constant in the reference** (p3b,
   p3b2, p3c, p3d); it degrades to a union or a widened nominal. rigor-rs's
   all-or-nothing decline is the outlier.
6. **Reassignment does not decline it either** (p4b/p4c): the reference unions the
   values, is not flow-ordered (the union is visible at a use site BEFORE the
   second write), and dispatches on the union. C5's `lit_multi` decline is a
   strict under-emit; a partial harvest can keep it unchanged.
7. **Pre-existing, unrelated finding — the reference's constant-value typing is
   PER-FILE; C5's is project-wide, and that is a live over-emission on master.**
   x1/x2: a fully-literal `TOPL = [1,2].freeze` in `a.rb` used in `b.rb` fires in
   rigor-rs and is silent in the reference, with both files in the same `check`
   invocation. §7c's lexical-visibility filter (added for the `DiffBlob::ATTRS`
   FP) suppresses the cross-NAMESPACE case but not the same-namespace cross-FILE
   case. The sweep is 0 FP today only because the shape is rare; any widening of
   the harvest widens this exposure proportionally, and it is arguably worth
   closing (or measuring) FIRST, independently of this mechanism.
