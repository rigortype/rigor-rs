# AST eviction — the `f` measurement (2026-08-26)

The one number [the AST-eviction probe](20260826-ast-eviction-probe.md) named
as its STOP condition (§5.1), taken. **A structural walk, not a timing
measurement** — no benchmark, no `Instant`, no claim about wall clock
anywhere in this note.

**Instrument**: `#[cfg(test)] mod probes_ast_eviction_f` at the end of
`crates/rigor-infer/src/source_index.rs` (`:5267-5629`), one `#[ignore]`d
test. Reproduce with:

```sh
cargo test --release -p rigor-infer --lib -- --ignored --nocapture \
  measure_pass3_capture_fraction
```

`#[ignore]`d because it walks real external OSS checkouts at machine-local
absolute paths — same convention `harness/sweep-corpora.yml` already uses
(absent ⇒ SKIPPED LOUDLY, never silently) — and would otherwise slow down
every `cargo test --workspace`. It is not part of the default run; `cargo
test --workspace` passes with it present and skipped (§6).

---

## 0. Verdict up front

| corpus | files | `f` node-weighted aggregate | median | p90 |
|---|---|---|---|---|
| conference-app | 245 | **0.104** | 0.000 | 0.750 |
| mastodon/app | 1,236 | **0.279** | 0.302 | 0.706 |
| gitlab-foss/lib/gitlab (subtree) | 3,117 | **0.257** | 0.225 | 0.658 |
| gitlab-foss/lib (full) | 4,675 | **0.204** | 0.180 | 0.624 |
| **combined, distinct files** (conference-app + mastodon/app + gitlab-foss/lib) | 6,156 | **0.214** | — | — |

Every corpus's node-weighted aggregate sits **well under the 0.40 close
threshold** — the highest is mastodon/app at 0.279, barely above the
0.15–0.40 band's midpoint. The measurement is a **deliberate
over-approximation** (§2): both approximations only ever inflate `f`, so the
true fraction is this number or smaller, never larger.

**Verdict: ORCHESTRATOR'S CALL, formally — but the evidence leans clearly
toward SPECIFY IT.** The combined node-weighted aggregate (0.214) sits in the
probe's defined "in between" band, not below the 0.15 line, so this is not a
clean SPECIFY read by the letter of the threshold. But it sits only 26% of
the way from 0.15 toward 0.40; three of four corpora cluster at 0.20–0.28;
the largest corpus at the probe's own canonical LSP-scale reference point
(gitlab-foss/lib, 4,675 files) is 0.204; and the measurement over-counts by
construction. Nothing in this data makes a case for CLOSE THE LINE.

---

## 1. Method

Per [the probe](20260826-ast-eviction-probe.md)'s §5.1 formula:

```
retained = ⋃ over ClassDef/ModuleDef · MethodBody passing gates 2/3/4
             of subtree(mb.body.last())
         ∪ { Definition nodes' name slots }
f = |retained| / ast.len()
```

**Gates 2/3/4, reused verbatim** from `source_index.rs`'s real `infer_one_return`
(`:2106-2136`) and `is_branch_carrier` (`:2609-2619`) — not reimplemented:

* gate 3 — `mb.has_explicit_return` ⇒ decline;
* gate 2 — `mb.body.last()` (`None` ⇒ decline);
* gate 4 — `is_branch_carrier(ast.get(ret_id))` (an `If`/`Case`/`When`/`Loop`/
  `Logical`/`BeginRescue` tail ROOT) ⇒ decline.

A method passing all three is exactly a method whose tail **reaches** gate 5
(`Typer::type_of`, `lib.rs:403`) — the walk doesn't call `type_of` (it never
reimplements inference, per the task and the memory note
`subset-arguments-need-probing`); it marks the tail's full reachable subtree
instead, which is a safe over-approximation of what `type_of` actually reads
(§2, approximation 1).

**Subtree reachability**: `children(node: &Node) -> Vec<NodeId>`
(`source_index.rs:5288-5343`) is an EXHAUSTIVE match over all 37 `Node`
variants (no `_` arm, mirroring `Node::span()`'s own exhaustive match in
`rigor-parse/src/ast.rs:769-812`) — a future 38th variant fails this to
compile rather than silently under-counting. `mark_subtree` is a DFS over
those edges from each surviving tail, deduped into one `HashSet<NodeId>` per
file (so a node reachable from two different method tails is counted once —
the right semantics for an ARENA-fraction question, not a per-method one).

The walk deliberately does **not** follow "orphaned" nodes — a receiver-bearing
`def`'s receiver expression, a parameter default value, a `Range`'s bounds, a
namespaced `ConstantRead`'s parent scope. `ast.rs` lowers those into the arena
purely so the flat `ast.iter()` call walk finds them (`ast.rs:1321-1345` for
the def-receiver/default-value case) but stores the resulting `NodeId` nowhere
— `let _ = self.lower_node(&recv);`. A pointer-following walk from a root
structurally cannot reach them, which is not an approximation but an exact
match to the probe's §1.2 "subtree-confined" finding.

The `Definition`-node term is added by a second, separate scan
(`retained_tail` is recorded before it, `retained_full` after) — see
approximation 2 below.

### Corpora

`harness/sweep-corpora.yml`'s mastodon/app (1,236 files) and one gitlab-foss
subtree — `lib/gitlab`, which turns out to be **exactly** 3,118 files (3,117
measured after 1 skip), matching the probe's own "3,117" interpolated scale
point in its memory table (§3.1) — plus the full `gitlab-foss/lib` (4,676
files, 4,675 measured), the probe's other canonical scale point, and
conference-app (245 files) for a small, non-Rails-scale real-app data point.
All four are real, absolute, machine-local checkouts; none was modified.

A file is skipped (excluded from both numerator and denominator, exactly
mirroring `check`'s own stage-1 skip) when it fails to read as UTF-8, looks
like an ERB template (`rigor_parse::looks_like_erb_template`), or Prism
reports a parse error — 1 file in each gitlab-foss corpus, 0 elsewhere.

### Approximations (both bias toward a LARGER `f`, the safe direction)

1. **Full-subtree marking, not `type_of`'s actual read set.** `type_of`
   resolves a branch's value through its TAIL statement only —
   `branch_value_type` / `stmt_value_type` (`lib.rs:659-722`) recurse into
   `body.last()`, never into the earlier statements of an `if`/`case`/`begin`
   branch. Those earlier statements' subtrees are read by no `type_of` call at
   all (only by the separate, whole-file `ast.iter()` rule walks, which need
   the file for unrelated reasons). Marking the WHOLE subtree therefore
   retains nodes `type_of` never touches — an over-count with no known upper
   bound on its size, since it depends on how many multi-statement branches
   sit inside surviving tails.
2. **Every `Definition` node marked unconditionally**, not only in files where
   `file_defines_method` (`lib.rs:1522-1525`) actually fires — gated behind a
   10-name membership test (`lib.rs:1336-1343`: `p|pp|format|sprintf|String|
   Hash|Integer|Float|Array|rand`) AND a qualifying gate-5 tail in the SAME
   file. `retained_tail` / `f_tail_agg` (reported alongside `f_full`) is the
   identical union WITHOUT this term, so its effect is visible directly
   (§3) — it inflates `f_full` over `f_tail` by roughly 4–8 percentage points
   depending on corpus.

No other approximation was made: gates 2/3/4 and the subtree edges are the
real code / the real grammar, not a reimplementation.

---

## 2. Results

| corpus | files (skipped) | total nodes | retained (full) | retained (tail-only) | f_full median | f_full p90 | **f_full agg** | f_tail agg | zero-tail-frac |
|---|---|---|---|---|---|---|---|---|---|
| conference-app | 245 (0) | 16,200 | 1,685 | 1,449 | 0.0000 | 0.7500 | **0.1040** | 0.0894 | 0.6694 |
| mastodon/app | 1,236 (0) | 130,096 | 36,317 | 29,620 | 0.3016 | 0.7059 | **0.2792** | 0.2277 | 0.2006 |
| gitlab-foss/lib/gitlab (subtree) | 3,117 (1) | 363,143 | 93,175 | 74,424 | 0.2250 | 0.6579 | **0.2566** | 0.2049 | 0.2611 |
| gitlab-foss/lib (full) | 4,675 (1) | 609,172 | 123,993 | 98,675 | 0.1802 | 0.6239 | **0.2035** | 0.1620 | 0.3435 |
| **combined, distinct files** | 6,156 (2) | 755,468 | 161,995 | 129,744 | — | — | **0.2144** | 0.1717 | 0.3278 |

"combined, distinct files" is conference-app + mastodon/app + gitlab-foss/lib
(full) — the gitlab-foss subtree is a strict subset of the full `lib`
directory, so it is excluded from this row to avoid double-counting the same
files; it is kept as its own row above because it is a useful independent data
point (it isolates one large, cohesive namespace from the noisier full tree).

"f=0 file fraction" — the task's literal question, "no surviving tails at all,
those files' arenas could be dropped entirely" — is the **zero-tail-frac**
column (`retained_tail == 0`), NOT a `retained_full == 0` count. Approximation
2 means a file can have `f_full > 0` purely from its `Definition` nodes while
having zero surviving tails; `retained_tail` is the precise "nothing Pass 3
reaches" signal. By that measure, **20–34% of files per corpus** (33% across
the distinct-file combination) have no surviving tail at all — their entire
arena is, under the real `file_defines_method` gating (not this walk's
blanket over-approximation), droppable.

---

## 3. What the median/p90/aggregate spread shows

The task flagged that histograms mask over-claims (`coverage-arc-and-node-
audit-lesson`) — reported in full here, not just the aggregate:

* **conference-app's median is exactly 0.0**: a majority of its 245 files (a
  full small-repo checkout, not scoped to `app/` — includes specs, config,
  migrations) have zero `Definition` nodes and zero surviving tails.
  Its aggregate (0.104) is pulled up entirely by a p90 tail at 0.75 — a
  minority of files that are almost nothing BUT a handful of simple methods.
* **The aggregate is not simply "below the median" or "above the median" in
  one direction.** mastodon/app's aggregate (0.279) sits slightly BELOW its
  median (0.302) — its larger files retain a bit LESS of their arena than a
  typical file. gitlab-foss/lib (full)'s aggregate (0.204) sits slightly
  ABOVE its median (0.180) — the opposite relationship. Neither corpus
  supports a simple "big files have low/high f" story; the aggregate has to
  be measured, not inferred from the median.
* **p90 is uniformly ~0.62–0.75 across every corpus** — a real minority of
  files (utility modules, value objects, files that are mostly trivial
  accessor-shaped methods) retain most of their arena regardless of corpus
  shape. These are not the files an eviction line would most want to keep
  fully loaded (they're already small), but they are the files where a
  capture buys the least.

---

## 4. Gates run

| gate | verdict |
|---|---|
| `cargo test -p rigor-infer --lib` | **PASS** — 279 passed, 0 failed, 1 ignored (the new probe) |
| `cargo test --workspace` | **PASS** — every crate green, the new test correctly skipped by default |
| `cargo clippy -p rigor-infer --all-targets -- -D warnings` | **PASS** — clean |
| `cargo clippy --workspace --locked -- -D warnings` | **PASS** — clean (the CI-blocking gate) |
| `python3 harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| `crates/` touched outside the `#[cfg(test)]` module | **none** — no production behaviour change |
| `reference/rigor` / `REFERENCE_RIGOR_DIR` touched | **no** |

---

## 5. What this note does not claim

* **No timing was taken, anywhere.** This is a structural reachability count
  over already-built ASTs; the instrument happens to run in about a second
  for ~9,000 files in `--release`, but that number is not reported as a
  finding and should not be read as one.
* **`f_full` is a genuine over-estimate**, not a point estimate with unknown
  bias direction. Both named approximations (§1) can only add nodes to
  `retained`, never remove them, so the true `f` a real capture would need is
  this number or smaller.
* **The "combined, distinct files" row is a simple node-weighted pool across
  three corpora**, not a stratified or otherwise-weighted average — a bigger
  corpus (gitlab-foss/lib) dominates it proportionally to its node count,
  which is the deliberate point (memory is what the eviction question is
  about) but means it is not "the average corpus's `f`".
* **`retained_tail == 0` is the precise "arena droppable" signal only under
  today's `file_defines_method` gating.** A future slice that widens that
  gate (more magic names, or removing the gate) would change which files
  qualify; this note measures today's code, not a hypothetical.
* No `reference/rigor` work, no `REFERENCE_RIGOR_DIR`, no production code
  changed — the entire change is one `#[cfg(test)]` `#[ignore]`d module.
