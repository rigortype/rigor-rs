# Coverage-gap census — what the 1193 gaps actually are (2026-08-07)

`fp_audit.py --gaps` answers *which rules* the gaps are in. That is the wrong
question to plan from: the 446 `call.undefined-method` gaps are half a dozen
unrelated mechanisms with very different closability, and a per-rule histogram
hides which. `harness/gap_census.py` (new) classifies every gap by the
**mechanism** behind it — receiver shape and method, taken from the reference's
own message — so a slice can be chosen from measured frequency.

Measured on the standing sweep set (8 corpora, 9204 files, pin `v0.3.1`).

## Where the 1193 gaps are

| rule | gaps | verdict |
|---|---:|---|
| `call.undefined-method` | 446 | **the actionable pool** — decomposed below |
| `call.possible-nil-receiver` | 435 | behind a standing conclusion (Tier B/C track CLOSED) |
| `flow.always-truthy-condition` | 141 | flow frontier, no cheap FP-safe wins |
| `call.argument-type-mismatch` | 101 | mostly `expected X, got X?` — the same nilable substrate as possible-nil |
| everything else | 70 | long tail (arity 21, return-type 13, ivar-write 11, …) |

**48% of the gap set (576) sits behind decisions this project already made and
should not re-litigate.** Reporting "1193 gaps" as one number invites exactly
that mistake.

## The 446 `call.undefined-method` gaps, by mechanism

| mechanism | gaps | concentration |
|---|---:|---|
| bare class receiver | 147 | gitlab-foss `lib` 83, concurrent-ruby 36 |
| shape / collection receiver (`Hash[…]`, `Array[…]`) | 92 | lib 45, mail 36 |
| namespaced class receiver | 69 | mail 47 |
| receiver typed `nil` | 63 | dependabot 22, mail 14 |
| **project monkey-patch (the reference's own ADR-17 `pre_eval:` hint)** | 49 | mail 49 |
| singleton receiver | 12 | mail 10 |
| refined / literal receiver (`bool`, `int<0, 4>`, `0.9..0.95`) | 14 | lib |

258 of them name a plain class, across **40 distinct classes**. The split inside
that is what matters:

- **~110 on CORE classes** — `String` 52, `Object` 26, `Hash` 17, `Class` 8,
  `Array` 7. The port *has* these signatures; it simply did not resolve the
  receiver. This is **receiver typing**, not missing knowledge.
- **~77 on the analysed project's own classes** — `RDoc::Markup::Document` 20,
  `RDoc::AnyMethod` 13, `RDoc::Attr` 12, `RDoc::Constant` 10, … all inside
  `mail`'s vendored bundle. 49 carry the reference's own hint that the project
  monkey-patches the class in another file and that `pre_eval:` would be needed
  (ADR-17) — a mechanism the port does not have at all.
- The rest are stdlib/gem classes (`OpenStruct` 18, `Bundler::Source::Git` 7).

## Three named, closable sub-mechanisms (with evidence)

### 1. `is_a?` / `case … when` class narrowing

The single clearest cluster. gitlab's `case_transform.rb` is the archetype:

```ruby
case value
when Hash   then value.deep_transform_keys! { |k| camel_lower(k) }   # gap
when String then value.underscore.camelize(:lower)                   # gap
end
# and the ternary form
rule.is_a?(Hash) ? rule.with_indifferent_access : rule               # gap
```

The reference narrows the tested local inside the branch; rigor-rs leaves it
`Dynamic`, so every call on it goes unwitnessed. **54** `undefined-method` gaps
have an `is_a?`/`kind_of?`/`when <Const>` test within 8 lines above the gap
(a crude window, so treat it as an upper bound with a solid core, not a
promise).

### 2. `X.to_s` is a `String` whatever `X` is

`awardable_params[:resource].to_s.singularize` — the reference types the `to_s`
result `String` and witnesses `singularize`; rigor-rs declines because the
receiver is `Dynamic`. `Object#to_s -> String` holds for every receiver in RBS.
Cheap, and it feeds mechanism 1's sites too.

> **CORRECTION (2026-08-07, same day): REFUTED by oracle probe — do not
> build.** The reference is SILENT on `dynamic.to_s.frobnicate_zzz` (its RBS
> dispatch declines a `Dynamic[Top]` receiver), so there is no universal fold
> to mirror; an unconditional fold would be FP by construction. The 4 sites
> reach `String` because the reference types the *block param* cross-file.
> [spec note](20260807-class-narrowing-slice-spec.md) § "Slice 2 is REFUTED".

### 3. Inherited singleton returns

```ruby
Digest::SHA256.hexdigest("x")   # reference: String   rigor-rs: Dynamic[top]
```

`self.hexdigest` is declared on the ancestor `Digest::Class`. Both engines
resolve the constant (`Digest::SHA256.new` types correctly here), so this is the
singleton-return lookup not reaching through the ancestor chain for this shape.
Worth isolating before building — the class is declared in rbs's *openssl* tree,
which neither engine's `DEFAULT_LIBRARIES` closure carries, so the provenance of
the reference's answer is itself part of the question.

> **CORRECTION (2026-08-07, same day, measured): both suspicions were wrong.**
> The declaring file is rbs stdlib `digest` (in BOTH engines'
> `DEFAULT_LIBRARIES`, byte-identical vendored), and ancestor singleton lookup
> works for top-level names. The real mechanism: the return-type lookup family
> (`method_return`, `singleton_method_return` + twins) never received the
> ADR-0042 qualified-key routing that the existence gates got — ANY method's
> declared return on a NAMESPACED receiver is lost (instance and singleton,
> own and inherited alike). ~14 solid gap closures predicted. Buildable as
> ADR-0042 Slice 5: [spec](20260807-adr0042-s5-return-lookup-spec.md).

## How to re-run

```sh
python3 harness/gap_census.py --sweep --dump /tmp/gaps.json
```

Buckets come from the reference's message text; `--dump` writes every row
(corpus, rule, receiver, method, path, line, message) for reading a bucket case
by case. Re-run it after any receiver-typing or flow change — the *shape* of the
gap set is the thing that moves, and the per-rule totals barely show it.

## What this says about effort

Do not spend on possible-nil / always-truthy: 576 gaps, all behind existing
decisions. Spend on **receiver typing**, where ~110 core-class gaps say the port
has the answer and cannot see the question. Mechanism 1 is the first slice;
mechanism 2 is smaller than it looks but feeds it; mechanism 3 needs a
measurement before a build. The `pre_eval:` cluster (49) is a genuine missing
feature and its own arc, not a slice.
