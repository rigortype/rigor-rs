# AST eviction — probe (2026-08-26)

The frozen-index arc's last open lever, and the fact two separate items are
blocked on: **merge M3 and stage 3 both still require every `LoweredAst`**
([#92 §5](20260825-s92-buildproject-pass-inventory.md),
[#104 §0](20260826-s104-harvest-cache-probe.md)). The LSP holds every project
AST (~44–53 KB/file, the dominant term), and
[#104's NO-GO](20260826-s104-harvest-cost-measured.md) names "the AST stops
being required by merge M3 **and** by stage 3's per-file rule walk" as its sole
reopening condition.

**Investigation only.** No production code was touched, no timing was taken (a
sibling agent is measuring), no `reference/rigor` / `REFERENCE_RIGOR_DIR` work.
Every size figure below is quoted from an already-recorded measurement or
computed from a field list, and is labelled as such.

---

## 0. Verdict up front

| question | answer |
|---|---|
| Is Pass 4b's AST read a bounded, capturable slice? | **YES** — a closed 10-of-37-variant grammar with a depth-16 cap, and a harvest-time prune that is exact by construction |
| Is Pass 3's AST read a bounded, capturable slice? | **Bounded, but not small** — subtree-confined (a real finding, §1.2) yet it reaches 30 of the 37 `Node` variants, so the capture is an AST sub-arena, not a summary |
| (a) Can merge stop needing OTHER files' ASTs? | **YES, and cheaply** — only Pass 4b descends cross-file, and that is the capturable half |
| (b) Is stage 3's need per-file-local? | **YES** — `analyze` reads exactly `p.ast` + `p.source` + the frozen project index; no other file's tree |
| ⇒ Can the LSP evict held ASTs while `check` still parses each file once? | **Only if Pass 3's inputs are ALSO captured.** (a)+(b) alone are not enough: Pass 3 reads every file's OWN tree at merge time |
| Does #104 reopen? | **NO — illusory.** Even an AST-free M3 leaves `check` paying read+parse+lower for stage 3, so #104's prize stays 4.1–4.7 ms |
| Prize for the LSP at 4,675 files | **~207–241 MB of a measured 428.3 MB steady state** (§3) — real, but headroom, not a violated budget |
| **Verdict** | **NEEDS-A-NARROWER-SLICE, gated on one measurement that has not been taken** (§5) |

The single number that decides the whole question is **the fraction of a file's
arena that lies under a surviving method tail**, and it has never been measured.
Below ~15 % the LSP win is most of 241 MB; above ~40 % the capture *is* the AST
and the line is dead. §5 makes taking that number the STOP condition.

The cheapest real win found here is **not** AST eviction at all: the per-URI
cross-file index cache costs a **measured 134.4 MB at 4,675 files** for its 8
entries ([the cache impl note](20260826-lsp-crossfile-cache-impl.md) §memory),
and its cap is a constant with no parity surface (§5.3).

---

## 1. What merge M3 actually reads, pass by pass

M3 is `source_index.rs:964-997`. It materialises `asts` at `:966` and hands the
slice to exactly two consumers: `infer_method_returns` (`:973`) and
`compute_literal_returns` (`:997`).

**Outside M3, the merge touches an AST exactly once**: `ast.file_key().clone()`
in M2's C5b constant gate (`:904`, `:910`). Since #102/#103 made that a
path-derived value, it is a `String`-sized field a harvest could carry
verbatim — the merge's only non-M3 AST dependence is one line and is free to
remove. Worth stating because it means the M3 characterisation below is the
*whole* problem, not most of it.

### 1.1 Pass 4b (`compute_literal_returns`) — a closed grammar, and it is capturable

`compute_literal_returns` (`:1563-1578`) → `resolve_fold_key` (`:1584-1608`) →
`fold_key_sites` (`:1613-1638`) → `fold_expr` (`:1647-1730`). The AST enters at
exactly one place: `fold_key_sites:1628`, `let ast = asts[site.ast_idx];`, then
`fold_expr(ast, site.tail, …)`.

**`fold_expr` inspects exactly these node shapes** (`:1663-1729`), and nothing
else — every other variant hits `_ => None` at `:1728` and declines the whole
fold:

| shape | what is read | what it does |
|---|---|---|
| `StringLit{value}` / `IntegerLit{value}` / `FloatLit{value}` / `SymbolLit{value}` | the literal payload | → `Scalar` |
| `NilLit` / `TrueLit` / `FalseLit` | nothing but the tag | → `Scalar` |
| `Call{receiver: None, method, block_body}` **and `block_body.is_empty()`** | `method`, block-emptiness | implicit-self ⇒ `resolve_instance_owner` then `resolve_fold_key` (`:1674-1683`) |
| `Call{receiver: Some(r), method, args, block_body}` **and `block_body.is_empty()`** | `method`, `args.len()`, `r` | three sub-cases below |
| — `method == "!" && args.is_empty()` | — | recurse on `r` at `depth+1`, invert truthiness (`:1691-1697`) |
| — `args.is_empty()` and `ast.get(r)` is `ConstantRead{name}`, `name` non-empty | the receiver's `name` | `Const.method` singleton ⇒ `resolve_fold_key` (`:1699-1713`) |
| — otherwise | — | fold `r` and every `a` in `args` at `depth+1`, then `folding::fold` (`:1716-1726`) |
| anything else (incl. any `Call` **with** a block) | the tag | `None` — declines the whole method |

That is **10 of the 37 `Node` variants** (`ast.rs:296`), `ConstantRead`
reachable only as a receiver. Depth is capped at `FOLD_DEPTH_CAP = 16`
(`:113`, checked at `:1660`), and — decisively — **depth resets to 0 at every
new key** (`fold_key_sites:1630` passes `0`), so the reachable depth from any
one site is exactly `0..=16`.

**The cross-file descent is real but indirect.** `fold_expr` never walks another
file's tree directly; it recurses through `resolve_fold_key` into the `defs`
map, which is built at `:984-995` from the harvests. So the descent is
"harvest → harvest", with `asts[site.ast_idx]` used purely to re-hydrate a
`NodeId` into a `Node`. If a `HarvestedFoldDef` (`:269-275`) carried an **owned
mini-tree** instead of a bare `tail: NodeId`, `fold_expr` would take no `&[&LoweredAst]`
at all — and `FoldSite::ast_idx` (`:131-141`), the slice-position hazard
#92 §5 flagged as obstacle 1, would disappear with it.

**And the capture prunes exactly, at harvest time.** Every decline in the table
above is decided by *syntax alone*: the shape tag, `args.is_empty()`,
`block_body.is_empty()`, `name.is_empty()`. None of them consults merged state.
So a harvest can walk the tail once and emit either a resolved `Scalar`, an
unresolved call reference (`method` / `(Const, method)` + captured children), or
a one-byte `Decline` — and a `Decline` is provably identical to what `fold_expr`
would have returned, without any subset argument. Since `walk_fold_defs`
(`:2534-2585`) harvests **every direct `def` in every class/module body**, and
the overwhelming majority of Ruby method tails are ivar reads, block-bearing
calls, `if`/`case` carriers or local reads — all `_ => None` — the capture for
most methods is that one byte.

**Verdict: Pass 4b's AST read is a bounded, characterizable slice that a
per-file harvest can capture.**

### 1.2 Pass 3 (tier-4b returns) — bounded in an unexpected way, but not small

`infer_method_returns` (`:2034-2100`) builds `Typer::with_source(core, idx)`
over the *complete* index, then, per file:

1. `ast.iter()` for `ClassDef{name, method_bodies}` / `ModuleDef{…}`
   (`:2054-2064`) — reading the per-class `Vec<MethodBody>` (`ast.rs:61-68`:
   `name`, `body: Vec<NodeId>`, `has_explicit_return`, `params`);
2. per `MethodBody`, `infer_one_return(ast, &typer, core, &empty_env, mb)`
   (`:2106-2136`): `mb.has_explicit_return` (gate 3), `mb.body.last()` (gate 2),
   `is_branch_carrier(ast.get(ret_id))` (gate 4, `:2609-2619`), then
   **`typer.type_of(ast, ret_id, empty_env, &mut scratch)`** (`:2129`);
3. on a decline, `infer_one_param_bound(ast, mb)` (`:2162-2201`), which walks
   `ast.get(cursor)` down `Call{receiver}` chains to a `LocalVariableRead` root.

Gates 2/3/4 are pure syntax and already harvest-time knowable. Gate 5 is
`type_of`, and that is the question.

**The good finding — `type_of`'s AST reads are SUBTREE-CONFINED.** Audited
mechanically over the whole `Typer` cluster (`lib.rs:403-1981`, which is the
transitive closure: every AST-taking call inside it is a `self.` method also
inside it):

* **no `ast.root()` anywhere** in the cluster;
* **exactly one whole-file scan**: `file_defines_method` (`lib.rs:1522-1525`),
  `ast.iter().any(|(_, n)| matches!(n, Node::Definition { name: Some(m), .. } if m == name))`
  — and it is gated behind a 10-name membership test (`lib.rs:1336-1343`:
  `p|pp|format|sprintf|String|Hash|Integer|Float|Array|rand`), so it fires only
  when the tail is an implicit-self call to one of those;
* `ast.file_key()` at `lib.rs:511` and `:520` (the C5 per-file constant gate);
* `enclosing_prefix(span)` (`lib.rs:365-378`) reads `self.lexical_scopes`, which
  for a `Typer::with_source` is `EMPTY_LEXICAL_SCOPES` (`lib.rs:299-301`) — so
  in Pass 3 it is a constant `&[]`. **Inert today, by one line.** A future slice
  that attaches per-file scopes to the Pass-3 typer would widen the capture to
  the file's whole lexical-scope table; the #104 probe flagged the same
  "exact-because-of-one-line" shape for `core` and the same caution applies.

So the Pass-3 capture per file is exactly:
`{ ClassDef/ModuleDef (name, method_bodies) } ∪ { subtree(tail) for each MethodBody passing gates 2/3/4 } ∪ { Definition names } ∪ { file_key }`.

**The bad finding — the subtree is the general expression grammar.** Counting
the variants matched by name across the cluster: 17 in `type_of` itself
(`lib.rs:404-661`), 8 more in `stmt_value_type` (`:692-722`:
`Statements`/`BeginRescue`/`When` and the five write carriers), plus `Other`
(`:1360`), `Return` (`:571`), `SelfExpr` (`:1592`), `Range` (`fold_tuple_projection`)
and `Definition` (`:1524`) — **30 of the 37 variants**. Only `Program`,
`ClassDef`, `ModuleDef`, `Loop`, `Lambda`, `Logical` and `VariableRead` are never
matched by name, and they still reach the `_ => untyped` arm, which is a real
answer the capture must reproduce.

Two consequences:

* **There is no closed-grammar prune for Pass 3.** Any "these shapes always type
  Dynamic, so drop them" rule is a subset argument over a 9,677-line inference
  module that every future slice re-opens. That is precisely the shape the memory
  note `subset-arguments-need-probing` records failing three times in one arc.
* **The honest capture is a sub-arena, not a summary** — keep the nodes, keep
  `Span`, keep `file_key`, and run the unmodified `type_of` over it. Which makes
  the parity story much better (no re-implementation) and the size story much
  worse (§3).

Gate 4 does *not* bound the grammar. It rejects a branch carrier only at the
tail ROOT; an `If` nested in a call argument or an array element is still typed
(`lib.rs:606-625`).

### 1.3 Summary table

| | Pass 3 | Pass 4b |
|---|---|---|
| entry | `infer_method_returns(&idx, core, &asts)` `:973` | `compute_literal_returns(&asts, &defs)` `:997` |
| descends into OTHER files' trees | **no** | **yes** (`fold_key_sites:1628`) |
| node shapes reached | 30 of 37 | **10 of 37** |
| depth bound | none | **16** (`:113`, `:1660`) |
| harvest-time prune | gates 2/3/4 only; gate 5 needs the merged index | **exact and total** — every decline is syntactic |
| capturable? | as an AST sub-arena | **as an owned mini-tree** |

---

## 2. The (a)/(b) separation

### (a) Can merge stop needing the ASTs of files it is not analysing? — YES, and it is the easy half

`infer_method_returns` is a `for ast in asts` loop whose body only ever touches
that same `ast` (`:2054-2098`), and `type_of` is subtree-confined (§1.2). **Pass
3 never reads another file's tree.** Its cross-file dependence is entirely
through the merged `idx` the typer borrows — which is why the #92 probe's
`MAX = 5` / `MAX = 6` case flips a Pass-3 answer for a byte-identical `a.rb`.

`fold_expr` is the only cross-file AST descent in the whole merge, and §1.1
shows it is the capturable one.

So: capture Pass 4b's fold inputs into `Harvest`, move `file_key` in with them,
and **the merge stops needing any file's tree except each file's own for Pass 3.**

### (b) Is stage 3's need per-file-local? — YES

`check`'s stage 3 (`main.rs:898-925`) is a `par_iter` over `prepared` whose
closure reads exactly `p.ast`, `p.source` and the shared frozen `index` /
`project_source`:

* `analyze_with_source_and_folder(&p.ast, …)` `main.rs:903`
* `shadowed_rescue_diagnostics(&p.ast, …, &p.source)` `:910`
* `void_value_use_diagnostics(&p.ast, …)` `:917`
* `line_col(&p.source, …)` `:934`

No other file's tree, no other file's text. **An AST could be dropped the
instant its own analysis finishes.**

### The conclusion this forces, and it is not the comfortable one

(a) + (b) are both favourable and **still do not unblock eviction**, because the
two are not the whole condition. Pass 3 needs *each file's own* tree **at merge
time** — i.e. after every harvest is in and before any analysis runs. In the
LSP, the merge is over the whole project on every dispatch, so "each file's own
tree" is "every held file's tree". The held table cannot shrink while Pass 3
reads trees, no matter how local Pass 3 is.

Restated as the precise blocking fact, which is narrower and more useful than
"#92 §5 blocks eviction":

> **Merge M3 needs no file's tree except for `Typer::type_of` over the tail
> expression of every method that passes gates 2/3/4, plus that file's
> `Definition` name set and its `FileKey`.**

Everything else in M3 is capturable today.

---

## 3. Sizing the prize, per consumer

All figures are quoted from recorded measurements. Two independent per-file AST
figures exist and they disagree by ~20 %; both bands are shown rather than one
picked.

| source | method | 1,236 | 3,117 | 4,675 |
|---|---|---|---|---|
| [baseline note §4](20260825-frozen-index-baseline-measurements.md) | RSS delta, same-root baseline | 46.6 KB/f | 50.1 KB/f | 52.9 KB/f |
| [held-harvest note, phase 0](20260825-lsp-held-harvest-impl.md) | in-process, pre-`SourceIndex` | 36.6 KB/f | — | 44.2 KB/f |
| held-harvest note, phase 0 | held `Harvest` | 3.68 KB/f | — | 4.04 KB/f |
| [cross-file cache note](20260826-lsp-crossfile-cache-impl.md) | per cached `SourceIndex` | 3.64 KB/f/entry | — | 3.68 KB/f/entry |

### 3.1 The LSP — the only consumer with a real prize

The steady state is **not** the baseline note's 262.6 MB. The cross-file cache
note measured, in one session at 4,675 files, `ps -o rss=` of **290.7 MB before
the cache fills and 428.3 MB at its 8-entry cap**. Decomposing 428.3 MB with the
recorded per-file rates:

| term | 4,675 files | share |
|---|---|---|
| held `LoweredAst`s | **206.6 – 241.3 MB** | 48 – 56 % |
| 8 cached `SourceIndex` entries | **134.4 MB** (measured) | 31 % |
| held `Harvest`s | 18.9 MB | 4 % |
| `CoreIndex` + runtime baseline | ~21.3 MB (measured, 0-file root) | 5 % |
| unattributed remainder | ~13 – 47 MB | — |

(The decomposition closes: 21.3 + 241.3 + 18.9 = 281.5 MB against a measured
290.7 MB pre-cache, which also says the *higher* AST band is the right one at
this scale.)

**Steady state if held ASTs became held harvests-plus-capture**, where `f` is
the fraction of a file's arena the Pass-3 + Pass-4b capture retains:

| files | today (8 cache entries) | f = 0 (floor) | f = 0.15 | f = 0.40 |
|---|---|---|---|---|
| 1,236 | **119.4 MB** (measured) | ~62 – 74 MB | ~70 – 81 MB | ~85 – 92 MB |
| 3,117 | ~251 – 281 MB (interpolated) | ~124 MB | ~143 – 148 MB | ~175 – 187 MB |
| 4,675 | **428.3 MB** (measured) | ~187 – 222 MB | ~223 – 253 MB | ~284 – 304 MB |

Against ADR-0029's `< 600 MB @ 5 K files`: today is **~71 % of budget**, the
floor is ~31–37 %, and `f = 0.40` is ~47–51 %. So the prize is **headroom, not a
violated budget** — which is a materially weaker driver than the OverlayGuard
cliff was, since that was a live functionality gap.

Two things a reader should not miss:

* **The 4,675-file held table is a NEW steady-state cost.** Before the
  held-harvest slice the guard tripped OFF at that scale and `swap_project(ctx,
  st, index, None)` (`lsp.rs:2620`) dropped the whole held table — the memory
  came back precisely because the feature turned itself off. The slice that
  fixed the functionality gap is the same slice that made 241 MB permanent.
* **`f` is unmeasured and it is the entire question.** The one structural fact
  that bounds it: `Node` is a fixed-size enum sized by its widest variant —
  `Definition` (`ast.rs:440-500`) computes to ≈240 B from its field list, and
  `LoweredAst.nodes` is a plain `Vec<Node>` (`ast.rs:871-881`), so **every
  arena slot pays the full width regardless of variant**. Blanking a node to
  `Node::Other` therefore frees only its heap tail; a capture that actually
  saves memory must **compact and renumber**, which is what puts
  `HarvestedFoldDef::tail` and `MethodBody::body` in the blast radius.
  (Cross-check, not a measurement: 44.2 KB/f ÷ 240 B ⇒ on the order of 150–180
  slots per file at gitlab-foss/lib scale.)

### 3.2 `check` — no prize at all, and #104's reopening is illusory

`check` holds every `Prepared` (source + AST + comments) through stage 3 by
construction, and its peak RSS has never been anyone's complaint. AST eviction
buys it nothing.

**Does #104 reopen?** Its condition is "M3 **and** stage 3 stop requiring the
AST". Taking the halves separately:

* **M3 alone: no.** With M3 AST-free, `check` still does `read → parse → lower`
  for every file because stage 3 needs it (§2b). The skippable unit stays
  `SourceIndex::harvest` alone, whose measured marginal wall is **4.1–4.7 ms at
  4,675 files** against ~48 ms of cache reads. The arithmetic in
  [the cost note §4.4](20260826-s104-harvest-cost-measured.md) is unchanged, to
  the millisecond. **#104 stays NO-GO.**
* **Stage 3 too: not by this route.** Removing the AST from stage 3 means a
  per-file *diagnostics* cache — cross-file-dependent (a change in file B moves
  file A's diagnostics), which is Design-B-shaped and which #93 already declared
  NO-GO on its own merits.
* **The tempting restructure does not work either.** "Parse → harvest+capture →
  drop the AST → merge → re-parse for stage 3" pays `parse+lower` twice (~82 ms
  wall at 4,675) to save peak RSS in a batch process. Strictly worse.

**So: reopening #104 is illusory.** State it plainly in any future ledger line —
the reopening clause reads as though an AST-free M3 would suffice, and it would
not.

---

## 4. The cost

### 4.1 Harvest size

* **Pass 4b capture: small, and bounded by construction.** One entry per direct
  `def`; the overwhelming majority collapse to a `Decline` tag at harvest time
  (§1.1). The depth cap (16) bounds the worst case per site. Plausibly a few
  hundred bytes per file against the recorded ~4.04 KB — but this is an
  estimate, not a number, and the implementation slice should report it.
* **Pass 3 capture: it IS the sizing question.** `f × AST` (§3.1). A `Harvest`
  is 312 B of struct + ~4 KB of payload today; a Pass-3 sub-arena at
  `f = 0.15` would roughly **double** it, at `f = 0.40` roughly **quintuple** it.
  A harvest that large also re-prices #104's cache read — which is already 10×
  its prize — so it can only make that verdict more negative.

### 4.2 The bit-identity obligation — this is a parity claim, not a refactor

`check` is byte-deterministic (ADR-0020) and the arc's standing bar is
byte-identical stdout **and** stderr against a master-built baseline. A capture
slice must reproduce, exactly:

1. **Every decline.** A capture that loses a shape turns a decline into a
   different decline (harmless) *or* an emission (an FP). The `_ => None` /
   `_ => untyped` arms are the load-bearing ones and they are the easiest to
   drop.
2. **`ClassId` assignment order.** Untouched by a capture — but `names` /
   `name_to_id` order reaches rendered union member order (#92 §3.3), and the
   cross-file hover/completion consumer has since made that channel reachable.
   Any renumbering must not perturb registration order.
3. **A landmine, named explicitly.** If a capture blanks unreached nodes rather
   than compacting, the hole filler **must not be `Node::Other` or
   `Node::Statements`**. Both are load-bearing sentinels: `type_implicit_self_call`
   treats an `Other`/`Statements` argument as a splat/forwarding arg and
   declines (`lib.rs:1358-1366`), and the `ArrayLit` arm degrades a `Tuple` to a
   bare `Array` nominal when any element is `Statements | Other | Return`
   (`lib.rs:568-573`). A blanked sibling would be indistinguishable from a real
   splat and would silently change a type — and it would change it in the
   *quiet* direction, so no fixture would notice.
4. **`FOLD_DEPTH_CAP` boundary fidelity.** #94's cap-boundary lesson applies
   verbatim: WHICH nodes are inside a cap is order- and depth-sensitive, and a
   cap's only coverage is a purpose-built test. #94's probe (§2) counted **0**
   hits on the analogous `OVERRIDE_ANCESTOR_WALK_LIMIT` across 12 corpus runs;
   `FOLD_DEPTH_CAP` hits have never been counted at all, so assume the corpora
   give it none.
5. **`Span` values.** Inert in Pass 3 today only because `with_source` supplies
   empty lexical scopes (§1.2). Spans must survive the capture regardless, or
   the inertness becomes a trap for the next inference slice.
6. **`FileKey`.** Read at `lib.rs:511`/`:520`; already path-derived (#102/#103),
   so this is a field move, not a hazard — provided the "never serialize an
   `Anonymous`" rule from the #104 probe §1.3 travels with it.

### 4.3 The subset-argument discipline

The memory note `subset-arguments-need-probing` records "we do strictly less
than X" failing **three times in one arc** — wrong axis, wrong carrier,
inverted. A capture slice is exactly that shape ("the capture is a subset of
what the typer reads, so it is safe"), and the safe form is the one this arc has
now used successfully three times:

* an **allow-list by construction**, never a deny-list — Pass 4b's grammar
  enumerates what is kept; Pass 3's sub-arena keeps whole subtrees rather than
  filtering by shape;
* a `#[cfg(test)]` **legacy oracle** — the pre-slice `infer_method_returns` /
  `compute_literal_returns` kept VERBATIM, in the `build_project_legacy` /
  `related_to_owner`-oracle / `overlay_source_index_legacy` pattern;
* **mutation testing of the oracle**, reported as a table of "mutation → tests
  that failed". The held-harvest slice's finding is the precedent that matters:
  its first equivalence test *passed* the stale-harvest mutation, and only a
  case whose own diagnostics depended on the captured fact could tell the two
  apart.

### 4.4 The parity evidence such a slice would need — the specific list

| # | evidence | why this one |
|---|---|---|
| 1 | `probes_s92`'s 17-field equivalence, extended to grade M3 against the verbatim legacy passes, over the six existing corpora **forward, reversed, and all six permutations** of the order-conflicting corpus | the established instrument; permutations are what catch a capture that leaks file order |
| 2 | `probes_s94`'s cap harness re-pointed at `FOLD_DEPTH_CAP` — a tail at depth 16, at 17, and a wide argument fan-out at the boundary | the cap has never been observed to fire on any corpus (§4.2 item 4) ⇒ corpora provide zero coverage here |
| 3 | A **`file_defines_method` discriminator**: a file with `def format` and a method whose tail is `format("x")`, so a capture that drops the `Definition` name set flips one diagnostic | the one whole-file read; nothing else exercises it |
| 4 | A **splat/hole discriminator**: `def m; foo(*a); end` and `def m; [1, *a]; end` beside blanked siblings | §4.2 item 3 — the failure is silent and in the quiet direction |
| 5 | Mutation table: drop one grammar shape, drop the `Definition` set, off-by-one the depth cap, blank with `Node::Other` — each must fail a *named* test | non-vacuity, #92/#94/#101 discipline |
| 6 | `harness/run_snapshot.rb` — 98 fixtures, 407 matched, 2 registered divergences, **0 unregistered extras** | a new registered divergence is a finding, not a pass |
| 7 | `PYTHONHASHSEED=0 harness/fp_audit.py --gaps --sweep` — **0 FP / 9,204 files, gap set BYTE-unchanged** vs a branch-point baseline | mandatory here specifically: the memory note `fixture-corpus-blind-spot` records that 6 of 9 survey-FP root causes were scoping/position bugs the fixture corpus cannot contain, and a renumbering capture is exactly a position change |
| 8 | `rigor check` stdout + stderr + exit byte-identical vs a master-built baseline on mastodon/app **and** gitlab-foss/lib, under default threads **and** `RAYON_NUM_THREADS=1`, with the two thread modes also agreeing | the arc's standing tripwire; and the `sweep-measures-release-binary` memory applies — rebuild release, verify the binary is newer than `crates/` |
| 9 | LSP: `overlay_source_index_legacy` equivalence over the ten editor states, graded by the full diagnostic set | the held-table consumer; a capture changes what `HeldFile` holds |
| 10 | A **non-vacuity floor** — assert `method_returns`, `param_bound_returns` and `literal_returns` are non-empty and equal to the oracle's on every corpus | a capture that silently declines everything is byte-identical on a corpus with no folds |

Note that #7's "gap set byte-unchanged" is the only gate that can catch an
*under*-emission introduced by a lost shape, and that neither #6 nor #7 can see
project-`sig/` behaviour (`fp-audit-blind-to-project-sig`).

---

## 5. Verdict

### **NEEDS-A-NARROWER-SLICE — and the narrow slice is a measurement, not code.**

Not GO: the LSP prize is real (~207–241 MB at 4,675 files, 48–56 % of a measured
428.3 MB) but it is headroom against a budget that is 71 % consumed, not a live
gap, and the dominant half of the work — Pass 3's capture — has no closed
grammar, no safe prune, and an unknown size.

Not NO-GO either, because the structural obstacles turned out to be *smaller*
than #92 §5 and #104 §0 recorded them:

* the merge's non-M3 AST read is one `file_key()` line;
* Pass 4b — the only cross-file descent — is a closed 10-shape, depth-16 grammar
  with an exact harvest-time prune;
* Pass 3 is subtree-confined with exactly one enumerable whole-file scan;
* stage 3 is strictly per-file-local.

"AST eviction is blocked" is true but is now a one-sentence statement about one
pass, not about the merge.

### 5.1 The STOP condition — take this number first

**Measure `f`: the fraction of each file's arena reachable from a surviving
method tail**, at 1,236 / 3,117 / 4,675 files. It is a `#[cfg(test)]` walk over
already-built ASTs — no timing, no benchmark, no production code:

```
retained = ⋃ over ClassDef/ModuleDef · MethodBody passing gates 2/3/4
             of subtree(mb.body.last())
         ∪ { Definition nodes' name slots }
f = |retained| / ast.len()
```

* **f ≳ 0.40 ⇒ NO-GO, close the line.** The capture is the AST; §3.1's third
  column shows the win collapsing to under a third of the term.
* **f ≲ 0.15 ⇒ the slice sequence below is worth specifying.**
* in between ⇒ a judgement call that at least has a number under it.

Report `f` with its distribution, not just the mean — the arc's own lesson
(`coverage-arc-and-node-audit-lesson`) is that histograms mask over-claims, and
one 5,000-node file with a huge tail matters more than the mean suggests.

### 5.2 If `f` is favourable: the slice sequence

1. **Move `file_key` into `Harvest`.** One field. Removes the merge's only
   non-M3 AST read. Independently useful for persistence (#104 probe §1.3).
2. **Capture Pass 4b's fold inputs.** The genuinely bounded half. **It does not
   enable eviction on its own** — Pass 3 still reads every tree — but it stands
   alone on its own merits: it removes `FoldSite::ast_idx`, the slice-position
   hazard #92 §5 named as obstacle 1; it lets `fold_expr` drop
   `&[&LoweredAst]` entirely; and it replaces arena indirection with a compact
   owned tree in the pass that was 43–47 % of the merge before #94. Parity
   evidence: rows 1, 2, 5, 6, 7, 8, 10 of §4.4.
3. **Capture Pass 3's tail sub-arena.** Only with `f` in hand. Compacted and
   renumbered (blanking saves nothing, §3.1), with a hole policy that is *not*
   `Other`/`Statements`, and with the unmodified `type_of` running over it so
   the only claim is reachability, not semantics. Parity evidence: all ten rows.
4. **Then, and only then**, the LSP change: `HeldFile` drops its
   `Arc<LoweredAst>` (`lsp.rs:869`), `held_pair` (`:1059`) returns the harvest
   alone, `overlay_source_index` (`:2663-2699`) merges without trees. Note that
   this is the *whole* consumer: the held AST's only production read is
   `lsp.rs:2690`.

The task's suggested alternative — "capture only Pass 3's inputs and leave 4b" —
is the wrong way round. 4b is the capturable one and the one with standalone
value; Pass 3 is the one that must be justified by a measurement.

### 5.3 Cheaper partial wins, sized

| win | prize | risk | status |
|---|---|---|---|
| **Cut the cross-file cache cap from 8** | **~101 MB at 4,675** (6 × 16.8 MB, measured) | none — a constant; costs hover/completion hit-rate across many open files | **available today**, and it is bigger per unit of risk than anything in §5.2 |
| Evict held ASTs when the guard is off | **0** | — | **already implemented** — `lsp.rs:2620` drops the held table on `GuardVerdict::Disabled`. And it now fires *less* often, because the held-harvest slice keeps the guard ON at 4,675 |
| Move `file_key` into `Harvest` | 0 MB | ~none | prerequisite for §5.2, worth doing regardless |
| Capture Pass 4b only | 0 MB of eviction | moderate | real non-memory value (§5.2 step 2) |

The first row deserves emphasis: **134.4 MB of the measured 428.3 MB is a
hover/completion convenience cache with a hand-chosen cap of 8**, and shrinking
it is a one-constant change with no parity surface at all. Any memory
conversation about the LSP that starts with the ASTs is starting with the harder
half. Whether 8 is the right cap is a hit-rate question the cache note's own
measurements can probably answer without new work.

### 5.4 Explicitly rejected

* **Caching Pass-3 *results* per file.** They are cross-file-dependent by
  construction (#92 §2.2's `MAX = 5` / `MAX = 6` probe), so this is a
  dependency-tracking design with an invalidation surface, i.e. Salsa by another
  name — which ADR-0006 defers until profiling shows cross-file invalidation
  dominates.
* **A symbolic/conditional capture** ("types to String unless the index says
  X"). Enumerable in principle (the Pass-3 typer's index surface is C5, C1, the
  class registry and `project_writes_constant` — note `method_returns` /
  `param_bound_returns` / `literal_returns` / `definers` are all still `Default`
  while Pass 3 runs, since M3 assigns them afterwards). Rejected anyway: it
  re-implements the typer's decision procedure, which is the maximal parity
  surface.
* **Shape-based pruning of the Pass-3 subtree.** §1.2 — the subset argument this
  repo has been burned by four times.

---

## 6. What this note does not claim

* **No measurement was taken.** `f` — the number the verdict turns on — is
  unmeasured, and §5.1 exists because of that. The Pass-4b capture size is an
  estimate from the grammar, not a count.
* **`size_of::<Node>()` ≈ 240 B is COMPUTED** from `Definition`'s field list
  (`ast.rs:440-500`), not observed. It is used only for the qualitative claim
  that blanking cannot save memory and a capture must compact.
* **The RSS figures are other notes' measurements**, taken on a machine the
  baseline note itself flags as running 25–55 % above the S4b-era numbers, and
  the two AST bands disagree by ~20 %. Every conclusion here rests on ratios and
  on the 428.3 MB / 134.4 MB decomposition, not on any single millisecond or
  megabyte.
* **The `type_of` closure audit is mechanical but bounded to `lib.rs:403-1981`.**
  It is sound because every AST-taking call in that range is a `self.` method
  also in that range (checked), but a future helper added outside it would
  escape the audit — which is the same one-line fragility §1.2 flags for
  `lexical_scopes`.
* **No LSP hover/completion path was audited** beyond confirming that the held
  AST's only production consumer is the merge. The cross-file cache's own
  `Arc<SourceIndex>` entries were priced from the cache note, not re-derived.
* **No production code, no `harness/` change, no reference work.**

## Gates

| gate | verdict |
|---|---|
| `cargo test --workspace` | **PASS** — 447 / 279 / 251 / 94 / 58 / 48 / 47 / 24 / 15 / 4 / 3, **0 failed** |
| `python3 harness/docs_check.py` | **PASS** — 4 budgets, links resolve |
| files under `crates/` touched | **none** |
| `reference/rigor` / `REFERENCE_RIGOR_DIR` touched | **no** |
