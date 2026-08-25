# `SourceIndex::build_project` pass inventory — issue #92 probe (2026-08-25)

The first deliverable of [#92](https://github.com/rigortype/rigor-rs/issues/92)
(harvest/merge decomposition), and the investigation the
[S4b mini-spec](20260719-lsp-s4b-overlay-mini-spec.md) deferred as
`SourceIndex::extend_with`. **Investigation only — nothing was refactored.**

Everything below was PROBED, not read off. The standing rule (memory
`subset-arguments-need-probing`) is that "we do strictly less / this is
additive" arguments fail when asserted from reading alone. Three things read
one way and measured another: Pass 2 *looks* cross-file and is not (§2.1);
`infer_method_returns`'s "a method never appears in BOTH maps" is false across
a cross-file reopen (§8); and harvest-then-evict is NOT unblocked by this
decomposition, because two merge-resident passes still walk the ASTs (§5).

Subject: `crates/rigor-infer/src/source_index.rs` — the struct at
`source_index.rs:180-349`, the entry point at `source_index.rs:391-679`.
`SourceIndex::build` (`source_index.rs:372-374`) is literally
`build_project(&[ast], core)`, so it is not a separate surface.

Instruments (committed, throwaway, `#[cfg(test)] mod probes_s92` at the end of
`source_index.rs`): a 17-field fingerprint of a built index plus eight probes.
Run with `cargo test -p rigor-infer probes_s92 -- --nocapture --test-threads=1`.
Delete the module when the slice lands.

---

## 1. Verdict up front

| | |
|---|---|
| Passes that are per-file decomposable | 8 of 13 |
| Passes that must stay in the serial merge | 5 of 13 |
| **Share of stage-2 wall time in the merge-resident passes** | **~57 %** (gitlab-foss/lib, 4 675 files) |
| Order-sensitive fields (file order changes the value) | `names`, `override_classes` |
| Fields whose ORDER is already nondeterministic per PROCESS | `definers`, `literal_constants`, `nested_constant_namespaces` |
| Order leakage into diagnostics on real corpora (25 shuffles, 5 corpora, 7 793 files) | **none observed** |
| Order leakage into diagnostics, minimal adversarial cases | **2 found, both reproducible** |

The headline for the arc: the decomposition is *feasible* and the coupling is
*enumerable*, but **`compute_literal_returns` (ADR-0038, Pass 4b) alone is
47 % of `build_project`** and is genuinely cross-file. A harvest/merge split
that leaves it in the merge caps the achievable stage-2 speedup at ~2.3×
(Amdahl), and an LSP keystroke would still pay ~100 ms at gitlab-lib scale.
That number should shape the mini-spec (see §7).

---

## 2. Pass-by-pass classification

Execution order, as written. "Inputs" uses the issue's four codes:
**(a)** one file's AST, **(b)** the `CoreIndex`, **(c)** `SourceIndex` state
accumulated from OTHER files, **(d)** state produced by an EARLIER pass over
ALL files.

| # | Pass | Inputs | Writes | Verdict | Evidence |
|---|---|---|---|---|---|
| 1 | source class/module structure | (a) | `classes`, `names`, `name_to_id` | **decomposable** (ordered merge) | `source_index.rs:394-413`, `add_source` `:716-725`, `register` `:706-712` |
| 1b | **lexical override index** | (a) | `override_classes` | **decomposable** (ordered merge) | `:415-423`, `collect_override_classes` `:1168-1225`, `ingest_override_class` `:1228-1252` |
| C1 | constant-shadow tables | (d) — reads merged `override_classes` keys | `toplevel_constants`, `nested_constant_namespaces` | **decomposable** (pure function of the merged key set; also derivable per file and unioned) | `:425-444` |
| 1c | project-wide toplevel defs | (a) | `toplevel_defs` | **decomposable** (pure union) | `:446-495` |
| 1d | discovered methods per qualified owner | (a) | `discovered_methods` | **decomposable** (pure per-key set union) | `:497-520`, `lexical_scopes` `:1826-1830` |
| 1e | mutated positional params | (a) | `mutated_params` | **decomposable** (pure per-key set union) | `:522-555`, `def_names` `:1803-1810` |
| C5a | per-file literal-constant collect | (a) | *(harvest only)* | **decomposable** | `:566-577`, `collect_literal_constants` `:1906-1942` |
| C5b | literal-constant gates | (c) + (d) — single-assignment across files, name-collision vs merged `classes`/`override_classes` | `literal_constants`, `qualified_literal_constants`, `project_constant_write_names` | **CROSS-FILE — merge** | `:578-603`, esp. the `lit_multi` gate `:586-588` and the collision gate `:593-595` |
| 2 | RBS-known constant registry | (a) + (b) | `names`, `name_to_id` | **decomposable** (see §2.1 — the `classes` read is a no-op) | `:605-627` |
| 2b | tuple-element registry + declaration-only set | (b) + (c) — `!name_to_id.contains_key` over ALL files | `declaration_only_classes`, `names`, `name_to_id` | **CROSS-FILE — merge** (cheap: O(#tuple classes), file-count independent) | `:629-655` |
| 3 | tier-4b method returns | (a) + (b) + (d) — a `Typer` over the COMPLETE index | `method_returns`, `param_bound_returns` | **CROSS-FILE — merge** | `:657-664`, `infer_method_returns` `:1619-1685` |
| 4a | fold-def harvest + definers inversion | (a) | `definers` | **decomposable** (but `FoldSite.ast_idx` is a slice POSITION — §5) | `:674-675`, `collect_fold_defs` `:2030-2048`, `walk_fold_defs` `:2054-2106` |
| 4b | interprocedural literal-tail fold | (a) + (c) + (d) — resolves calls into OTHER files' bodies, applies the overridable degrade over the merged ancestry | `literal_returns` | **CROSS-FILE — merge** | `:676`, `compute_literal_returns` `:1263-1274`, `fold_expr` `:1340-1419`, `overridden_in_project` `:1466-1473`, `related_to_owner` `:1478-1500` |

### 2.1 Why the "decomposable" verdicts hold

The merge must replay per-file contributions **in a fixed path order**. Given
that, each decomposable pass's merge equals the current all-at-once
computation because:

* **Pass 1 / 1b.** Both are pure `&mut self` folds over one AST at a time with
  no read-back: `add_source` and `ingest_override_class` never consult any
  other field. So `build_project(f₁…fₙ)` ≡ `fold(ingest, harvest(f₁) ++ … ++
  harvest(fₙ))`, and replaying a per-file harvest list in the same order is
  literally the same call sequence. The order-bearing parts are exactly:
  `superclass` first-**Some**-wins (`:718-720`, `:1237-1239`),
  `method_visibilities` first-write-wins per method (`:1244-1246`), `includes`
  ordered append-with-dedup (`:1247-1251`); `methods` is a `HashSet` union
  (commutative).
  A bare reopen (`class Foo` with no `< X`) never clobbers a `< Bar` written
  elsewhere — `superclass` is only assigned when the slot is `None` — so only
  two CONFLICTING superclasses are order-dependent, which is a Ruby `TypeError`
  at runtime and did not occur in any corpus probed.
* **C1.** `toplevel_constants` is a `HashSet`; `nested_constant_namespaces` is a
  dedup'd `Vec` read only through `.any(…)` (`constant_shadowed` `:866-878`).
  The derivation is per-key and independent, so per-file-then-union and
  merged-key-set-then-derive agree on content.
* **Pass 1c / 1d / 1e.** Each reads only its own AST — the span containment in
  1c/1d/1e compares spans WITHIN one file — and writes only `HashSet` unions
  (`toplevel_defs`) or per-key `HashSet` unions (`discovered_methods`,
  `mutated_params`). Commutative and idempotent ⇒ order-free.
* **Pass 2.** The gate is `!idx.classes.contains_key(name) && (core.knows_class
  || core.knows_qualified_class)` (`:619-621`). The `classes` term reads
  merged state — but it is a **no-op for the result**: if `name` IS a source
  class, Pass 1 already called `register(name)` (`:724`), and `register` is
  idempotent (`:707`). So Pass 2 is exactly "register every `ConstantRead`
  whose name the CoreIndex knows", which needs only (a) + (b). The `CoreIndex`
  is frozen before any of this runs (ADR-0028), so a harvest may pre-filter
  against it.
* **Pass 4a.** `walk_fold_defs` is a per-file lexical walk; the `definers`
  inversion is a set-union over the merged `defs` key set, and every read of
  `definers` is `.any(…)` (`owner_defines` `:1454-1458`,
  `overridden_in_project` `:1470-1472`), so the `Vec` order is not semantic.

### 2.2 Why the "cross-file" verdicts hold — each demonstrated, not argued

(Every `.rb` snippet below and in §3.2 was run as a real file carrying a
`# frozen_string_literal: true` header + blank line; that is why the quoted
line numbers sit two lines below the snippet's own.)

**C5b (single-assignment gate).** Probe 3
(`probe_union_of_singletons_vs_project`): each single-file build harvests
`C1`; the 3-file project build has **no** `C1` at all. The project result is
NOT the union of the per-file results.

```
[literal_constants]
  project : C2->[#2=Scalar(Int(3))]
  singles : C1->[#1=Scalar(Int(1))]  ||  C1->[#2=Scalar(Int(2))] | C2->[#2=Scalar(Int(3))]
```

**Pass 2b (declaration-only).** Probe: `Process::Status` is declaration-only
iff no analyzed file names it. Reaches diagnostics —
`crates/rigor-rules/src/lib.rs:1436` gates the qualified-witness arm on it:

```ruby
# a.rb                          # b.rb
pid, status = Process.wait2     KLASS = Process::Status
puts pid                        puts KLASS
status.nosuchthing
```
`rigor check a.rb` → `a.rb:5:8: error: undefined method 'nosuchthing' for
Process::Status`. `rigor check a.rb b.rb` → **silent**.

**Pass 3 (method returns).** Probe 4
(`probe_pass3_depends_on_pass_c5_over_all_files`) — the typer is built over the
already-complete index (`:1627`), so a Pass-3 answer for file A can be flipped
by file B's constants:

```
a alone : A#m -> Some("Integer")
a + b   : A#m -> None
```

End-to-end (the spec's test case):

```ruby
# a.rb                    # b.rb
MAX = 5                   MAX = 6
class A
  def m
    MAX
  end
end
A.new.m.upcase
```
`rigor check a.rb` → `a.rb:11:9: error: undefined method 'upcase' for Integer`.
`rigor check a.rb b.rb` → **silent**. `a.rb` is byte-identical in both runs; a
per-file harvest of `a.rb` cannot decide this.

Note the compounding: `infer_one_param_bound` (`:1747-1786`) reads ONLY the
AST, but it is invoked in the `else` arm of `infer_one_return` (`:1666`), so
`param_bound_returns` inherits Pass 3's cross-file dependence.

**Pass 4b (overridable degrade + interprocedural resolution).** Probe 5
(`probe_pass4_degrade_is_cross_file`):

```
a alone : literal_returns[Base.m] = Some(Int(1))
a + b   : literal_returns[Base.m] = None
```

End-to-end (the second spec test case):

```ruby
# a.rb                          # b.rb
class Base                      class Sub < Base
  def self.enabled?               def self.enabled?
    true                            false
  end                             end
end                             end

if Base.enabled?
  puts "on"
end
```
`rigor check a.rb` → `a.rb:9:4: warning: condition is always truthy …`.
`rigor check a.rb b.rb` → **silent**.

`fold_expr` additionally *resolves into other files' bodies*: an implicit-self
or `Const.method` tail recurses through `resolve_fold_key` into a `FoldSite`
that names a different AST (`:1322-1323`). So Pass 4b is not merely gated by
cross-file state; it *reads other files' ASTs*.

---

## 3. Order sensitivity

### 3.1 What varies with FILE ORDER

Probe 1 (`probe_permutation_field_diff`), 5 permutations of a 4-file set, all
17 fields compared. Exactly **two** fields differ, plus one already-random one:

| field | why | leaks to diagnostics? |
|---|---|---|
| `names` | ClassIds are handed out in registration order = file order × AST arena order | channel EXISTS (§3.3); not observed reachable |
| `override_classes` | `method_visibilities` first-write-wins; `includes` append order | **YES — twice, §3.2** |
| `definers` | `Vec` order from `HashMap` iteration | no (all reads are `.any`) |

`classes`, `toplevel_defs`, `discovered_methods`, `mutated_params`,
`method_returns`, `param_bound_returns`, `literal_returns`,
`toplevel_constants`, `qualified_literal_constants`,
`project_constant_write_names`, `declaration_only_classes` were **identical
under every permutation**. In particular `literal_returns` never varied,
which is the empirical half of the argument that the `compute_literal_returns`
memo + per-root `visiting` cycle guard (`:1268-1302`) is order-independent.

### 3.2 The two order leaks that reach a diagnostic

Both are pre-existing (they are properties of today's `build_project`, not of
the refactor). The merge must reproduce them by ordering files identically.

**(i) `method_visibilities` first-write-wins.** Two files, one flip:

```ruby
# a.rb                # b.rb
class Base            class Base
  def m                 private
    1                   def m
  end                     2
end                     end
                      end

                      class Sub < Base
                        private
                        def m
                          3
                        end
                      end
```
`rigor check a.rb b.rb` → `b.rb:14:7: warning: visibility of 'm' reduced from
public to private (overrides Base#m); breaks substitutability`.
`rigor check b.rb a.rb` → **silent**.

**(ii) `includes` accumulation order** — a fully idiomatic Ruby shape (one
class reopened in two files, each adding an `include`). `override_ancestor_names`
(`:1116-1132`) walks includes in accumulated order, so the MRO's *nearest*
defining ancestor flips:

```ruby
# mods.rb                    # a.rb            # b.rb
module M1                    class Foo         class Foo
  def m; 1; end                include M1        include M2
end                          end                 private
module M2                                        def m; 3; end
  private                                      end
  def m; 2; end
end
```
`mods.rb a.rb b.rb` → `b.rb:8:7: warning: visibility of 'm' reduced from
public to private (overrides M1#m) …`. `mods.rb b.rb a.rb` → **silent**.

### 3.3 The `names` / ClassId channel

`Interner::cmp` orders `Type::Nominal` and `Type::Singleton` by `ClassId`
(`crates/rigor-types/src/interner.rs:135,137`), `Algebra::make_union` sorts
members by that comparator (`crates/rigor-types/src/algebra.rs:241`), and
`named_union` renders members in canonical order, floating only `nil` to the
end (`crates/rigor-types/src/display.rs:444-449`). So file order → registration
order → ClassId → rendered union order. Probe 7 shows it directly:

```
order [a,b]: Alpha=1000000 Beta=1000001 union renders as "Alpha | Beta"
order [b,a]: Alpha=1000001 Beta=1000000 union renders as "Beta | Alpha"
```

`erase_to_rbs` is **not** insulated either: `erase_union`
(`crates/rigor-types/src/display.rs:232-243`) hands members to `uniq_join`
(`:333-340`), which dedups but does **not** sort — the canonical (ClassId)
order survives into the erased spelling too. Checked because the opposite
would have been the comfortable answer.

What makes the channel unreachable TODAY is narrower and more fragile: every
consumer that renders a type is either a diagnostic that never witnesses a
union receiver (I looked for a reachable `check` case and found none), or a
SINGLE-FILE tool — `sig-gen`, `annotate` and `type_of` all call
`SourceIndex::build` on one AST (`crates/rigor-cli/src/sig_gen.rs:323`,
`annotate.rs:95`, `type_of.rs:111`), where the id order is the AST arena order
and no file list exists. 25 real-corpus shuffles found no difference.

Treat it as a live channel that the merge must close by construction (assign
ids in fixed path order), not as a bug to hunt — and note that the recon
note's consumer 1 (cross-file hover/completion off the project index) would
move `describe_named` onto a `build_project`-built index, i.e. would make this
channel reachable.

### 3.4 `HashMap`/`HashSet` iteration order that reaches a built field

Three sites iterate a `HashMap` and push into a `Vec`:

| site | field | reads | verdict |
|---|---|---|---|
| `:431-443` (`override_classes.keys()`) | `nested_constant_namespaces` | `.any(…)` | inert |
| `:585` (`lit_first` by value) | `literal_constants` | `.filter().max_by_key(len)` | inert — see below |
| `:2041` (`defs.keys()`) | `definers` | `.any(…)` ×2 | inert |

Probe 6 run twice in separate processes, same input, same file order:

```
run A: definers[shared/Instance] = ["Delta","Beta","Alpha","Gamma"]
run B: definers[shared/Instance] = ["Beta","Alpha","Gamma","Delta"]
run A: literal_constants[KEY] namespaces = ["Gamma","Delta","Beta","Alpha"]
run B: literal_constants[KEY] namespaces = ["Delta","Gamma","Alpha","Beta"]
run A: nested_constant_namespaces[Time] = [Alpha, Gamma, Delta, Beta]
run B: nested_constant_namespaces[Time] = [Alpha, Beta, Gamma, Delta]
```

These three `Vec` orders are **already nondeterministic between runs of the
same binary on the same input**. That is itself the proof they cannot leak:
rigor-rs is byte-deterministic (ADR-0020) and the sweep is stable, so nothing
downstream can be reading them positionally.

`literal_constant`'s `max_by_key` (`:756`) returns the LAST maximum on a tie,
so the tie case needs an argument rather than an appeal to `.any`: entries
under a bare name `B` come from distinct qualified keys `ns::B` (`lit_first` is
keyed by the qualified name), so two entries with the same `ns` are impossible;
and after the visibility filter every surviving `ns` is a prefix of the same
`use_prefix`, and a prefix of a given length is unique — so all surviving
lengths are distinct and **no tie can occur**. The result is order-free.

**Recommendation for the spec:** make the merge emit all three in path order.
That is a strict improvement (deterministic artifacts, which the persistence
slice needs — recon note §"Why oracle parity is not threatened") and provably
cannot change behaviour.

### 3.5 What "fixed path order" has to MEAN (a trap)

The issue and the recon note both say "merge in a fixed path order". Read
literally as *lexicographic sort of the whole file set*, that would be a
behaviour change on exactly the §3.2 shapes.

Today's order is `expand_check_paths` (`crates/rigor-cli/src/main.rs:517-536`,
documented at `:507-516` as a faithful port of the reference's
`Runner#expand_paths`): **each directory argument expands to its recursive
`**/*.rb` SORTED, and the per-argument results are concatenated in ARGUMENT
order**; an explicit `.rb` argument keeps its argument position. It is not a
global sort. `analyze_files` then feeds `build_project` in exactly that order
(`main.rs:781-782`). The LSP's overlay reproduces the same discipline over
`paths:` roots (`crates/rigor-cli/src/lsp.rs:851-865`, `:786-787`).

So the merge order must be **the existing expansion order**, and the mini-spec
should say so in those words. `rigor check a.rb b.rb` and `rigor check b.rb a.rb`
are legitimately different runs today (§3.2), and the first-write-wins
semantics are documented in-tree as mirroring the reference's accumulator
(`source_index.rs:166`, `:1243` — asserted there, NOT re-verified against the
oracle in this probe: no reference work was done here). Normalising the order
away would therefore risk a divergence, not remove one.

### 3.6 Real-corpus reorder probe

`rigor check` over shuffled file lists, diagnostics compared as a SORTED SET
(output is emitted in input order, so a raw byte-compare would differ for a
non-finding reason). Release binary, `harness/`-grade corpora:

| corpus | files | diagnostics | shuffles | differing |
|---|---|---|---|---|
| survey/haml/lib | 51 | 5 | 6 | 0 |
| survey/net-ssh | 180 | 125 | 6 | 0 |
| mastodon/app | 1 236 | 420 | 6 | 0 |
| gitlab-foss/lib | 4 676 | 1 093 | 4 | 0 |
| survey/dependabot-core | 1 650 | 138 734 | 3 | 0 |

Zero differences anywhere. Combined with §3.2 this says the order leaks are
real but not *dense* — a per-file harvest + path-ordered merge is very likely
to be bit-identical on the sweep, and §3.2's two fixtures are the tests that
would catch it if the merge got the order wrong.

---

## 4. The lexical override index (issue item 5)

**What it is.** `override_classes: HashMap<String, OverrideClass>`
(`source_index.rs:221`, type at `:158-171`) — the ADR-35 slice-1 index keyed by
FULLY LEXICALLY-QUALIFIED name (`IssuableFinder::Params`, never the collapsed
`Params`). Each entry holds the as-written `superclass`, the `include`/`prepend`
names in source order, a `method_visibilities` table, and the direct-`methods`
existence set. The qualification is the documented zero-FP keystone: collapsing
namespaced same-last-segment classes invented phantom overrides (the
gitlab-foss FP cluster).

**Built by** `collect_override_classes` (`:1168-1225`), a recursive walk of ONE
AST with a nesting-prefix stack, descending only through
`Program`/`Statements`/class/module bodies. **Reads (a) only** — no `CoreIndex`,
no other index field. **Writes** `override_classes` only.

**Read by**, in the same build: C1's shadow-table derivation (`:431`),
C5b's name-collision gate (`:593`), and — through `override_ancestor_names` /
`resolve_override_ancestor` (`:1116-1156`) — Pass 4b's `resolve_instance_owner`
(`:1426-1451`) and `related_to_owner` (`:1478-1500`). Read by the rules through
`nearest_ancestor_defining` (`:1076-1110`, called at
`crates/rigor-rules/src/lib.rs:1249`).

**Verdict: DECOMPOSABLE, with an ordered merge.** The mini-spec's hazard
("later passes may depend on complete earlier-pass state, e.g. the lexical
override index") is **half right and worth restating precisely**:

* The override index itself is a clean per-file harvest. Nothing in
  `collect_override_classes` or `ingest_override_class` reads back accumulated
  state; the merge is a replay of per-file `ingest` calls.
* But its **first-write-wins and include-order semantics are load-bearing and
  DO reach diagnostics** (§3.2). "Decomposable" here means "harvest per file,
  merge in a fixed path order" — NOT "mergeable in any order". An unordered
  or parallel merge would change diagnostics.
* And the passes that read it (C1, C5b, Pass 4b) need it **complete**. So the
  merge has an internal barrier: all override harvests must be folded before
  C1/C5b/Pass 4b run. That is the real content of the mini-spec's warning.

---

## 5. Two obstacles the recon note's slice 2 (harvest-then-evict) has to solve

Neither blocks #92 itself; both block "hold harvests instead of ASTs".

1. **Pass 3 and Pass 4b need the AST at MERGE time.** `infer_one_return` calls
   `typer.type_of(ast, ret_id, …)` (`:1714`) and `fold_expr` walks
   `ast.get(node_id)` recursively (`:1355`). `FoldSite` stores
   `ast_idx: usize` — a POSITION IN THE `asts` SLICE (`:131-135`,
   `:2037-2038`, dereferenced at `:1322`) — which a per-file harvest cannot
   carry across a rebuild with a different file set. A `Harvest` that is to
   outlive its AST must carry a self-contained representation of each method's
   tail expression (an owned mini-tree), or these two passes must be
   restructured. Whatever replaces `ast_idx` must be a stable file handle, not
   a slice index.
2. **`LoweredAst::file_id` is a process-global counter, not content**
   (`crates/rigor-parse/src/ast.rs:814-833`). It is stored INSIDE the harvested
   constant values (`HarvestedConst = (Vec<String>, u64, ConstLit)`,
   `:177`) and compared against the analyzed file's id at the use site
   (`literal_constant` `:754`, `crates/rigor-infer/src/lib.rs:511,520`). A
   cached/persisted harvest therefore cannot store `file_id` as-is; the merge
   must re-stamp it (or the field must become a path/content key). This is the
   one place a blake3-keyed on-disk harvest would silently go wrong.

---

## 6. What a `Harvest` needs to contain

One file's contribution, everything derivable from `(AST, CoreIndex)` alone.
Merge order = fixed path order. Grouped by merge discipline:

**Pure unions (order-free, mergeable in any order):**

| harvest field | merges into |
|---|---|
| `toplevel_defs: HashSet<String>` | `toplevel_defs` |
| `discovered_methods: HashMap<String, HashSet<String>>` | `discovered_methods` (per-key union) |
| `mutated_params: HashMap<String, HashSet<usize>>` | `mutated_params` (per-key union) |
| `constant_write_bare_names: HashSet<String>` | `project_constant_write_names` |

**Ordered replay (first-write-wins / append semantics — merge in path order):**

| harvest field | merges into | discipline |
|---|---|---|
| `source_classes: Vec<(name, Option<superclass>, Vec<method>)>` in AST order | `classes` + `register` | superclass first-Some-wins; methods union; registration order = ClassId |
| `override_classes: Vec<(qualified, Option<superclass>, Vec<method>, Vec<(method, Visibility)>, Vec<include>)>` in walk order | `override_classes` | superclass first-Some-wins; `method_visibilities` first-write-wins; `includes` append-dedup |
| `rbs_constant_names: Vec<String>` in AST order (already filtered by `core.knows_class \|\| core.knows_qualified_class`) | `names` / `name_to_id` | registration order = ClassId |
| `constant_writes: Vec<(qualified, namespace, Option<ConstLit>)>` + per-file write COUNT per qualified name | C5b's `lit_first`/`lit_multi` | first file wins the value; Σ counts ≥ 2 ⇒ multi. Equivalent to today because today's `lit_first`/`lit_multi` (`:566-577`) also count intra-file duplicates |
| `fold_defs: Vec<(qualified owner, method, DefKind, tail, has_explicit_return)>` | `defs` (Pass 4a) | `Vec<FoldSite>` append order per key. **`tail` must NOT be a slice index — §5** |

**Computed at merge only (must NOT be in a harvest):**

| field | why |
|---|---|
| `toplevel_constants`, `nested_constant_namespaces` | derived from the merged `override_classes` key set (could also be per-file-derived + unioned; either is equivalent) |
| `literal_constants`, `qualified_literal_constants` | single-assignment gate + collision gate against merged `classes`/`override_classes` |
| `declaration_only_classes` | needs "no file registered this name" |
| `method_returns`, `param_bound_returns` | typed against the complete index |
| `definers` | inversion of the merged `defs` |
| `literal_returns` | interprocedural fold across files + overridable degrade |

`SourceIndex` has no `Clone` today (`#[derive(Default)]` only, `:179`); a
layered/masked index will need one, and `SourceClass`/`OverrideClass` already
derive it (`:138`, `:158`).

---

## 7. Cost: where stage 2 actually goes

Measured with temporary per-pass instrumentation (added, measured, **removed
before commit** — reproduce by re-inserting `Instant` marks between the pass
comments in `build_project`). Release build, 12 threads, warm page cache.

| pass | mastodon/app (1 236 f) | gitlab-foss/lib (4 675 f) | decomposable? |
|---|---|---|---|
| 1 classes | 5.7 % | 4.2 % | yes |
| 1b override | 6.0 % | 6.0 % | yes |
| C1 shadow tables | 0.8 % | 0.8 % | merge (trivial) |
| 1c toplevel defs | 2.7 % | 2.6 % | yes |
| 1d discovered methods | 6.9 % | 8.1 % | yes |
| 1e mutated params | 4.2 % | 4.3 % | yes |
| C5a per-file collect | 2.9 % | 5.2 % | yes |
| C5b merge gates | 1.0 % | 1.1 % | merge |
| 2 constant registry | 4.0 % | 4.0 % | yes |
| 2b declaration-only | 0.1 % | 0.0 % | merge |
| 3 method returns | 12.4 % | 7.7 % | **merge** |
| 4a collect fold defs | 10.3 % | 8.7 % | yes |
| **4b compute_literal_returns** | **43.1 %** | **47.1 %** | **merge** |
| total (stage 2) | 28.5 ms | 181 ms | |

Parallelisable share: **~46 %** (mastodon) / **~43 %** (gitlab-lib).
Merge-resident: **~54 % / ~57 %**.

Two consequences the mini-spec should absorb:

* **Amdahl.** Even with a perfect free harvest, stage 2 shrinks to ~57 % of
  today. At gitlab-lib scale that is ~103 ms — still over the LSP's practical
  keystroke share of the ADR-0029 budget. The recon note's consumer 1 (layered
  index, guard removal) is therefore **not** unblocked by #92 alone; it needs
  `literal_returns` to become incremental or cacheable too.
* **`compute_literal_returns` is a standalone optimisation target,
  independent of #92.** It resolves every `defs` key and, per key with a
  folded value, runs `overridden_in_project` → `related_to_owner`, which is a
  fresh ancestor BFS per candidate definer (`:1466-1500`) with no memo across
  keys. That shape (O(keys × definers × ancestor walk), unmemoised) is the
  likeliest reason it dominates. Worth its own issue.

Stage context for the same runs (`RIGOR_TIMING=1`): gitlab-foss/lib —
index-load 26 ms, stage1 93–216 ms, **stage2 183–221 ms**, stage3 49–58 ms.

---

## 8. Incidental finding: a false doc claim in `infer_method_returns`

`source_index.rs:1613-1615` states "A method never appears in BOTH maps (its
tail is either a concrete core class under the empty env, or param-rooted —
never both)". True per DEF SITE; false per `(class, method)` KEY once the
method is reopened across files, because each site is dispatched
independently through the `if …/else if …` at `:1652-1680`. Probe 8:

```ruby
# a.rb                        # b.rb
class A                       class A
  def m                         def m(x)
    "s"                           x
  end                           end
end                           end
```
```
method_returns[A#m]      = Some("String")
param_bound_returns[A#m] = Some(ParamBoundReturn { param_index: 0, chain: [] })
```

Harmless in effect — the call site consults `method_return` first and
`param_bound_return` only on a miss (documented at `:1013-1014`) — so this is
a comment defect, not a behaviour defect. Worth correcting while the file is
open for #92; it is exactly the kind of "obviously true" invariant a merge
refactor would lean on.

## 9. Anything that resisted classification

* Nothing was left unclassified, but two verdicts are narrower than they look
  and should be quoted with their qualifier:
  * **Pass 2 "decomposable"** rests on the `!classes.contains_key` gate being a
    no-op *because Pass 1 already registered every source class*. If Pass 1's
    registration ever becomes conditional, Pass 2 becomes cross-file.
  * **Pass 4a "decomposable"** is true of the WALK, not of `FoldSite`, whose
    `ast_idx` is a slice position (§5).
* The `names`/ClassId leak (§3.3) is **unproven-reachable**, not proven-safe.
  I looked for a reachable diagnostic and did not find one; that is weaker
  than a proof. Its containment rests on every type-rendering tool being
  single-file, which the LSP cross-file slice would end.
* `cargo fmt --check` is dirty across the whole repo on this machine's
  rustfmt (48 files, including untouched ones) — a toolchain-version
  mismatch, not a consequence of this branch.

## Gates run

* `cargo test -p rigor-infer` — 262 passed, 0 failed (261 before the probe
  module; the delta is the probes themselves).
* `cargo clippy -p rigor-infer --all-targets` — clean.
* `RIGOR_RS_BIN=target/release/rigor ruby harness/run_snapshot.rb` — PASS,
  0 unregistered extras, 2 registered divergences, 407/443 (the reference-free
  gate; `run.rb` needs `REFERENCE_RIGOR_DIR`, deliberately not touched).
* `python3 harness/docs_check.py` — PASS.
* No `reference/rigor` work, no `REFERENCE_RIGOR_DIR` — this is a port-side
  investigation only.
