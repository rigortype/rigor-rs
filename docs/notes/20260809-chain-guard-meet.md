# Chain-guard meet — the `Bot` sentinel and the refinement half (2026-08-09)

The chain half of the [sequential-guard meet](20260808-sequential-guard-meet.md).
PR #78 replaced the review-R3 blanket drop with the reference's
`narrow_nominal_to_class` meet for LOCAL narrowing facts; stage-3a-3 CHAIN facts
(`GuardTarget::Chain`, address `(root local, no-arg method)` —
`h.last.is_a?(C)`) kept the old blind compare, *keep if the class is equal, else
remove*, because `chains: HashMap<ChainAddr, String>` could not represent a
collapse.

A 26-row oracle matrix says the chain family behaves **identically** to the
local one. So the meet is now literally the same code — `narrow_nominal_to_class`
was extracted out of the local arm and both arms call it.

## The change

1. `Facts.chains` is `HashMap<ChainAddr, ClassFact>` (was a bare class name).
   `Bot` is a SENTINEL: it survives every later guard (the chain twin of the
   `guard_collapses` Bot short-circuit) and, at the consumption site, records
   the call node into `ClassNarrowing::dead` instead of `calls`.
2. `apply_guards`'s `Chain` arm: Bot short-circuit → R3 conflict → the shared
   meet → the mint. The MINT path is untouched: `g.mintable`,
   `classes.len() == 1`, and the carrier gate off `type_of(chain_call)` (no
   `coarse` allow-list — `k_root_or_union`).
3. `join_cenv` still wipes chain facts unconditionally, a chain `Bot` included:
   a `Bot` escaping a branch would SUPPRESS rather than merely go silent, and
   the reference does not carry a branch-established chain narrowing out
   (`n_escape_after_if`). The narrow pre-join re-seed in `class_flow_if` carries
   the whole `ClassFact`, so a `Bot` crosses the early-return propagation
   exactly as a `Narrowed` does — that is what makes `chain_third` stick.
4. `kill_local` / `kill_chains_rooted_at` / `kill_cenv_narrowed` are unchanged:
   a rebind or a call on the ROOT invalidates the ADDRESS, so the collapse no
   longer describes anything and dies with it (`chain_ctrl_rebind`,
   `chain_ctrl_pop_between` — the reference mints `Hash` in both, and so do we).

## Why a sentinel and not "absent"

"Absent" is the wrong lattice element. `chain_third`
(`String` → `Hash` → `String`) removed the fact on the disjoint second guard and
the THIRD guard then re-minted `String` against an empty env and witnessed —
reference-silent, a live FP. The same defect in the local family was the reason
S2's drop was replaced by the meet in the first place.

## The meet (existing fact `Narrowed(C1)`, guard classes `[C2]`)

Unchanged from the local slice — see that note for the derivation. `C1 == C2`
keep; `exact` name mismatch → `Bot` before the hierarchy;
`class_ordering(C1, C2)`: `Superclass` → `Narrowed(C2)` (refine),
`Subclass`/`Equal` → keep `C1`, `Disjoint` → `Bot`, `Unknown` → keep iff either
side is a PROJECT-declared class (`SourceIndex::knows_class`) else DROP; an `||`
union meets per member and unions the survivors (0 → `Bot`, 1 → that class,
2+ → drop; an RBS-space `Unknown` member poisons the union → drop).

The `Unknown` split is load-bearing here and would be easy to lose: the three
project-class rows passed BEFORE this slice only because a project-class guard
is non-mintable and the arm skipped. Once the meet runs, dropping the split
regresses them.

## Probe matrix (pin `v0.3.1`, fresh temp cwd per scenario, `--no-cache`, plugin path pinned)

Root is an untyped param `h`; the address is `h.last`; guards are
`return unless h.last.is_a?(…)` unless shown; use is `h.last.frobnicate_zzz`.
`rs` columns are the RELEASE binaries of master `fc91cd2` and this branch.

| row | guards | ref | rs `fc91cd2` | rs branch |
|---|---|:--:|:--:|:--:|
| chain_same | String → String | 1 `for String` | 1 | 1 |
| chain_disjoint | String → Hash | 0 | 0 | 0 (now `Bot`) |
| chain_bang | String → `return if !…Hash` | 0 | 0 | 0 |
| chain_third | String → Hash → String | 0 | **1 `for String`** | 0 |
| chain_or_disjoint | String → `Hash \|\| Array` | 0 | **1 `for String`** | 0 |
| chain_or_mixed | String → `Hash \|\| String` | 1 `for String` | 1 | 1 |
| chain_subclass | Numeric → Integer | 1 `for Integer` | 0 | 1 `for Integer` |
| chain_superclass | Integer → Numeric | 1 `for Integer` | 0 | 1 `for Integer` |
| chain_br_subclass | Numeric → `if …Integer` | 1 `for Integer` | 0 | 1 `for Integer` |
| chain_br_superclass | Integer → `if …Numeric` | 1 `for Integer` | 0 | 1 `for Integer` |
| chain_br_disjoint | String → `if …Hash` | 0 | 0 | 0 |
| chain_br_else_keeps | String → `if …Hash` / else use | 1 `for String` | 1 | 1 |
| chain_exact_disjoint | String → `instance_of?(Hash)` | 0 | 0 | 0 |
| chain_exact_subclass | Numeric → `instance_of?(Integer)` | 0 | 0 | 0 |
| chain_r7 | `File::Stat` → `URI::HTTP` | 0 | 0 | 0 (Unknown-drop) |
| chain_projclass | String → `ProjKlass` | 1 `for String` | 1 | 1 |
| chain_projsub | String → `ProjKlass < Hash` | 1 `for String` | 1 | 1 |
| chain_projsub_or | String → `Hash \|\| ProjKlass` | 1 `for String` | 1 | 1 |
| chain_ctrl_single | String | 1 `for String` | 1 | 1 |
| chain_ctrl_rebind | String → `h = w` → Hash | 1 `for Hash` | 1 | 1 |
| chain_ctrl_pop_between | String → `h.pop` → Hash | 1 `for Hash` | 1 | 1 |
| chain_ctrl_use_between | String → use → Hash → use | 1 `yyy for String` | 1 (`yyy` only) | 1 (`yyy` only) |
| chain_caseeq_same | String → `String === h.last` | 1 `for String` | 0 | 0 (recognition) |
| chain_caseeq_subclass | Numeric → `Integer === h.last` | 1 `for Integer` | 0 | 0 (recognition) |
| chain_nilq | String → `h.last.nil?` | 1 `for String` | 0 | 0 (recognition) |
| chain_caseeq_disjoint | String → `Hash === h.last` | 0 | 0 | 0 |

Vs master `fc91cd2`: **2 live FPs closed** (`chain_third`, `chain_or_disjoint`),
**4 rows newly matched** (the refinement family), **0 rows lost**. The branch
matches the reference on 23 of 26 rows.

The three remaining declines are a RECOGNITION gap, not a meet gap, and are out
of scope: `guard_predicate` requires a bare LOCAL operand, so `===` and `nil?`
with a chain receiver never produce a `GuardTarget::Chain` at all. Closing them
means touching guard recognition, which this slice deliberately did not.

## Gates (all green)

- `cargo build --offline && cargo test --offline` — new
  `class_narrowing_chain_guard_meet_matrix` (all 26 rows above, asserting the
  snapshot class AND `dead`-set membership). One pre-existing row flipped and
  was updated with its oracle measurement: `d_seq_subclass` in
  `class_narrowing_stage3a3_chain_guard_matrix` was a recorded DECLINE
  (`None`) and now refines to `Some("Integer")`, which is what the reference
  emits.
- `ruby harness/run.rb` — **94 fixtures, 0 unregistered extras**; new fixture
  `harness/corpus/94_chain_guard_meet.rb` (11 scenarios: 6 firing lines,
  5 silent families), every expected line oracle-verified before registering.
- `ruby harness/snapshot.rb` + `ruby harness/run_snapshot.rb` — PASS.
- clippy `-D warnings` in a FRESH `CARGO_TARGET_DIR` — clean.
- `python3 harness/fp_audit.py --gaps --sweep` on a fresh release binary —
  **TOTAL FP candidates: 0**, 8 corpora / 9204 files.
- `python3 harness/gap_census.py --sweep --dump`, row-diffed against a
  `fc91cd2` dump built the same way — see the ledger line.

The sweep corpora contain no instance of the sequential chain-re-guard shape
(it is dead code in real Ruby), so the slice's value is FP hygiene plus probe-set
fidelity, not a census delta.
