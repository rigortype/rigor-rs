# S102 — path-derived identity for the constant-consumption gate (impl, 2026-08-26)

Implements [the mini-spec](20260826-s102-file-identity-mini-spec.md) / issue #102.
Branch `claude/s102-file-identity`, cut from `7ca6e4a`. The C5 per-file
constant-VALUE consumption gate no longer compares a process-global counter; it
compares the file's CANONICAL PATH. Semantics are identical — the path is what
"per-file" always meant — and the headline acceptance is the FLIPPED regression
test: a cache-hit hover now folds `FOO : 5` instead of degrading it to
`FOO : Dynamic[top]`.

## 1. The `file_id` consumer inventory (the spec's first deliverable)

Every occurrence of `LoweredAst::file_id` in `crates/` at the branch point,
classified. 53 lines total; the executable ones are these:

| # | site | role |
|---|---|---|
| W | `crates/rigor-infer/src/source_index.rs:906` (`SourceIndex::merge`, pass C5b) | **WRITE** — stamps the ASSIGNING file's id into `HarvestedConst` |
| R1 | `crates/rigor-infer/src/lib.rs:511` (`Typer::type_of`, `Node::ConstantRead` arm) | **READ** — `literal_constant(name, prefix, ast.file_id())` |
| R2 | `crates/rigor-infer/src/lib.rs:520` (same arm) | **READ** — `qualified_literal_constant(…)`, the stage-2e qualified spelling of the SAME harvest |

Everything else is one of:

* **30 test call sites**, all in `crates/rigor-infer/src/source_index.rs` — each
  passes an AST's id into `literal_constant` / `qualified_literal_constant`, or
  asserts the stamp. Plus one in the `probes_s92` legacy re-implementation
  (`:4279`), which exists to fingerprint-compare against `merge`.
* **11 doc-comment mentions** (`ast.rs`, `lsp.rs:7379`, `source_index.rs`
  `:187/:191/:424/:1057/:3914`).

**Verdict: the C5 gate is the ONLY consumer.** Nothing else reads it — no rule,
no LSP code path, no serialization, no hashing, no ordering, and deliberately not
`LoweredAst`'s hand-written `Debug` (which excludes it so the LSP's
incremental-vs-full differential tests can compare two lowerings of the same
bytes). So, per the spec, the counter was **retired as an identity** rather than
left beside a second one.

It survives in exactly one reduced role: as the source of `FileKey::Anonymous`,
which is the pathless non-collision guarantee. That is not "two identities" —
it is one identity TYPE with a variant for callers that genuinely have no path
(tests, stdin, a synthesized buffer), and the gate compares `FileKey` uniformly.

No consumer's semantics change under a path key, so nothing here required the
spec's STOP-and-report escape.

## 2. What changed

**`crates/rigor-parse`** — a new public `FileKey`:

```rust
pub enum FileKey { Path(Arc<Path>), Anonymous(u64) }
```

`FileKey::for_path` canonicalizes, falling back VERBATIM (not to an anonymous
key) when the path no longer resolves — a verbatim path still compares equal to
itself across lowerings, which is the only property the gate needs, and it is
exactly the parent-resolved spelling the LSP hands back for a buffer whose file
was just deleted. `FileKey::anonymous` is the retired counter.
`LoweredAst::file_id() -> u64` became `file_key() -> &FileKey`. `lower()` keeps
its signature and now means "no filesystem identity"; the new `lower_with_key()`
is the entry point for a caller that knows the file. `Debug` still excludes the
key.

**`crates/rigor-infer`** — mechanical. `HarvestedConst`'s middle field is a
`FileKey`; the merge stamps it from the paired AST exactly as before (the harvest
stays a pure function of `(AST, CoreIndex)`); `literal_constant` and
`qualified_literal_constant` take `&FileKey` and compare it. Three comparison
sites in total, all `==`.

**Who now supplies a path** (the four production lowerings that have one):

| site | key |
|---|---|
| `check` stage 1 (`main.rs:748`) | `FileKey::for_path(<the discovered path>)` |
| LSP tier-1 held files (`lsp.rs::lower_one`) | the same canonical path tier 1 records |
| LSP dispatch (`compute_diagnostics`) | `buf.canonical` — the buffer's file |
| LSP `hover` / `completion` | `uri_to_canonical_path(uri)` — the same file again |

A single helper, `lsp.rs::document_file_key`, is the one place the LSP derives it,
so the four LSP lowerings cannot drift apart on the rule. The key is derived from
the SAME path the overlay's REPLACE lookup compares, and `for_path` is idempotent
on an already-canonical path — so **key equality tracks REPLACE equality
exactly**, which is what makes the cache-hit hover match.

Deliberately left pathless: `namespace_completion`'s constant-path extraction and
`document_symbols` — neither types anything, so neither can reach the gate; and
the single-file tools (`annotate`, `type_of`, `sig_gen`, `coverage`), which build
their index from the very AST they analyse, so their key matches itself whatever
it is.

**Cost of the added `canonicalize`.** `check` now pays one `fs::canonicalize` per
file inside stage 1's rayon closure. Measured at `gitlab-foss/lib` (4 676 files,
3 interleaved runs each): master 0.662 / 0.538 / 0.559 s, branch 0.594 / 0.651 /
0.516 s — inside the run-to-run noise, so the syscall is free beside the read +
parse it sits next to. In the LSP it lands on tier-1 build and once per
hover/completion, neither of which is a hot loop.

**Why `check` is byte-identical, argued and then measured.** `check` lowers each
discovered path exactly once, so "same lowering" and "same path" partition the
file list identically — except when discovery yields one file twice (overlapping
`paths:` roots, a symlink and its target). There, a constant that file assigns is
counted TWICE by the single-assignment gate (`lit_writes` sums per-file counts)
and declines under both identities, so no entry exists to gate. Gates 3 and 4
below are the measurement.

## 3. Tests

| # | test | file |
|---|---|---|
| 1 | `crossfile_cache_hit_declines_the_same_file_literal_constant_fold` — **FLIPPED** to assert `FOO : 5` survives a cache hit | `crates/rigor-cli/src/lsp.rs` |
| 2 | `crossfile_cache_hit_still_gates_a_cross_file_constant_per_file` — NEW; closing the same-file fold must not open a cross-file one | `crates/rigor-cli/src/lsp.rs` |
| 3 | `path_keyed_gate_still_folds_per_file` — NEW; two real files, same bare constant name, each folds only its own — and a RE-lowering of one keeps its identity | `crates/rigor-infer/src/source_index.rs` |
| 4 | `pathless_lowerings_never_collide` — NEW; two pathless lowerings are two files, even byte-identical ones | `crates/rigor-infer/src/source_index.rs` |
| 5 | `harvested_const_file_id_is_the_assigning_file` (INVARIANT 5) — kept, now over `FileKey` | `crates/rigor-infer/src/source_index.rs` |

The flipped test was strengthened while it was flipped: it used to build the
"cached" index from a hand-rolled `lower()` + `overlay_source_index` call, which
would have made the new assertion partly self-fulfilling. It now takes the index
from a real `compute_diagnostics` dispatch, so BOTH sides — the worker's lowering
and `hover`'s re-lowering — are production code.

**Non-vacuity, each proven by re-breaking the implementation once:**

| re-break | tests that failed |
|---|---|
| `hover` lowers with `lower()` again (no key) | exactly 1 — the flipped test, with the ORIGINAL `FOO : Dynamic[top]` message |
| `compute_diagnostics` lowers with `lower()` again | exactly 1 — the flipped test |
| the gate itself stops comparing the file (`literal_constant`'s filter always true) | 6 in `rigor-infer` (incl. tests 3 and 4 and the three pre-existing per-file tests) + test 2 in `rigor-cli` |

So both halves of the identity are load-bearing, and the per-file rule itself is
still pinned by tests that fail when it is removed.

## 4. Gate verdicts (BARE, in the spec's order)

| # | gate | verdict |
|---|---|---|
| 1a | `cargo test --workspace` | **PASS** — 390 / 4 / 3 / 9 / 52 / 24 / 94 / 279 / 47 / 251 / 48, **0 failed**. Only two suites move: `rigor-cli` 389 → 390 (+1, test 2) and `rigor-infer` 277 → 279 (+2, tests 3 and 4) |
| 1b | `cargo clippy --workspace --all-targets -- -D warnings`, fresh `CARGO_TARGET_DIR` | **PASS** — clean, exit 0 |
| 2 | `ruby harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched / 443 reference, 35 gaps, 2 registered divergences, **0 unregistered** (the pre-change numbers exactly) |
| 3 | release rebuild + `python3 harness/fp_audit.py --gaps --sweep` | **PASS** — **0 FP across 9 204 files** (8 corpora present, 0 absent), and the **gap set is byte-identical** to the branch-point baseline: 65 body lines, every corpus's reference / rigor-rs / matched / FP / gap count and every per-rule total unchanged. The staleness guard ran (binary built 01:02:06, newest `crates/` source 00:59:44) — the sweep measured this tree |
| 4 | `rigor check` byte-identical vs the master baseline on `mastodon/app` | **PASS** — 420 findings, stdout + stderr + exit code all byte-identical under default threads AND `RAYON_NUM_THREADS=1`; the branch binary's own two thread modes also agree (ADR-0020). Repeated on `gitlab-foss/lib` (4 676 files, 1 093 findings) — identical on all three channels, both modes |
| — | `python3 harness/docs_check.py` | **PASS** |

The sweep baseline was taken at the branch point with a master binary saved
aside, `PYTHONHASHSEED` unset on both runs, against a `reference/rigor` submodule
populated in this worktree at the pin `b10bd5df` (v0.3.4) — never
`REFERENCE_RIGOR_DIR`, never a different checkout.

## 5. Deviations from the spec

1. **`completion` was stamped too, not just `hover`.** The spec's buffer clause
   names "the buffer's lowering"; `completion` lowers the same buffer's text with
   one stub name spliced in, and types the receiver against the same cached
   index. Leaving it pathless would have left a second copy of the bug in the
   sibling handler, and the two handlers already share `crossfile_for` precisely
   so they cannot drift. It is the same fix, not a wider one: `completion` reads
   the C5 gate through `Typer::type_of` exactly as `hover` does.
2. **The flipped test's fixture was rebuilt around `compute_diagnostics`**
   (§3) rather than only having its assertion inverted. Inverting alone would
   have left the cached index built by the test itself, which is the one thing
   the assertion must not control.

Nothing else departs from the spec. No persistence work, no `harness/` change,
no other consumer of the new key, and no change to what the C5 gate admits.

## 6. Left open (deliberately)

* **The harvest cache (#93) is UNBLOCKED, not built.** `HarvestedConst` may now
  carry its key verbatim across a process boundary; the merge still stamps it
  from the paired AST because the harvest is still a pure function of
  `(AST, CoreIndex)` and carries no file identity of its own. Making a `Harvest`
  self-identifying is that slice's work, not this one's.
* **The pathless key is still a counter**, which is the one thing a persisted
  ANONYMOUS entry could not replay. Nothing persists an anonymous lowering today
  (only `check` and the LSP build project indices, and both supply paths), so the
  hazard has no reachable site — but a future persistence slice must refuse to
  serialize a `FileKey::Anonymous` rather than re-stamp it.
