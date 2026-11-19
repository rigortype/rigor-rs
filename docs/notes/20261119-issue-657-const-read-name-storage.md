# #657: ConstRead prefix-shared names; bounded `knows_*`; one chain walk

PR #660 (head `86a6e29`) — **Approved + fix-confirmation, READY, unmerged**:
merge waits on older PR #659 (overlaps `rbs.rs`/`reach_memo.rs`); then rebase
+ merge per the self-merge rules. Ledger line lands at merge.

## What was slow

Leftover from #651 (the #424 `A::A::…` row): every `ConstantRead` stored its
full dotted name per level (O(depth²) bytes — 197 MB @10k, ~750 MB @20k, ~1 GB
at the 25k nesting bound); harvest pass 2 SipHash'ed each through
`knows_class`/`knows_qualified_class` (~70% of `check` at 24k); a deep
`class A::A::…; end` header paid the recursive `constant_path_string` AND —
the dominant site, found by `sample` — `add_declared`'s join+insert of every
enclosing prefix into `HashSet<String>` (O(depth²) of each).

## Port shape

- `ConstName` (`rigor-parse::ast::const_name`): `Arc<str>` + byte len,
  `Deref`/`Borrow`/`Hash` consistent with `str`. Every level's lenient name is
  a byte-prefix of the outermost's, so `lower_constant_path` builds it once;
  ~45 consumer files compile unchanged through deref.
- `ConstPathChain` (`constants.rs`): one iterative collect (nodes outermost
  first + base); the lenient/strict renders and `rooted`/`self_anchored` flags
  and the lowering all share it.
- `CoreData`: `max_*_key_len` early-outs in the three `knows_*` (exact — a
  longer name cannot be present).
- `add_declared` → `DeclaredNames = HashSet<ConstName, FastHasher>`: the
  census names are prefixes of one join too; `contains(&str)` stays exact.
  Plus (design-companion follow-up, `86a6e29`) longest-first insert with
  early stop — the set is prefix-closed, so a hit means every shorter prefix
  is present: nested `module A`×d went O(d³)→O(d²) bytes hashed.

## Measured (release `check`, implementer's host)

| shape | before | after |
|---|---|---|
| `A::A::…` 20k | 0.74 s / 749 MB | 0.26 s / 44 MB |
| `A0::A1::…` 20k | 1.84 s / 1.6 GB | 0.27 s / 44 MB |
| `A::A::…` 25k | ~1 GB | 0.25 s / 36 MB |
| `class A::A::…; end` 20k | 1.25 s / 747 MB | 0.55 s / 40 MB |
| `class A0::…; end` 20k | 2.56 s / 1.6 GB | 0.94 s / 40 MB |

Gates: `check --format json` byte-identical to master on mastodon `app/` +
gitlab-foss `lib/` (1,200 files each; reviewer re-diffed 3,665 more); sweep
**0 FP**, gaps 360 unmoved; 19+14 probe rows matched (`ERB::Util.h` diverges
identically on master — pre-existing gap). Ordinary-code time/RSS flat.
Reviewer filed #661 (`(Object)::String.new.nope` — pre-existing FP, same
family as #655). Companion follow-ups: #662 (`loop_edges` `Subject::Const`
materialises a String per level — O(d³) on a deep path in a predicate, the
largest reachable residual: ~29 s at 10k), #663 (`sig-gen` prefix joins).
Nested-module residual after `86a6e29` (~31 s/1.2 GB at 4k, master 68.7 s) is
the per-level `Vec<String>` prefix clones — different mechanism, unfixed.
