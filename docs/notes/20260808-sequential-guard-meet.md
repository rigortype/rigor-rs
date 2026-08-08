# Sequential-guard meet — the disjoint-vs-refinement rule (2026-08-08)

Closes the sequential-guard FP family recorded in the PR #76 section of the
[stage-3 spec note](20260807-narrowing-stage3-spec.md). Two sequential guards
on one local leave the reference's scope carrying the FIRST guard's class into
the second, whose narrowing is `narrow_nominal_to_class` (`narrowing.rb:2381`)
— a real MEET, not a drop. The S2 half of the qualified-witnessing arc
(PR #82) had already stopped the original disjoint re-mint by re-seeding the
pre-join facts into the propagation and letting review-R3's blanket drop
collide — but a drop is the wrong meet, and on the S2 base FIVE spellings were
still live FPs and the whole refinement family was silenced against the
reference. This slice replaces the local-side R3 collision with the meet
itself.

## The meet (existing fact `Narrowed(C1)`, guard classes `[C2]`)

Direct port of `narrow_nominal_to_class`:

- `C1 == C2` → keep (`===` included — `seq_caseeq_same` fires `for String`).
- `exact` (`instance_of?`) name mismatch → `Bot` BEFORE the hierarchy
  (`narrowing.rb:2383` — `seq_exact_subclass` is reference-silent).
- `class_ordering(C1, C2)`: `Superclass` (C2 refines) → `Narrowed(C2)`,
  mintable or not (the fact exists; updating is not minting — `Integer === v`
  after a Numeric guard fires `for Integer` on the reference);
  `Subclass`/`Equal` → keep `C1`; `Disjoint` → `Bot`; **`Unknown` splits on
  WHY the ordering failed**: a PROJECT-declared class (`SourceIndex`
  `knows_class`) is unknown to the reference's RBS env too, and `:unknown
  stays conservative` (`narrowing.rb:2388`) KEEPS the carrier there — measured
  even when the project hierarchy would prove disjointness (probe `projsub`:
  `ProjKlass < Hash` after a String guard still fires `for String`); but an
  ordering that fails on two RBS-SPACE names is OUR resolver being weaker than
  the reference's — it proves `File::Stat` vs `URI::HTTP` disjoint and is
  silent (the S2 probe r7) — so keeping would be a live FP: DROP.
- An `||` union meets PER MEMBER and unions the results: all-disjoint → `Bot`
  (`seq_or_disjoint`); one survivor → that class (`seq_or_mixed`:
  `Bot ∪ String` fires `for String`; `projsub_or` keeps through a
  project-class member); two survivors → a real union → drop; an RBS-space
  `Unknown` member poisons the whole union → drop.

Chains (3a-3) keep the plain R3 drop: the disjoint half is the same silence,
the refinement half is an unprobed decline.

## Probe matrix (pin `v0.3.1`, fresh temp cwd, `--no-cache`, plugin pinned)

Guard pairs are `return unless v.is_a?(…)` unless shown; use is
`v.frobnicate_zzz`; `rs` columns are the release binaries of master `94250f8`
(post-S2) and this branch. **The branch matches the reference on every row.**

| # | pair | ref | rs 94250f8 | rs branch |
|---|---|:--:|:--:|:--:|
| seq_disjoint | String → Hash | 0 | 0 (S2) | 0 |
| seq_raise / seq_bang / blk_next_disjoint | `raise` / `!` / `next` spellings | 0 | 0 (S2) | 0 |
| seq_third | String → Hash → String | 0 | **1 `for String`** | 0 |
| seq_caseeq_disjoint | String → `Hash === v` | 0 | **1 `for String`** | 0 |
| seq_nilq | String → `return unless v.nil?` | 0 | **1 `for String`** | 0 |
| seq_or_disjoint | String → `Hash \|\| Array` | 0 | **1 `for String`** | 0 |
| seq_exact_disjoint | String → `instance_of?(Hash)` | 0 | 0 | 0 |
| seq_exact_subclass | Numeric → `instance_of?(Integer)` | 0 | 0 | 0 |
| br_disjoint / br_exact_disjoint | non-terminating branch use | 0 | 0 | 0 |
| r7 (S2) | `File::Stat` → `URI::HTTP` | 0 | 0 | 0 (Unknown-drop) |
| seq_caseeq_subclass | Numeric → `Integer === v` | 1 `for Integer` | **1 `for Numeric`** (wrong class) | 1 `for Integer` |
| seq_subclass | Numeric → Integer | 1 `for Integer` | 0 | 1 `for Integer` |
| seq_superclass | Integer → Numeric | 1 `for Integer` | 0 | 1 `for Integer` |
| blk_next_subclass | `next` spelling, Numeric → Integer | 1 `for Integer` | 0 | 1 `for Integer` |
| br_subclass / br_superclass | branch-edge refinement | 1 `for Integer` | 0 | 1 `for Integer` |
| seq_same / seq_caseeq_same | re-guard, `is_a?` / `===` | 1 `for String` | 1 | 1 |
| seq_or_mixed | String → `Hash \|\| String` | 1 `for String` | 1 | 1 |
| seq_projclass / projsub / projsub_or | project-class 2nd guard (± `< Hash`, ± union) | 1 `for String` | 1 | 1 |
| ctrl_single / ctrl_write_between / ctrl_use_between / br_else_keeps | controls | 1 | 1 | 1 |

Vs master `94250f8`: **5 live FPs closed** (`seq_third`, `seq_caseeq_disjoint`,
`seq_nilq`, `seq_or_disjoint`, and `seq_caseeq_subclass`'s wrong-class
witness), **6 rows newly matched** (the refinement family), **0 rows lost**.
The S2 note's r7 shape (`File::Stat` → `URI::HTTP`) stays silent via
`Disjoint → Bot`.

`Numeric` single-guard witnessing (`x_single_numeric`) was a pre-existing gap
closed independently by the qualified-witnessing arc — both engines fire
`for Numeric` now; not this slice.

## Gates (all green, post-rebase onto `94250f8`)

`cargo test --offline` (new `class_narrowing_sequential_guard_meet_matrix`, 30
rows); `ruby harness/run.rb` — **93 fixtures, 0 unregistered extras** (new
fixture `93_sequential_guard_meet.rb`); `ruby harness/snapshot.rb` +
`run_snapshot.rb`; clippy `-D warnings` fresh `CARGO_TARGET_DIR`; `python3
harness/fp_audit.py --gaps --sweep` fresh release binary — **TOTAL FP
candidates: 0**, 8 corpora / 9204 files; `gap_census.py --sweep --dump`
row-diffed against a fresh `94250f8` dump — see the ledger line for the
numbers. The sweep contains no instance of the sequential shape (dead code),
so the slice's value is FP hygiene + probe-set fidelity.
