# Sequential-guard meet — the disjoint-vs-refinement rule (2026-08-08)

Closes the pre-existing FP recorded in the PR #76 section of the
[stage-3 spec note](20260807-narrowing-stage3-spec.md): two SEQUENTIAL guards
on one local (`return unless v.is_a?(String)` then `return unless
v.is_a?(Hash)` then a use) are reference-silent — its scope carries the first
class into the second guard and the meet reaches `Bot` — while rigor-rs
witnessed `for Hash`. Root cause: `class_flow_if` ran `join_cenv` (which
retains only `Bot`) BEFORE the early-return propagation, so `apply_guards` saw
an empty env and re-minted the second class from nothing.

The naive fix (apply the carried map against the pre-join snapshot) would have
regressed the SUBCLASS case through the old review-R3 blanket drop, so the fix
is two halves:

1. **`class_flow_if` propagates against the PRE-join facts** and merges only
   the carried locals' results back; every other local keeps the conservative
   join.
2. **`apply_guards` replaces the R3 blanket drop with a meet** — the
   reference's `narrow_nominal_to_class` (`narrowing.rb:2381`) applied to the
   fact's carrier.

## Probe matrix (pin `v0.3.1`, fresh temp cwd, `--no-cache`, plugin path pinned)

Guard pairs are `return unless v.is_a?(…)` unless shown; use is
`v.frobnicate_zzz`; `rs` is the MASTER release binary (`5fe661f`).

| # | pair | ref | rs master | verdict |
|---|---|:--:|:--:|---|
| seq_disjoint | String → Hash | 0 | **1 `for Hash`** | **the FP** — meet → `Bot` |
| seq_raise | same, `raise` spelling | 0 | **1** | same |
| seq_bang | String → `return if !is_a?(Hash)` | 0 | **1** | same |
| seq_third | String → Hash → String | 0 | **1 `for String`** | a third guard cannot revive `Bot` |
| blk_next_disjoint | `next` spelling in a block | 0 | **1** | same |
| ctrl_use_between | use between the guards | 1 (first use) | **2** | second use dead |
| seq_exact_disjoint | String → `instance_of?(Hash)` | 0 | **1** | exact: name mismatch → `Bot` |
| seq_exact_subclass | Numeric → `instance_of?(Integer)` | 0 | **1 `for Integer`** | exact collapses BEFORE the hierarchy (`narrowing.rb:2383`) |
| seq_subclass | Numeric → Integer | 1 `for Integer` | 1 | refinement: the GUARD class wins |
| seq_superclass | Integer → Numeric | 1 `for Integer` | 0 | superclass guard is a no-op; carrier stays (now matched) |
| seq_same | String → String | 1 `for String` | 1 | keep |
| seq_caseeq_same | String → `String === v` | 1 `for String` | 0 | `===` meets too (now matched) |
| seq_caseeq_subclass | Numeric → `Integer === v` | 1 `for Integer` | 0 | `===` refines an EXISTING fact (updating ≠ minting; now matched) |
| seq_caseeq_disjoint | String → `Hash === v` | 0 | 0 | → `Bot` |
| seq_nilq | String → `return unless v.nil?` | 0 | 0 | `NilClass` disjoint → `Bot` |
| seq_or_disjoint | String → `Hash \|\| Array` union | 0 | 0 | every member disjoint → `Bot` |
| seq_or_mixed | String → `Hash \|\| String` union | 1 `for String` | 0 | DECLINE: live member — we drop |
| seq_projclass | String → in-source class | 1 `for String` | 0 | DECLINE: `Unknown` ordering — ref keeps the carrier; our index may miss a disjointness the ref proves, so silence |
| br_disjoint | String → non-terminating `if is_a?(Hash)` branch use | 0 | 0 | edge meet → `Bot` (was R3 drop — silent either way) |
| br_subclass | Numeric → branch `is_a?(Integer)` | 1 `for Integer` | 0 | edge refinement (now matched) |
| br_superclass | Integer → branch `is_a?(Numeric)` | 1 `for Integer` | 0 | edge no-op (now matched) |
| br_else_keeps | String → else edge of disjoint branch | 1 `for String` | 1 | held |
| ctrl_single / ctrl_write_between | — | 1 | 1 | must-still-fire, held |

## The meet (per local, existing fact `Narrowed(C1)`, guard classes `[C2]`)

- `C1 == C2` → keep (also for non-mintable `===`).
- `exact` and name mismatch → `Bot` — before the hierarchy.
- `class_ordering(C1, C2)`: `Superclass` (C2 refines) → `Narrowed(C2)`,
  mintable or not (the fact already exists; updating is not minting);
  `Subclass`/`Equal` → keep `C1`; `Disjoint` → `Bot`; `Unknown` → DROP (the
  reference keeps `C1` and can fire — coverage cost — but an ordering our
  index cannot resolve may be one the reference proves disjoint).
- Union guard: `Bot` iff EVERY member is disjoint (exact: mismatched), else
  drop (`seq_or_mixed` is the recorded price).

`Numeric` single-guard witnessing is a pre-existing orthogonal gap
(`return unless v.is_a?(Numeric)` then use: ref fires `for Numeric`, rs
silent on master and after) — not touched here.

The `case`/`when` clause path keeps its old R3 drop: the sequential-disjoint
shape through a `when` is silent on both sides today; clause-edge refinement
(`when Integer` after a Numeric guard) stays a decline.

## Gates (all green)

`cargo test --offline` (15 suites; new
`class_narrowing_sequential_guard_meet_matrix`, 27 rows); `ruby
harness/run.rb` **90 fixtures, 0 unregistered extras** (332/360 matched, 27
gaps, 1 registered; new fixture `90_sequential_guard_meet.rb` adds 6 matched
rows); `ruby harness/snapshot.rb` + `run_snapshot.rb`; clippy `-D warnings`
fresh `CARGO_TARGET_DIR`; `python3 harness/fp_audit.py --gaps --sweep` on the
fresh release binary — **TOTAL FP candidates: 0**, 8 corpora / 9204 files;
`gap_census.py --sweep` diff vs the 1136 baseline: see below.
