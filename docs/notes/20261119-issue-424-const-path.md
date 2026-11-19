# #424: iterative constant-path lowering

PR #651 → merged 2026-11-19 (`9cc20bc`). Refs #424 (the `A::A::…` row).
Follow-ups filed: #655–#658.

## What was slow

`A::A::…` was quadratic *in the builder*, before typing started: each of
the n `ConstantPathNode` levels ran `constant_path_string` and
`strict_constant_path_string` (O(depth) string-building walks) plus
`self_anchored_constant_path` and `rooted_constant_path` (O(depth) pointer
walks) — four O(depth) traversals per level. A `sample` of a 10k run put
~all time under `lower_expressions` in the two string helpers. Master:
10k 49.8–55.9 s; 20k >60 s (420.9 s measured under load).

## Port shape

`Builder::lower_constant_path` collects the chain once, lowers the
non-path base through `lower_node` as before, then pushes each level
inner→outer — the same arena order as the recursion — computing per
level:

- `name`: running lenient join (`k::A::B` → `A::B`; missing segment names
  render `A::` → `A::::B`, as before).
- `rooted` / `self_anchored`: uniform per chain — both answer on the
  leftmost segment.
- `dynamic_base`: prefix-monotone strict-name failure (non-constant base
  or a missing `cp.name()`), seeded from `strict_constant_path_string`
  on the base alone.

Skipped `lower_node` dispatches on intermediate levels are provably
inert: no other family claims a `ConstantPathNode` (the debug
`check_families_disjoint` assertion is the codification).

## Measured (release `check`, `harness/time_nesting.py --all`)

| shape | before | after |
|---|---|---|
| `A::A::…` 10k | 49.8–55.9 s | 0.37 s |
| `A::A::…` 20k | >60 s | 0.72 s |

Growth ×1.95–2.0 vs the ×2.5 / <2 s bar. Equivalence: a full lowered-arena
dump (every node + field + mutation census) is master-identical over 125
shapes (dynamic bases mid-chain, `self::`, `::A`, recovery `A::`/`A::::B`,
masgn targets, `defined?`, compound path writes, multi-byte, paths under
`def`/`rescue`/`ensure`/`case`); diagnostics byte-identical on the
pathological files, a 60-shape battery, net-ssh and gitlab-foss/lib.
Review: Opus Approved, Grok advisory Approved, Fable design companion no
blocker.

## Residuals (filed)

- **O(n²) name bytes stay** — each level owns its full prefix (~1 GB at
  the 25k depth bound; 70% of 24k time is `harvest` hashing them).
  Prefix-sharing + bounded `knows_*_class` lookups + a shared chain
  collector (deep class/module headers still take the recursive helpers):
  #657.
- **`self::X` FP + `::Object` shadow coverage loss**: #655.
- **Receiver-display message drift** (same key, folded values): #656.
- **`v.push(1)…` mutator chains**: 53 s at 2.5k — `CoreData::collect`
  hashing, a separate mechanism from the env-decline row: #658.
- Still on #424: `v+v+…` / `v ** v ** …` / `x = foo; x.a.a…` (local reads
  decline the `type_of` memo). Companion sketch: pin env epochs in the
  `ReachMemo` window rather than validating a static read set (the
  `h\x1f` synthetic keys would leak through a name list).
