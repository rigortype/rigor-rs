# The unresolved-constant-receiver carrier — BUILT, MEASURED AT 0, NOT SHIPPED (2026-08-09)

The census row the join-wipe slice could not close
([20260809-join-wipe-retention.md](20260809-join-wipe-retention.md), "The
census row that did not close"): gitlab-foss `lib/bulk_imports/
object_counter.rb:52` (`Hash#symbolize_keys`). The carrier gate named there as
"the next lever" was built, measured, and **discarded**. The code is on the
closed [PR #89](https://github.com/rigortype/rigor-rs/pull/89); this note is
the part worth keeping.

**Verdict: the row is CLOSED TO CARRIER WORK. Do not re-attempt it from that
direction.**

## What was built

`narrowable_binding`'s allow-list (the 2026-08-08 carrier-fidelity fix)
declined EVERY constant receiver, on the strength of one probe
(`recv_const_float`, `Float::INFINITY.abs`) that was a live FP. That is too
coarse: a constant receiver is hazardous exactly when it RESOLVES. A resolvable
constant hands the reference a real return (`Float::INFINITY.abs` → `Float`;
`ENV.fetch` → `String`; an in-source singleton whose tail is a `Logical` → a
union — the `fp2` shape through a constant receiver). A constant resolving to
NOTHING is untyped on both engines, and the reference narrows it
(`narrow_class_other`).

The gate was ~15 lines: a `Typer::unresolved_constant_root(name)` predicate —
the ROOT segment of the receiver path (leading `::` stripped) must miss all
three of `index.knows_class` (the WIDE short-key table),
`index.object_constant_class`, and `source.constant_defined_anywhere` —
threaded into `narrowable_binding`/`coarse_locals` as a callback so the
`Node::Call { receiver: Some(ConstantRead) }` arm could allow it.

## Oracle matrix (pin v0.3.2/c6b91b9e, fresh temp cwd, --no-cache, both ref libs on -I)

Shape family `x = <RHS>; return unless x.is_a?(Hash); x.symbolize_keys`:

| probe | RHS | ref | rigor-rs with the gate |
|---|---|:--:|:--:|
| p1 | `Gitlab.values_from_hash(k)` | fires | fires |
| p2 | `Gitlab::Cache.values_from_hash(k)` | fires | fires |
| p9 | `::Gitlab.values_from_hash(k)` | fires | fires |
| p3 | `Gitlab.foo(k).bar` (chained) | fires | fires |
| p4 | `Gitlab.foo(k) { \|a\| a }` (block) | fires | fires |
| p5 | `Gitlab&.foo(k)` (safe-nav) | fires | fires |
| r5_full | the census shape (`class << self`, guard pair, argument-position use) | fires | fires |
| p6 | `Hash.frobnicate_zzz(k)` (RBS root, unknown method) | fires | declines (cost) |
| p7/p8 | in-source class, unknown/known method | fires | declines (cost) |
| c1 | `Float::INFINITY.abs` | silent | silent |
| c4 | `ENV.fetch(k)` | silent | silent |
| c8 | in-source `self.mk` with `Logical` tail | silent | silent |
| c12/c13/c14 | project const = tuple literal (toplevel / nested / `.freeze`d) | silent | silent |
| c3/c5/c6 | `File.basename` / `Math.sqrt` / `Time.now` | silent | silent |

The naive version (allow ANY constant receiver, relying on the existing tenv
`Dynamic`/`Top` mint gate) was measured **unsound** before gating: c1, c4 and
c8 all fired. rigor-rs types `Float::INFINITY` and `ENV` as `Dynamic`, so the
type gate never sees the precision the reference has — the same inversion as
the 2026-08-08 carrier-fidelity FP.

## Why the census row STILL does not close

Measured, not predicted:

| run | rigor-rs with the gate |
|---|:--:|
| `rigor check <the one file>` | **fires** (line 52, col 46) |
| `gap_census.py` over `gitlab-foss/lib` (4676 files) | still a gap |

The difference is `constant_defined_anywhere("Gitlab")`. In a real Rails corpus
the namespace ROOT is always project-defined (`lib/gitlab.rb`), so the gate
declines exactly where the shape occurs. Loosening the root check does not
help: the full path `Gitlab::Cache::Import::Caching` is project-defined, and so
is the method —

```ruby
def self.values_from_hash(raw_key)
  key = cache_key_for(raw_key)

  with_redis do |redis|      # <- the return TAIL
    redis.hgetall(key)
  end
end
```

— so closing this row needs the reference's INTERPROCEDURAL return-tail
inference, on two carriers the allow-list declines by measurement: an in-source
singleton method's return (`cost_implicit_self_insource`) reached through an
IMPLICIT-SELF call (`cost_implicit_self`, `fp_via_method_toplevel` — the
`Logical`-tail FP). That is the deferred Logical-union / return-tail arc,
already adjudicated as "a dedicated arc, not a slice"
([verdicts](20260809-deferred-slices-and-upstream-feedback.md)).

## Measured value: 0 rows

Standing sweep with the gate: **9204 files, 0 FP candidates, 841 coverage
gaps** — identical to the 2026-08-09 baseline, `gitlab-foss/lib` still exactly
170. Nothing closed anywhere in the set; nothing opened. The shape is real and
oracle-matched, but it does not OCCUR in project-shaped Ruby, because real code
namespaces its constants and a namespaced constant is project-defined.

## Why it was not shipped

Three reasons, in order of weight:

1. **0 measured rows.** The standing rule (never ship a slice whose measured
   payoff is 0) applies unchanged.
2. **It would have been the first allow-list member whose FP-safety depends on
   the two engines' INDEXES agreeing.** Every other member (a parameter, an
   ivar, a call through one) is `Dynamic` on both sides *by construction*, with
   no index lookup involved. This gate instead asks rigor-rs's own
   `knows_class` / `object_constant_class` / `constant_defined_anywhere`
   whether a constant is unknown — so any constant the reference resolves and
   we do not is an FP. The standing sweep runs both sides core+stdlib only and
   is therefore structurally blind to that dependency in gem/bundle mode.
   Project `sig/` WAS checked by hand and is safe in the conservative
   direction (a `sig/` class enters `knows_class`, so the gate declines more);
   gem/bundle mode was not, and a 0-payoff slice does not justify building
   that probe set.
3. The row it was built for does not close.

## The reusable lesson

"Verify the closeable pattern OCCURS before building" applies to CARRIER SHAPE,
not only to rule frequency. The probe family here was 14 free-standing
single files in a scratch dir; every one measured correctly against the oracle,
and the slice was still worth 0 rows across 9204 files. A single-file probe
**cannot** exercise a discriminating predicate of the form "the project does
not define this" — a one-file probe has no project. The isolation-vs-corpus
split WAS the finding.

**Run the real corpus path before building**: `rigor check <the actual file>`
AND `gap_census.py --dump` over the corpus, and compare. Had that been done
first, this slice would have been rejected at the design stage.
