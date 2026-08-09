# The unresolved-constant-receiver carrier (2026-08-09)

The census row the join-wipe slice could not close
([20260809-join-wipe-retention.md](20260809-join-wipe-retention.md), "The
census row that did not close"): gitlab-foss `lib/bulk_imports/
object_counter.rb:52` (`Hash#symbolize_keys`). A local bound from a call whose
RECEIVER is a constant path was unconditionally coarse — `narrowable_binding`'s
allow-list (the 2026-08-08 carrier-fidelity fix) had declined ALL constant
receivers because the one measured probe (`recv_const_float`,
`Float::INFINITY.abs`) was a live FP.

The split that fixes it: a constant receiver is hazardous exactly when it
RESOLVES. A resolvable constant can hand the reference a precise return
(`Float::INFINITY.abs` → `Float`; `ENV.fetch` → `String`; an in-source
`self.mk` whose tail is a `Logical` → a union — the `fp2` shape through a
constant receiver), and narrowing a carrier the reference types precisely is
the carrier-fidelity FP again. A constant that resolves to NOTHING — no RBS
class root, no RBS object constant, no project-defined name — yields an
untyped call result on BOTH engines, and the reference narrows it
(`narrow_class_other`) and fires.

`Typer::unresolved_constant_root` (crates/rigor-infer) is that gate: the ROOT
segment of the receiver path (leading `::` stripped) must miss
`index.knows_class` (short-key table — deliberately the WIDE one),
`index.object_constant_class`, and `source.constant_defined_anywhere`.
Scope-independent and conservative toward declining.

## Oracle matrix (pin v0.3.2/c6b91b9e, fresh temp cwd, --no-cache, both ref libs on -I)

Shape family `x = <RHS>; return unless x.is_a?(Hash); x.symbolize_keys`:

| probe | RHS | ref | rigor-rs after |
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

The naive allow (any constant receiver, relying on the tenv Dynamic/Top mint
gate) was measured UNSOUND before gating: c1, c4 and c8 all fired — rigor-rs
types `Float::INFINITY` and `ENV` as `Dynamic`, so the type gate never sees
the precision the reference has. The p6/p7/p8 declines are accepted coverage
cost, pinned as `cost_const_recv`/`cost_insource_const_recv` in the matrix
test: the SAME resolution that makes them fire here hands the reference a
precise return elsewhere, and there is no per-method discriminator worth the
risk.

## The census row STILL does not close — and this time the blocker is final

Measured, not predicted. `object_counter.rb:52` fires when the file is
analysed ALONE, and is still a gap in the corpus-wide run:

| run | rigor-rs |
|---|:--:|
| `rigor check <the one file>` | **fires** (line 52, col 46) |
| `fp_audit`/`gap_census` over `gitlab-foss/lib` (4676 files) | still a gap |

The difference is `constant_defined_anywhere("Gitlab")`. In a real Rails
corpus the namespace ROOT is always project-defined (`lib/gitlab.rb`), so the
gate declines exactly where the shape actually occurs. Loosening the root
check does not help: the FULL path `Gitlab::Cache::Import::Caching` is
project-defined too, and so is the method —

```ruby
def self.values_from_hash(raw_key)
  key = cache_key_for(raw_key)

  with_redis do |redis|      # <- the return TAIL
    redis.hgetall(key)
  end
end
```

— so closing this row needs the reference's INTERPROCEDURAL return-tail
inference, on two carriers the allow-list already declines by measurement: an
in-source singleton method's return (`cost_implicit_self_insource`) reached
through an IMPLICIT-SELF call (`cost_implicit_self`, `fp_via_method_toplevel`
— the `Logical`-tail FP). That is the deferred Logical-union / return-tail
arc, already adjudicated as "a dedicated arc, not a slice"
([verdicts](20260809-deferred-slices-and-upstream-feedback.md)), NOT a carrier
gate. The census row is closed to carrier work; do not re-attempt it from this
direction.

## Measured value: 0 rows

Standing sweep after: **9204 files, 0 FP candidates, 841 coverage gaps** —
byte-identical to the 2026-08-09 baseline, `gitlab-foss/lib` still exactly 170.
Nothing closed anywhere in the set, and nothing opened. The shape is real and
oracle-matched, but it does not OCCUR in project-shaped Ruby: real code
namespaces its constants, and a namespaced constant is project-defined.

The standing lesson (verify the closeable pattern OCCURS before building)
applies to CARRIER shape as well as to rule frequency — a probe family written
as single free-standing files cannot tell you whether the discriminating
predicate (here: "the project does not define this constant") ever holds in a
real corpus. The probes should have been run against a project-shaped corpus,
not only against `/tmp` one-liners.

Kept rather than reverted: it is FP-safe, oracle-gated, and it closes two of
the three carrier rows the join-wipe note measured as blocked
(`Gitlab.values_from_hash`, `Gitlab::Cache.values_from_hash`). The third
(a bare self-call) is the implicit-self decline, deliberately untouched.

## Gates

- `class_narrowing_carrier_fidelity_matrix` — 6 new `ok_unresolved_*` rows +
  `fp_env_recv`/`fp_insource_const_recv`/`cost_insource_const_recv`.
- Fixture `harness/corpus/96_narrowing_unresolved_const_receiver.rb` — 7
  matched firings, 4 silent controls; live gate 96 fixtures / 0 unregistered
  FPs, snapshot gate PASS, the other 95 snapshots byte-unchanged.
- Cross-file check (a project constant defined in ANOTHER file of the batch):
  both engines silent — our `SourceIndex` is project-wide, so the gate does not
  leak on a multi-file batch.
- Full workspace tests (700 pass) + clippy in a fresh `CARGO_TARGET_DIR`: clean.
- Standing sweep: 0 FP / 9204 files, 841 gaps (unchanged).
