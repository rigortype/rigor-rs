# Rust Glancer recon — a frozen persisted index for rigor-rs? (2026-08-25)

Prompted by the Rust Glancer announcement
(<https://rust-glancer.github.io/blog/hello-world/>) and matklad's commentary
(<https://matklad.github.io/2026/08/21/rust-glancer.html>); the implementation
was surveyed from a local checkout (`~/repo/rust/rust-glancer`), rigor-rs from
source + the S4b-era measurements. Question: should rigor-rs adopt its
architecture?

## Verdict

No ground-up redesign is warranted — rigor-rs ALREADY shares Glancer's base
choices: no Salsa ([ADR-0006](../adr/0006-incremental-computation.md)), a flat
arena AST (`LoweredAst` = `Vec<Node>`), and save-driven invalidation (the LSP
has no `didSave` handler; saves arrive via `didChangeWatchedFiles`). What
Glancer supplies is a **validated implementation shape for two things rigor-rs
designed but never built** — the ADR-0017/0028 persistent cache and the
S4b-deferred `SourceIndex::extend_with` — and its portable ideas all converge
on ONE keystone: per-file harvest/merge decomposition of `SourceIndex`.

Issues filed: [#92](https://github.com/rigortype/rigor-rs/issues/92)
(keystone), [#93](https://github.com/rigortype/rigor-rs/issues/93)
(measurements). Every other slice is gated on those two.

## The problem profiles differ — adopt selectively

Glancer's enemy is the Rust dependency graph (multi-GB RSS, proc macros; it
targets <100MB per engine). rigor-rs is three orders smaller: ~20MB baseline
RSS, CoreIndex build 40.7ms / 661 classes. The measured pain points here
([S4b note](20260719-lsp-s12-s4b.md)) are:

1. **Per-keystroke O(project) `SourceIndex` rebuild** — 58.0ms @ 3,117 files,
   135.5ms @ 4,675; the OverlayGuard cliff disables cross-file context
   entirely at ~4.7k files; cross-file hover/completion withheld for the same
   reason.
2. **Held `LoweredAst`s dominate LSP memory** — ~40KB/file, 189MB @ 4,675
   files. The index itself is ~15MB.

"Two orders of magnitude less RAM" is not the goal to chase; these two numbers
are.

## Idea-by-idea (each verified in Glancer's source, not just the blogs)

| Glancer idea | rigor-rs verdict |
|---|---|
| Frozen index: full reindex on save, no keystroke incrementality, no salsa | ✅ already half-true here (`reharvest_sources` = 0.21ms AST swap on save); what's missing is NOT rebuilding on the next dispatch |
| Drop syntax after extracting item facts ("ItemTree is a lowering input, not retained project state"; `evict_syntax_trees` phase points) | ✅ highest value: holding harvests instead of ASTs kills the 189MB term |
| Disposable on-disk cache: blake3 content keys, generation-fingerprint dir, schema version, atomic writes, fail-open, "rejected and rebuilt rather than partially salvaged" | ✅ the implementation template for ADR-0017/0028 — gated on measurement first (ADR-0037 precedent) |
| Current-body (per-METHOD) shallow reanalysis, `masked_files` + request-only decls | ⚠️ adopt at FILE granularity: rigor-rs has no per-method seam (three whole-file flow passes precede the converged walk), and the expensive part is the index rebuild, not single-file analysis |
| Engine-as-subprocess (heap-fragmentation isolation), jemalloc purge points | ❌ no measured driver at 200MB scale |
| bump arenas / mmap | ❌ Glancer itself rejected bumpalo and never implemented mmap (cache reads are seek + read_exact) |

## The keystone: harvest/merge decomposition (#92)

Ruby has no crate boundary, so the shard unit is the FILE — a *better* deal
than Glancer got: its worst invalidation hazard (one package's cache miss
propagates to its whole reverse-dependency closure, because cached scopes
retain dependency-local arena IDs) has no analogue here. `SourceIndex` is a
union of per-file contributions; file A's edit invalidates only harvest(A).

```
today:    build_project(ALL asts)                O(project); no provenance, no Clone
keystone: harvest(file) ∥  →  merge(fixed path order)   parallel collect + serial deterministic join
```

- ADR-0028's ordering contract survives by construction: harvests are the
  parallel part, merge is the frozen serial barrier — today's stage 2, cheaper.
- ADR-0020 determinism: merge in fixed path order; nothing
  environment-dependent enters a harvest.
- The known hazard is already on record in the
  [S4b mini-spec](20260719-lsp-s4b-overlay-mini-spec.md): `build_project`'s
  later passes may depend on complete earlier-pass state (lexical override
  index). The pass inventory + bit-identity parity evidence is #92's first
  deliverable; genuinely cross-file passes simply STAY in merge.

Three consumers, in order:

1. **LSP layered index** (Glancer's `masked_files` + request-only decls, at
   file granularity): frozen base merge + mask of the current file's saved
   contribution + overlay of the buffer's fresh harvest. Keystroke = parse
   buffer + harvest + O(1) layered lookup; re-merge only on save. The guard
   cliff disappears, and the synchronous handlers (hover/completion) can
   afford a layered lookup where they could never afford a rebuild — unblocks
   the S4b carve-out. **No new staleness**: today's REPLACE model already
   answers from saved harvests for every file but the current one; the
   layered model reproduces exactly that, so the FP surface is unchanged by
   construction (S4b acceptance 3, "replacement not addition", becomes the
   mask).
2. **Harvest-then-evict**: with harvests first-class, the LSP has no reason to
   hold ASTs (they exist only to feed REPLACE rebuilds). 189MB → harvest-sized.
3. **Disposable harvest cache** (CLI warm runs + LSP cold start): blake3(file
   bytes) keys, generation fingerprint over (`.rigor.yml`, `Gemfile.lock`,
   `sig/**`, binary version), atomic writes, fail-open. NOT under `.rigor/` —
   that directory belongs to the reference's own cache.

A property worth naming: **the harvest hash is a firewall**. A body-only edit
(the common case) leaves harvest(A) unchanged ⇒ merged index unchanged ⇒
every other file's cached diagnostics stay valid. The moral equivalent of
Glancer's "a declaration with the same saved header keeps its saved identity".

## Why oracle parity is not threatened

- The oracle only ever sees SAVED files. The frozen model changes
  unsaved-buffer behavior only, where no oracle exists; at save it converges
  to exactly `check`'s answer (the existing `lsp_check_parity` bar).
- Cache correctness reduces to "hit ≡ recompute", which ADR-0028 already
  specifies as `--verify-incremental`; the no-partial-salvage policy keeps
  that gate simple.
- ADR-0020's normalization is precisely what makes serialized artifacts
  bit-stable — rigor-rs is unusually well positioned for persistence. The one
  new obligation: `HashMap` iteration order must not leak into any artifact
  (sort on write).

## Synergies

- ADR-0006's open trigger ("adopt Salsa when cross-file invalidation dominates
  editor latency") gets a different answer: at this scale the frozen model IS
  the answer, and Glancer is the existence proof.
- ADR-0043 effects slice 4 (transitive propagation over the project call
  graph) wants exactly this substrate: per-file facts + a deterministic global
  join. `.rigor-effects.yml` is already the repo's only *intended* persistent
  artifact, and Glancer's `Data`/`Facts` split (structure vs conclusions as an
  index-aligned sidecar) is the right vocabulary for summary storage.
- ADR-0007's unrealized intent (pre-parsed embedded RBS — today the 4MB of
  vendored text is re-parsed every start, 40.7ms) is a candidate LAST slice
  only: it requires replacing the `Box::leak` `&'static str` design with an
  offset/interned representation, and 40ms does not justify that alone.

## Slices

1. **Keystone** — [#92](https://github.com/rigortype/rigor-rs/issues/92):
   harvest/merge decomposition, pass-coupling inventory, bit-identity parity
   evidence. Everything else is gated on this.
2. LSP layered index + AST eviction (gated on 1): guard removal, cross-file
   hover/completion, memory.
3. On-disk harvest cache (gated on 1 + the
   [#93](https://github.com/rigortype/rigor-rs/issues/93) measurements — a
   NO-GO is a valid outcome, per ADR-0037).
4. Optional: per-file diagnostics cache + `--verify-incremental` (gated on 3).
5. Optional, last: pre-serialized CoreData (the ADR-0007 intent).

Rejected: subprocess engine, bump arenas, mmap, per-method current-body
(revisit only if single-file analysis itself ever blows a budget).
