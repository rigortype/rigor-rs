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

## Second re-review round: residual rebind channels + gitlab-foss lib

The PR #33 re-review flagged two residual channels; fixing them surfaced (via
a gitlab-foss lib node audit, 4676 files / 624,233 nodes — denominator exact
there too) several more. All fixed classes, with witnesses:

5. **Un-modeled rebind forms** — multi-write (`x, y = f, 2 if c`), `for x
   in …` index, `rescue => x` captures: none produce arena flow-writes, so a
   straight-line binding survived them. Fixed with Prism-side taints in
   coverage.rs (per review direction, NOT by widening the substrate's
   `collect_flow_writes`). Plus, found by the same audit: index-writes
   (`h[:k] += v` — gitlab metrics_interceptor.rb) and block-parameter
   SHADOWING (`with_object(iterator) do |text, iterator|` — gitlab
   gitmodules_parser.rb) taint too.
6. **Composite entry-scope semantics** — the reference types every walked
   node (and its handler's whole recursion) under the ONE scope recorded AT
   that node. Generalized `eval_ctx` (pinned env at the walked node's start,
   own-span scope/lexical entries excluded) replaces the wrapper-only context:
   fixes the class/module wrapper tier (`module M; x = 5; x; end` → wrapper
   dynamic, headline 80.0% not 100.0%), the reopened-module constant
   resolution, and `@f ||= begin; present = …; present - w; end` (the
   composite must not see bindings established inside itself).
7. **Untrusted composite bindings invalidate, never bind** — the Typer's
   composite arms type to the LAST-child constant (an interpolated-symbol
   branch yields the factually wrong `Constant["_z"]`); the walker collector
   also missed nodes reached via CONCRETE-typed Prism fields (RescueNode et
   al — the generated `Visit` bypasses the enter hooks for those). Both
   fixed; a declined bind still pushes an untyped INVALIDATION event so a
   later read cannot fall back to a stale earlier binding (gitlab
   api/helpers.rb `messages`).
8. **`.new` gate completed** — applied in the BIND pass too (the Typer's
   `.new` interception is unconditional: `Group.new` types `Group` with no
   Group in scope), and extended to core classes without an RBS-declared
   singleton `new` (`Integer.new` is a NoMethodError — gitlab
   template_parser/ast.rb).
9. **Dispatch fidelity** — `__FILE__` → non-empty-string (nominal, was
   constant), `__LINE__` → positive-int (shaped, was constant), backtick
   XString → nominal (was constant), embedded `#{…}` → its statements' value
   structurally (was a span-map collision minting Nominal[String]).

**Residual over-claims: 27 nodes on gitlab-foss lib (0 on the other three
targets), all enumerated, all PROVABLY SOUND extra precision** — method-chain
returns that are unconditional in RBS and cannot be otherwise at runtime:
`File.join/dirname/basename(…) → String` (12), `Integer#to_s(36)` chains the
reference loses at its own IntegerRange carrier (5), `Array#-`/`select{}` +
`join → String` (6, incl. their `#{}` wrappers), `String#*`/`+` (1), plus 3
wrapper echoes. The reference declines these on argument-dependent dispatch
or its own carrier gaps — transient reference gaps, not permanent semantics.
Per AGENTS.md (generative-tool parity), sound extra precision is NOT guarded:
encoding transient reference gaps is anti-convergence. This is the principled
line drawn: every FACTUALLY-WRONG claim class found by any audit round was
fixed (stale bindings, bogus constants, span collisions, lexically-invalid
resolutions — types a value cannot have); types a value provably HAS are kept
and enumerated here.

Reviewer-verified addendum (final acceptance pass): each family sampled at
real gitlab source lines against the reference binary — no sampled site's
extra precision can be wrong at runtime. One named member of the wrapper-echo
set: `click_house/migration_support/migrator.rb:150`, rs `Constant[nil]`
where ref `bot` (the reference's own fixpoint artifact pins `attempts=1`;
rigor-rs is the more correct side; both tiers are precise-numerator so ratios
are unaffected).

Metric scope note: nominal-where-ref-refined/shaped RANK inversions (e.g.
`Nominal[String]` where the reference keeps a literal-string or a shaped
`map` carrier) are deliberately excluded from the over-claim metric — they
are semantically coarser-or-equal claims that never cross the precise/opaque
boundary, never move a ratio, and are never claims a value cannot satisfy.

Final node-audit table (strict metric: precise-where-ref-dynamic OR
constant-where-ref-weaker-precise):

| target | files | nodes | over-claims |
|---|---|---|---|
| harness fixtures | 70 | 2268 | **0** |
| conference-app/app | 98 | 4235 | **0** |
| mastodon/app/models | 248 | 31381 | **0** |
| gitlab-foss/lib | 4676 | 624233 | **27 (all sound, enumerated above)** |

Whole-output fixtures after this round: 30/70 byte-exact (fixture 56 joined
the divergent set — the rescue-taint/composite rules under-claim a few more
nodes; node-audit-verified all-coarsen). Ratios (ref/rs): fixtures
0.8386/0.679, conference-app 0.3922/0.3629, mastodon 0.4911/0.4261,
gitlab-foss lib rs 0.4364.

Substrate finding flagged for separate triage (not this slice): the Typer
types `x = if c; :"a#{b}_z"; …` to `Constant["_z"]` (interpolated symbols
lower to a `Statements` wrapper; Statements types as its last child) — a
potential check-pipeline hazard, spawn-tasked.

## Gates (all green, re-run post-fix)

- `cargo build --offline` + `cargo test --offline --workspace` (833 tests;
  incl. the 5-form conditional-reassign regression, the user.rb extract, and
  the 6 rebind/wrapper witnesses from the re-review rounds)
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
