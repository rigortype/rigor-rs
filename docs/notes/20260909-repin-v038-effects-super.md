# `super` is a dispatch — the effects half of the `v0.3.8` re-pin (#446 / PR #453)

2026-09-09. Branch `upstream-pin-v0.3.8`. Everything measured against the
PINNED submodule at `v0.3.8` (`ffb456b0`), populated into a worktree from the
parent checkout at the pin (never the network, never `REFERENCE_RIGOR_DIR`),
invoked as
`ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib …`
from the project directory each measurement names, with `.rigor/cache` cleared
either side of every oracle run. The two real projects were COPIED into a temp
project with a synthesised `.rigor.yml`, exactly as `harness/effects_diff.py`
does; the user's checkouts were only ever read.

**Headline: `python3 harness/effects_diff.py` went from `OVER=16` to `OVER=0`,
and the fix is NOT the one the upstream diff suggests. Resolving the ancestry
would have closed only 11 of the 15 mastodon rows — the other 4 are supers that
DO resolve, onto a parent that is itself tainted. The port taints at every
`super`, and the whole measured cost of that is 20 rows across 35,555 methods.**

## 1. What upstream shipped

`v0.3.5` (#446, PR #453) made `super` contribute. `UnitScan#visit_super`
(`unit_scan.rb:249`) sets `@delegates_upward` and emits a
`FileCollection::Edge` carrying the **enclosing unit's own class and selector**
with `super_call: true` — a field rather than a convention, because the
propagator asks a different question of it than of every other edge
(`file_collection.rb:34-42`). `Propagator::Index#super_targets`
(`propagator.rb:249`) resolves it with `resolve_super`, which walks the
ancestry **above** the owner class (the class's `include`s first, then its
superclass chain, with the class itself pre-seeded into `seen`) and
deliberately **omits the closed-world override join** every ordinary edge gets:
`super` in `C#m` never reaches `D#m` for a subclass `D`. When nothing answers,
`taint_unresolved_super` (`propagator.rb:71`) writes
`["unresolved-super", selector]` into the propagator's SEED, so the fixpoint
carries it to the method's callers like any other cause. `TaintCause::ALL` grew
from ten members to eleven.

## 2. The port had a live OVER, and the pin exposed it

`crates/rigor-cli/src/effects/collect.rs` ignored `super` entirely: neither
`SuperNode` nor `ForwardingSuperNode` had an arm, so the walk descended past
them and the unit claimed exhaustiveness across an unread parent body. Against
the `v0.3.8` oracle that is the unsafe direction of ADR-0043 § 2's exhaustive
row, and the standing gate said so:

```
TOTAL  MATCH=5523  UNDER=2028  OVER=16  DECLARED-MISMATCH=0
RESULT: FAIL — the port may never claim an effect the oracle does not prove.
```

`harness/effects-corpus/03_taint`'s `Taint::Ghost#method_missing` (a bare
`super` in a `method_missing`) and 15 mastodon methods, all 16 of them
"claims exhaustiveness the oracle does not".

## 3. The measurement that chose the design

The task's own instruction was to emit a `super_call` edge and resolve it, and
to fall back to an unconditional taint only if the port's propagator cannot
resolve ancestry — after measuring. Both halves of that measurement came out
against resolution, and the second one decisively.

**(a) The port has no ancestry to resolve against, and its propagation is not a
fixpoint.** `Scanner::record_declaration` treats `include` / `prepend` as
no-ops and `walk_namespace` records no superclass; there is no `superclasses`
or `includes` table anywhere in the module. The port's transitive bit is
`Summary::exhaustive`, a single set test —
`causes.is_empty() && edge_selectors.is_disjoint(selectors)` — where
`selectors` is every selector the run collected. So even a RESOLVED super
would taint under the existing edge machinery, because a super's selector is
the enclosing unit's own name and that name is always in the run's selector
set. Resolution buys exactly nothing until slice 4's real closure exists.

**(b) Resolving would have left 4 of the 15 mastodon OVER rows standing.** Of
the 15, **11 carry the oracle's own `unresolved-super`**:

| row | oracle causes |
|---|---|
| `Auth::RegistrationsController#create` / `#edit` / `#new` | `unresolved-super(create/edit/new)` |
| `Authorization#authorize` | `unresolved-super(authorize)` |
| `GroupedNotificationsPresenter#initialize` | `unresolved-super(initialize)` |
| `OAuth::AuthorizationsController#mfa_setup_path` | `unresolved-super(mfa_setup_path)` |
| `Poll::Option#initialize` | `unresolved-super(initialize)` |
| `PostStatusService::UnexpectedMentionsError#initialize` | `unresolved-super(initialize)` |
| `Status#cache_key` | `unresolved-super(cache_key)` |
| `TranslationService::DeepL#initialize`, `…::LibreTranslate#initialize` | `unresolved-super(initialize)` |

The other **4** carry no `unresolved-super` at all —
`ActivityPub::FetchAllRepliesService#call` and the three
`…#supported_context?` — because their `super` RESOLVES into the project and
the parent it lands on is itself `dynamic-receiver`-tainted. A rule that taints
only the unresolvable supers is therefore not even sufficient for the gate; the
blanket rule is not merely the fallback, it is the only rule available without
the transitive closure.

**(c) The cost is 4 rows on mastodon, and 2 of them were already lost.** A
Prism walk over mastodon/app's 6,664 units finds **94 containing a `super`**;
the oracle calls exactly **4** of them exhaustive (`HomeFeed#initialize`,
`LinkFeed#initialize`, `ListFeed#initialize`, `TagFeed#initialize` — all
`super(account, options)` into `PublicFeed#initialize`, which is clean). The
port already tainted two of them for other reasons, so the blanket rule costs
**2 rows** there. The other 77 super-bearing rows the port already tainted.

## 4. What was built

`UnitScan` gained the unit's own selector (upstream reads `@method_name` at the
same site), and `visit_construct` gained one arm:

```rust
if node.as_super_node().is_some() || node.as_forwarding_super_node().is_some() {
    return self.visit_super();
}
```

`visit_super` taints `("unresolved-super", Some(selector))`. It runs on the
`visit_branch_node_enter` hook — ruby-prism dispatches BOTH node types through
it — so the walk still descends into the `super`'s own arguments and block and
their origins are collected, exactly as upstream's `walk` does. `super` in a
nested `def` belongs to the nested unit for free, because `visit_def_node`
records the def rather than descending into it; a `define_method(:d) { super }`
lands on `C#d` for the same reason. The cause constant joins the four already
there; nothing else in the port enumerates causes.

## 5. `harness/effects-corpus/11_super`

25 units. `Shapes` is the half that BITES: none of its selectors exists on the
parent, so the oracle taints and an engine whose walk never reached the `super`
claims exhaustiveness the oracle does not — an OVER, not a silent UNDER. Its
block hosts are `File.open` / `Dir.glob` rather than `[1, 2].each`, because a
literal receiver taints the port for an unrelated reason and would have masked
the row. `Resolvable` is the other direction and pins the ORACLE's rows in
comments: a superclass parent, an `include`d module parent and a singleton
parent, each contributing its label with `exhaustive: true`. `Orphan` is the
unresolvable case where both engines' cause strings coincide exactly.
`Nesting` pins that a nested `def`'s `super` is the nested unit's, and
`NoSuper` is the no-`super` control.

```
=== harness/effects-corpus/11_super ===
  oracle=25 methods / 15 proven labels   rigor-rs=25 / 10
  MATCH=19  UNDER=6  OVER=0  DECLARED-MISMATCH=0
  UNDER by kind: {'missing-label': 5, 'extra-taint': 1}
      snapshot: MATCH=7  UNDER=3  OVER=0  DECLARED-MISMATCH=0  unresolved-only=0
```

The 6 UNDER are the 6 `Resolvable` rows — the exact shape § 3(c) prices.

## 6. Numbers

`python3 harness/effects_diff.py --show 40` (default set; `--self-test` PASS on
all 12 projects both before and after). Per-project OVER, and the TOTAL lines:

| project | OVER before | OVER after |
|---|---|---|
| `01_core_origins` … `02_propagation` | 0 | 0 |
| `03_taint` | **1** | 0 |
| `04_declared` … `10_declared_plugins` | 0 | 0 |
| `11_super` (new) | **11** | 0 |
| `mastodon/app` | **15** | 0 |
| `gitlab-foss/lib` (`--scale`) | **77** | 0 |

```
before  TOTAL  MATCH=5523  UNDER=2028  OVER=16  DECLARED-MISMATCH=0
        SNAPSHOT TOTAL  MATCH=575  UNDER=365  OVER=0  DM=0  HEADER-MISMATCH=0  unresolved-only=632
after   TOTAL  MATCH=5555  UNDER=2036  OVER=0   DECLARED-MISMATCH=0
        SNAPSHOT TOTAL  MATCH=570  UNDER=375  OVER=0  DM=0  HEADER-MISMATCH=0  unresolved-only=637

--scale (adds gitlab-foss/lib, 28,607 oracle methods)
before  TOTAL  MATCH=26337  UNDER=9780  OVER=104  DECLARED-MISMATCH=0
after   TOTAL  MATCH=26397  UNDER=9801  OVER=0    DECLARED-MISMATCH=0
```

Every `MATCH → UNDER` transition, computed with `effects_diff.compare`'s own
lane logic. All 20 are `under:extra-taint`, and every one is a row whose oracle
summary is EXHAUSTIVE with **no causes at all** — i.e. a `super` the oracle
resolves onto a clean project parent, which is § 3(c)'s priced case and not the
`unresolved-super`-tainted rows the task expected:

- mastodon/app (2): `LinkFeed#initialize`, `TagFeed#initialize`
- gitlab-foss/lib (18): `Backup::Targets::Files#initialize`,
  `Backup::Tasks::Repositories#initialize`,
  `Banzai::Filter::References::IssueReferenceFilter#reference_class`,
  `Banzai::Filter::References::MergeRequestReferenceFilter#reference_class`,
  `BitbucketServer::Representation::Comment#initialize`,
  `Gitlab::Checks::ContainerMoved#initialize`,
  `Gitlab::Ci::Build::Context::Build#initialize`,
  `Gitlab::CycleAnalytics::Summary::DeploymentFrequency#initialize`,
  `Gitlab::Database::Aggregation::ActiveRecord::DimensionDefinition#initialize`,
  `…::FilterDefinition#initialize`, `…::PartDefinition#initialize`,
  `Gitlab::Database::Migrations::TestBatchedBackgroundRunner#initialize`,
  `Gitlab::GitAccessSnippet#initialize`,
  `Gitlab::Metrics::WebTransaction#initialize`,
  `Gitlab::ReferenceExtractor#initialize`,
  `Gitlab::ReferenceExtractor#reset_memoized_values`,
  `Gitlab::Template::Finders::GlobalTemplateFinder#initialize`,
  `Security::CiConfiguration::ContainerScanningBuildAction#comment`

No fixture project lost a MATCH; `03_taint` gained one (8 → 9) and `11_super`
went 9 → 19.

## 7. `rigor check` parity — ADR-0043 § 1

Unmoved, and verified by measurement rather than by argument: the collector
change was stashed, the debug binary rebuilt, and `ruby harness/run.rb` run on
both sides.

```
both arms:  105 fixtures / 538 reference / 488 rigor-rs
            matched 487   gaps 50   registered 0   UNREGISTERED 0
            PASS — no unregistered false positives
```

`ruby harness/run_snapshot.rb` agrees (same six numbers). `cargo test
--workspace --offline`: 1,303 passed / 0 failed. Clippy in a fresh
`CARGO_TARGET_DIR`: clean under `-D warnings`.

## 8. Residues

- **The 20 resolvable-super rows are a slice-4 debt, not a bug.** Closing them
  needs the project ancestry (`superclass` + `include`, merged across
  reopenings, resolved through `Scanner#lexical_candidates`' as-written
  candidate lists) AND the real transitive closure, because a resolved super
  must carry the parent's bit, not merely its identity. Half of that subsystem
  alone would move nothing: § 3(a).
- **The port's cause string differs from the oracle's on the 4 resolved-but-
  tainted rows** (`unresolved-super(x)` where the oracle names the parent's own
  causes). That reaches only the snapshot's `unresolved:` COUNT, which
  `compare_snapshots` grades as `UNRESOLVED-ONLY` — never fatal, never MATCH —
  and those rows are `under:extra-taint` anyway, which is checked first.
  Snapshot `unresolved-only` moved 632 → 637 on mastodon for this reason.
- **`effects_diff.py --self-test` FAILS on `master`** and passes on
  `upstream-pin-v0.3.8`: the schema-2 `unresolved:`-as-a-count change (#434)
  landed in `a38007a`. A worktree branched off `master` rather than off the
  re-pin branch reads the whole snapshot surface as INVALID, and INVALID scores
  `OVER=0` — the "gate that lies" shape this repo keeps rediscovering. Check
  what the worktree is BASED on before quoting a green effects run.
- Not probed here: whether `@delegates_upward` has any port-side consumer. It
  feeds upstream's framework-unit harvest (`scanner.rb:131`), which is the
  plugin stratum and out of ADR-0043 entirely.
