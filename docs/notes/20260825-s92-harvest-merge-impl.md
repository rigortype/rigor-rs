# S92 harvest/merge — implementation (2026-08-25)

Implements [the mini-spec](20260825-s92-harvest-merge-mini-spec.md) from
[the pass inventory](20260825-s92-buildproject-pass-inventory.md) (issue #92).
Two commits, pure refactor, gated on bit-identity — **no emitted diagnostic
changes anywhere**, under any file order or thread count.

## What moved where

`SourceIndex::build_project(asts, core)` is now literally

```rust
merge(asts.map(|a| (harvest(a, core), a)), core)
```

| pass | before | after |
|---|---|---|
| 1 source classes, 1b override index, 1c toplevel defs, 1d discovered methods, 1e mutated params, C5a constant writes, 2 RBS constant reads, 4a fold-def walk | inline in `build_project`, looping `for ast in asts` | **`SourceIndex::harvest`** — one file, no accumulated state |
| — replay of the above | — | **merge M1**, pass by pass across files in the caller's order |
| C1 shadow tables, C5b constant gates, 2b declaration-only registry | inline | **merge M2** (barrier aggregates) |
| 3 tier-4b returns, 4a definers inversion, 4b interprocedural fold | inline | **merge M3** (still AST-consuming — §5 of the probe stands) |

Supporting moves:

* `collect_override_classes` went from a `&mut self` method to a free walker
  emitting `Vec<HarvestedOverrideClass>`; `collect_literal_constants` emits
  `Vec<HarvestedConstWrite>` (walk order, first-write value + **per-file write
  count**); `walk_fold_defs` emits `Vec<HarvestedFoldDef>` and lost its
  `ast_idx` parameter. `collect_fold_defs` split into that walker plus
  `invert_definers`.
* **`FoldSite` stayed a slice position and is built at merge.** `Harvest`
  carries the file-relative `HarvestedFoldDef` (owner, method, kind, tail
  `NodeId`, explicit-return flag); merge stamps the slice index on. The
  original `FoldSite` doc now says why.
* **`HarvestedConst`'s file id is stamped at merge** from the paired
  `&LoweredAst`, never carried in the harvest, with the persistence hazard
  (process-global counter) written into the type's docs — a persisted harvest
  must re-stamp it.
* Pass 2's `!classes.contains_key(name)` term is not reproduced: Pass 1 has
  already registered every source class and `register` is idempotent, so the
  skipped call was a no-op. The harvest also deduplicates repeated reads of the
  same name, for the same reason. Both are pinned by `register_is_idempotent`.
* CLI: the harvest runs inside stage 1's rayon closure; stage 2 is `merge`.
  `RIGOR_TIMING` labels became `stage1(parse+lower+harvest)` / `stage2(merge)`.
* Fixed the false doc claim at `infer_method_returns` ("a method never appears
  in BOTH maps"): true per def site, false per `(class, method)` key across a
  reopen. Pinned by `method_can_appear_in_both_return_maps`.

## The equivalence instrument

`probes_s92` is promoted from throwaway to a permanent harness:

* the **pre-#92 build path is kept verbatim** under `#[cfg(test)]`, with its
  own copies of the three walkers, as the oracle. `assert_paths_agree` compares
  `merge` against it on all 17 fields — canonicalised for content, and
  order-exact for `names` / `name_to_id` / `override_classes` (the three
  `Vec`-valued fields whose order comes from `HashMap` iteration are already
  unstable between processes on identical input, §3.4, so pinning their order
  would pin noise, not behaviour).
* fingerprint **extended** from 16 to 17 fields (`name_to_id` added, rendered
  with the ids, so id assignment is pinned in both modes).
* run over six corpora forward and reversed, all six permutations of the
  order-conflicting corpus, the single-file path, and the empty file list.
* probes 4, 5 and 8 were promoted from printouts into assertions
  (`coupling_pass3_reads_the_merged_constant_table`,
  `coupling_pass4b_degrade_is_cross_file`,
  `method_can_appear_in_both_return_maps`); probes 1 and 7 gained assertions
  (only `name_to_id`/`override_classes` may move under permutation; the ClassId
  → union-rendering channel is live and closed only by id assignment following
  file order). Probes 2, 3 and 6 stay as dumps — each documents a channel
  nothing else prints.

**The instrument was mutation-tested, not assumed.** Reversing M1's override
replay: 4 tests fail (both equivalence tests, both order-leak tests). Changing
the constant write count to a per-file bool: 4 tests fail (the equivalence
test, the intra-file duplicate test, two pre-existing harvest tests).

### Invariants pinned (spec §"Invariants to pin")

| # | pin |
|---|---|
| 1 | `order_leak_visibility_first_write_wins`, `order_leak_includes_accumulation_order` (index level) **and** `override_vis_project_order_is_normative`, `override_vis_project_include_order_is_normative` in rigor-rules (diagnostic level: `a,b` fires with the oracle's exact message, `b,a` silent) |
| 2 | `coupling_pass3_reads_the_merged_constant_table`, `coupling_pass4b_degrade_is_cross_file`, `coupling_pass2b_declaration_only_is_cross_file` |
| 3 | `constant_single_assignment_counts_intra_file_duplicates` (twice in one file / once in each of two / twice in one of two — all decline; a lone write still harvests) |
| 4 | `register_is_idempotent` |
| 5 | `harvested_const_file_id_is_the_assigning_file` + the persistence-hazard doc |
| 6 | `method_can_appear_in_both_return_maps` + the corrected doc |
| 7 | `merge_equals_legacy_build_project`, `merge_equals_legacy_under_every_permutation`, `merge_equals_legacy_for_a_single_file`, `merge_of_no_files_still_runs_the_barrier_passes`, `harvest_is_file_local` |

## Gates

| # | gate | verdict |
|---|---|---|
| 1 | `cargo test -p rigor-infer` / `cargo test --workspace` | **PASS** — 273 (was 262 pre-slice) / 1 105 total, 0 failed |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings`, fresh `CARGO_TARGET_DIR` | **PASS** — clean |
| 3 | `harness/run_snapshot.rb` | **PASS** — 407/443, 2 registered divergences, **0 unregistered extras** |
| 4 | `harness/fp_audit.py --gaps --sweep` | **PASS** — **0 FP / 9 204 files**, 820 gaps, gap set **byte-unchanged** vs the master-baseline binary |
| 5 | `rigor check` vs the master binary, mastodon/app | **PASS** — stdout + stderr + exit identical under default threads AND `RAYON_NUM_THREADS=1`; the two thread modes also agree with each other. Run beyond the spec on gitlab-foss/lib (1 094 lines) and dependabot-core (138 735 lines) too — identical there as well |
| 6 | `RIGOR_TIMING` before/after | **PASS** — no stage-1+stage-2 regression (table below) |

Gate 4 needed one measurement fix worth recording: `fp_audit` renders its
per-rule breakdown with `Counter.most_common()` over a Python **set**, so
equal-count rules tie-break in `PYTHONHASHSEED` order and two runs of the SAME
binary differ textually. Both sides were re-run with `PYTHONHASHSEED=0`; the
outputs are then byte-identical, all counts included. (Baseline side run with
`RIGOR_RS_BIN` pointed at the saved master binary.)

## Timing (release, 12 threads, warm; medians of 5 INTERLEAVED runs per side)

Interleaved because a straight before-then-after block put all the thermal
drift on one side — the first, non-interleaved pass disagreed with itself on
the absolute `stage2` numbers while agreeing on the direction.

| corpus | stage | before | after | Δ |
|---|---|---|---|---|
| mastodon/app (1 236 f) | stage 1 | 22.8 ms | 22.6 ms | −1 % |
| | **stage 2** | **35.3 ms** | **25.1 ms** | **−29 %** |
| | stage 1+2 | 62.6 ms | 47.7 ms | −24 % |
| gitlab-foss/lib (4 675 f) | stage 1 | 76.5 ms | 81.2 ms | +6 % |
| | **stage 2** | **196.8 ms** | **140.1 ms** | **−29 %** |
| | stage 1+2 | 273.1 ms | 237.7 ms | −13 % |

Stage 2 falls ~29 % on both corpora, under the probe's ~43 % decomposable
share: the harvest moves the WALKING into stage 1, but the merge still pays the
replay (every ordered field is cloned into the index, as `build_project` always
did). Stage 1 absorbs part of that at gitlab scale and is flat at mastodon
scale. Nothing regresses.

The Amdahl reading of the probe is unchanged: `compute_literal_returns` is
still 43–47 % of the merge, so the LSP keystroke path needs **#94** before the
layered index is worth building.

## Deviations from the spec

1. **Pass-2 harvest deduplicates repeated constant reads** (the spec says
   "`rbs_constant_names: Vec<String>` in AST order"). First-occurrence order is
   preserved and `register` is idempotent, so the result is identical; the
   dedupe just keeps a file that names `Time` fifty times from carrying fifty
   strings into the merge.
2. **The merge does NOT emit `definers` / `literal_constants` /
   `nested_constant_namespaces` in path order**, which §3.4 of the probe
   recommended to the spec author. The spec did not adopt it and it is a
   (provably inert) behaviour change, so it stays out of a pure-refactor slice.
   The equivalence harness compares those three canonicalised for the same
   reason. Worth doing in the persistence slice, which actually needs
   deterministic artifacts.
3. **The harvest sits outside stage 1's `catch_unwind`.** Inside it, a harvest
   panic would become a per-file internal-error diagnostic and silently drop
   the file from the index; outside, it propagates exactly as a `build_project`
   panic did. Matching today's behaviour beat extending panic isolation in a
   slice gated on bit-identity.
4. **`Stage1::Prepared`'s harvest is boxed** — `clippy::large_enum_variant`
   fires otherwise (432 vs 56 bytes). One allocation per file, in parallel.

## Not done (spec non-goals, unchanged)

AST eviction is still blocked (M3 walks the ASTs); the OverlayGuard does not
fall; no LSP change — `lsp.rs` calls the `build_project` wrapper exactly as
before; no persistence; no `literal_returns` work (#94).
