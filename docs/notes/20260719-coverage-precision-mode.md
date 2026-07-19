# Coverage precision mode — implementation + measured parity (2026-07-19)

Implements the two slices scoped in
[20260719-coverage-command-scoping.md](20260719-coverage-command-scoping.md):
the `rigor coverage` default (precision) mode and the read-only MCP `coverage`
tool. Branch `coverage-precision-mode` (PR #33); oracle = the pinned reference
`7a69f142`, every invocation hardened per `UPSTREAM.md` (#194 plugin path),
from a clean temp cwd.

## Construction (what is byte-exact by design)

- **Denominator**: the scan walks the real Prism tree via the `ruby_prism`
  `Visit` trait — the identical `compact_child_nodes` child set the reference's
  `NodeWalker`/`rigor_each_child` yields — minus the same
  `NON_EXPRESSION_NODE_TYPES` list. Measured: `expressions_typed` is equal on
  **70/70 fixtures, 4235/4235 (conference-app/app), 31381/31381
  (mastodon/app/models)** — zero denominator drift anywhere.
- **Renderers**: text + JSON are hand-ported (Ruby `JSON.pretty_generate`
  shape, `Float#round` half-away-from-zero + `Float#to_s` spellings, `ljust`/
  `rjust`, the `files - parse_errors` header arithmetic). Proven byte-exact by
  the 31 whole-output fixture matches and byte-identical multi-file runs.
- **File resolution**: `collect_paths` reproduces the reference exactly,
  including Ruby `Dir.glob("**/*.rb")`'s *per-directory* traversal order (the
  `admin/` dir sorts before the `admin.rb` file — a flat path sort is WRONG;
  caught + fixed against conference-app). File lists byte-identical on both
  real corpora.
- **Tiers**: per-node classification reproduces `ExpressionTyper#type_of`'s
  `PRISM_DISPATCH`: value leaves route to the existing `rigor_infer::Typer`
  through an exact-span Prism→arena map WITH a kind-compatibility gate;
  structural handlers compose at the tier level with Bot-absorbing unions.
  Scope envs (toplevel + each def/class/module body) bind only straight-line
  unconditional writes, with a NEVER-BIND taint over
  `rigor_infer::collect_flow_writes` (conditional/compound rebinds AND
  in-place mutator calls) — the flat-env discipline that keeps every read at
  or below the reference's flow-joined precision.

## The sound-subset invariant, verified at NODE level

The PR #33 acceptance review proved histogram-level auditing insufficient: a
file's aggregate can be strictly lower than the reference's while still
containing individual nodes typed MORE precisely (mastodon user.rb netted 3
over-claimed nodes under a globally lower ratio). Method now used:

- Both tools dump one `start end tier` line per counted node in DFS pre-order
  (rigor-rs: the internal `RIGOR_COVERAGE_NODE_DUMP=1` stderr hook, run
  `--workers=1`; reference: a dumper over `PrecisionScanner`/`ScopeIndexer`/
  `NodeWalker` internals). Since both walks traverse the identical Prism tree
  in the same order, lines align positionally; spans are asserted equal as the
  alignment check.
- Over-claim := a node where rigor-rs's tier is in the precise set
  (constant/nominal/shaped/refined/bot) while the reference's is not.

Result (post-fix, all three targets):

| target | nodes | equal | coarsen (ref precise → rs dyn) | shuffle (precise↔precise) | dyn-drift | **over-claims** |
|---|---|---|---|---|---|---|
| fixtures (70 files) | 2268 | 1931 | 301 | 35 | 1 | **0** |
| conference-app/app (98) | 4235 | 4109 | 126 | 0 | 0 | **0** |
| mastodon/app/models (248) | 31381 | 29310 | 2040 | 24 | 7 | **0** |

**Correction of record**: the first submitted version of this slice claimed
"0 over-claims" from per-file precise-count/dynamic-top comparisons only. At
node granularity that claim was FALSE — 8 fixture nodes + 7 mastodon nodes
were over-claimed, from four defects all since fixed:

1. **Stale straight-line binding** (the review's blocker): a conditional or
   compound reassignment after `x = 5` left the Constant bound — in def
   bodies AND the toplevel env (`Typer::build_toplevel_env` does not widen
   either, contrary to the review's aside; harness fixture 34's toplevel `na`
   was the witness). Fixed by the never-bind taint; regression-tested in five
   forms + the mastodon user.rb extract.
2. **Missing mutation widening**: `statuses_to_query << id` left a `Tuple[]`
   binding pinned, folding a later `.empty?` predicate (mastodon report.rb).
   Fixed by tainting from the substrate's `collect_flow_writes` (now `pub`),
   which records mutator calls on bare-local receivers.
3. **Span-collision mistyping**: `where(domain:)`'s shorthand keyword-hash
   lowers to an arena `HashLit` at exactly the Prism value node's span; the
   span map handed the wrong node's type out (mastodon account.rb + 3 more).
   Fixed by the arena kind-compatibility gate.
4. **Over-permissive constant gate**: `SourceIndex::knows_class` registers
   nested classes by short name, typing a toplevel `Inner` read/`Inner.new`
   nominal where Ruby's lexical lookup (and the reference) resolves nothing
   (fixtures 68/69). Fixed with a lexical-visibility check over
   `rigor_infer::lexical_scopes` + declaration-position header spans; the
   `.new` call guard matches the reference's PERMANENT Ruby-scoping semantics
   (AGENTS.md guard-exception 2), not a transient inference gap.

The one previously-documented contrary-direction case (fixture 27/28's
nilable-return side-channel) is also CLOSED: the scanner now binds the honest
`C | nil` union from `CoreIndex::method_return_nilable` — reads stay nominal
(worst member), dispatches on the local decline to dynamic, both matching the
reference.

## Measured parity (whole-output)

| target | result |
|---|---|
| harness fixtures (70 files) | **31/70 byte-exact** (json AND text); all divergences are tier-only, node-level all coarsen/shuffle (table above), 0 unexplained |
| conference-app/app | denominators + file list identical; 49/98 files tier-divergent; ratio ref 0.3922 vs rs 0.3625 |
| mastodon/app/models | denominators + file list identical; 223/248 files tier-divergent; ratio ref 0.4911 vs rs 0.4261 |

Switch checks (re-run post-fix): `--workers=1/7/absent` byte-identical (json +
text, 248 files); `--threshold` exits equal to the oracle (0.5→0, 0.9→1 on
fixture 01; 0.4→0, 0.5→1 on mastodon); exit 64 + the reference's exact stderr
on a bad path; exit 2 for the deferred `--protection`/`--mutation` family
(ADR-63/70); MCP `coverage` tool byte-identical to CLI `--format=json`
(unit-asserted).

## Divergence attribution (all divergences are UNDER-claims or shuffles)

- **A. Implicit-self / Kernel call returns** — the reference types `puts`→nil,
  `private`→Constant; rigor-rs's implicit-self dispatch deliberately declines
  all but the ported `p`/`pp` fold. The dominant family everywhere.
- **B. Flow-substrate facts (ADR-0022 deferral)** — branch-join rebinds, loop
  convergence, ivar/cvar/global accumulators, `$~` narrowing, interprocedural
  literal tails; plus the flat-env taint's own conservatism (a read BEFORE a
  conditional reassign widens where the reference keeps the pre-branch value).
- **C. Constant-fold chain depth** — mid-chain declines
  (`"s".downcase.strip.reverse` stops at Nominal[String]).
- **D. `raise` → Bot** — the reference types `raise` calls Bot; rigor-rs
  leaves them Dynamic.
- **E. Shape/refined carriers** — RBS-generic elaborations and `Type::Refined`
  narrowing the port has not built.

Unexplained diffs: **0**. Over-claims: **0** at node granularity.

## Gates (all green, re-run post-fix)

- `cargo build --offline` + `cargo test --offline --workspace` (827 tests;
  includes the 5-form conditional-reassign regression + user.rb extract)
- `ruby harness/run.rb`: PASS, 0 unregistered FP, 216/218
- `ruby harness/run_snapshot.rb`: PASS
- `python3 harness/docs_check.py`: PASS
- `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace -- -D warnings`: clean

## Repro

```sh
# oracle (hardened), from a clean temp cwd, absolute paths:
ruby -I <repo>/reference/rigor/lib -I <repo>/reference/rigor/plugins/rigor-rbs-inline/lib \
  <repo>/reference/rigor/exe/rigor coverage <abs-path> --format json
# rigor-rs (node-level dump for audits):
RIGOR_COVERAGE_NODE_DUMP=1 <repo>/target/release/rigor coverage <abs-path> --workers=1 --format json
```
