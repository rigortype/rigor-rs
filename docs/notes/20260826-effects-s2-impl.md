# Effects slice 2 — DIRECT summaries: implementation note

2026-08-26. Implements [the mini-spec](20260826-effects-s2-mini-spec.md) over
[the probe](20260826-effects-s2-probe.md), whose § 3 mechanics and § 5 verdict
table were normative. Branch `claude/effects-s2-direct`.

**Result: the differential lands on the predicted table exactly —
`MATCH=12 UNDER=34 OVER=0 DECLARED-MISMATCH=0` → PASS**, per corpus
01: 3/13/0/0 · 02: 4/11/0/0 · 03: 5/6/0/0 · 04: 0/4/0/0, with the UNDER-by-kind
breakdown also matching the probe's § 6b prediction row for row. Not higher: a
higher MATCH would have been an over-reach to re-derive, not a win.

## 1. What shipped

| piece | where |
|---|---|
| the Prism-walking collector (unit identity + one body) | `crates/rigor-cli/src/effects/collect.rs` |
| the six narrowing handlers | `crates/rigor-cli/src/effects/narrowing.rs` |
| `LocalOwnership` + `MutationClassifier` | `crates/rigor-cli/src/effects/ownership.rs` |
| `rigor effects [--full] [--format=json]` + the annotation self-defense | `crates/rigor-cli/src/effects/mod.rs` |
| the mutator sets, EXTRACTED from the pinned Ruby | `harness/vendor_effects.py` → `crates/rigor-effects/vendor/effects/mutators.yml` + `src/mutators.rs` |
| `Catalog::resolve` (the un-collapsed row) + mutator-set expansion | `crates/rigor-effects/src/catalog.rs` |

`rigor-cli` now has a path dependency on `rigor-effects`. ADR-0043 § 1 stays a
dependency-graph fact from the other side: `rigor-effects` depends on no crate
of ours, and the collector lives outside `rigor-infer` / `rigor-rules`, so
nothing on the `check` path can reach it. The edge re-entered `rigor-effects`
into the stale-binary scan with no harness edit — the prose at
`harness/effects_diff.py:80-83` was updated in the same commit to say so.

## 2. The four grading traps, and what each cost

1. **Exhaustiveness** — `"exhaustive": false` everywhere. `causes` carries an
   out-of-enum `["port-incomplete", "taint is ADR-0043 slice 3"]` marker; the
   grader never reads it, and borrowing a real `TaintCause` would have been a
   claim nothing computed. Cost: 27 of the 34 UNDERs are `extra-taint`.
2. **Declared** — `"declared": []` everywhere, plus the self-defense: a project
   whose `.rigor.yml` carries `effects.attribution:`, or whose `sig/**/*.rbs` or
   own `.rb` sources match upstream's `SignatureSources::ANNOTATION_HINT`,
   reports `"methods": {}`. That is what turns 04_declared's fatal
   DECLARED-MISMATCH into 4 clean `absent-method` UNDERs **without the gate
   command having to remember to skip it**.
   **The must-still-fire control is `harness/effects-corpus/01_core_origins`
   itself**: the same binary, same run, reports 16 real methods there. Both
   directions are also pinned as unit tests
   (`the_annotation_hint_matches_the_two_spellings_upstream_honours` asserts the
   two spellings match AND that `%a{purely}` / `%a{rigor:v1:effects …}` /
   `%a{assert Foo}` do not). Suppressions without controls are how this repo got
   burned three times; this one has both.
3. **Proven = raw string subset, not label subsumption** — the six handlers are
   implemented and `Catalog::lookup` is never consumed for a narrowed row. The
   seam is `Catalog::resolve`, which returns `Row` / `Universal` / `Posture`
   un-collapsed so the caller branches on `Row::narrow()` itself;
   `every_narrowed_row_is_reachable_as_a_row_through_resolve` pins that all
   seven narrowed rows answer through the `Row` arm and that exactly seven
   exist. Where the port cannot answer (only `sql_verb`, which no `core.yml` row
   names) it answers **∅ and still CLAIMS the call** — never the parent label.
4. **The posture tier** — sidestepped by taking only upstream's first two
   `catalog_target` arms (implicit self, constant path) and declining
   `record.receiver_class`. No Typer, no `SourceIndex`, no inference state is
   read anywhere in `crates/rigor-cli/src/effects/`.

## 3. The one place the subset argument did NOT hold — measured, not reasoned

The spec's shape was "our handled-target set ⊂ upstream's ⇒ only UNDER". That is
true of the CATALOGUED path. It is **false of the mutation judgment on the
UNCATALOGUED path**, and the direction is the dangerous one: upstream's answer
gets *narrower* when the catalogue claims the call, because a ROW is
authoritative and may say "this is not a receiver mutation".

Probed against the oracle (`p_mut`, a scratch project with a `sig/` typing one
parameter as `Thread`):

```
Probe#typed_thread   eff=['global.write']     direct={'catalogue:Thread#[]=': ['global.write']}
Probe#typed_hash     eff=['mutate.instance']  direct={'construct:receiver-mutation': [...]}
Probe#untyped_param  eff=['mutate.instance']  direct={'construct:receiver-mutation': [...]}
Probe#env_write      eff=['global.write']     direct={'catalogue:ENV#[]=': ['global.write']}
```

`t[:k] = 1` on a `Thread` proves `global.write` and **not** `mutate.instance`.
A no-typer port falls to the uncatalogued path there, where `[]=` mutates every
receiver, and would have emitted `mutate.instance` — an `OVER`. So the collector
**suppresses `[]=`-shaped selectors that some catalogued ROW answers as a
non-mutation**, derived from the shipped catalogue rather than listed
(`NON_MUTATING_ROWED_SELECTORS`; at the pin it is
`["[]=", "default_external=", "default_internal="]`, from `ENV#[]=`,
`Thread#[]=` and `Encoding`'s two singleton writers).

Cost, measured on the same probe: `typed_hash`, `untyped_param` and
`owned_local` lose their `mutate.*` (three UNDERs, 0 OVER). Zero cost on the
graded corpus, which contains no `[]=` mutation at all. Three things stay exact:

- the **compound** writes (`x[0] += 1`, `x.foo ||= 1`) — upstream classifies
  those from the NODE TYPE and never consults the catalogue, so both engines
  read pure syntax;
- **attribute writers** (`x.name = 1`) — no catalogued row answers one as a
  non-mutation, so the derivation leaves them alone and a future upstream row
  would join the suppression set automatically;
- a **constant** receiver (`ENV["k"] = "v"`) — the catalogued path, identical
  on both sides.

## 4. Composition probes (gate 2) — run BEFORE the differential

The differential grades 6 of the catalogue's 420 rows. Two scratch projects were
measured against the ORACLE (pinned submodule at `b10bd5df`, `.rigor/cache`
cleared either side of every run) and compared to the port's JSON **per ORIGIN
BUNDLE**, which is strictly stronger than the differential's flat proven lane —
a right label filed under a wrong origin fails here and is invisible there.

| probe | methods | result |
|---|---|---|
| `p_units` — unit identity, all 11 construct spellings, all six handlers, the posture / universal / implicit-self controls | 55 | **AGREE=55 DISAGREE=0** |
| `p_mut` — the mutation hazard of § 3 | 6 | **AGREE=2, 4 under, 0 OVER** (all four are § 3's designed suppression) |

Handler-by-handler, port == oracle: `file_open` absent mode / `"w"` / `"a"` /
`"wb"` / `"r:UTF-8"` / `mode:` keyword / `"r+"` → `[io.fs.read, io.fs.write]` /
`File::RDWR` → `io.fs` / computed → `io.fs`; `pathname_open` (shifted arity);
`kernel_open` `"|ls"` and the leading literal run of `"|#{cmd}"` → `io.process`,
computed → `io`; `uri_open` http / file / other-scheme / bare path / computed;
`time_new` and `random_new` bare → label, positional → ∅, **`Time.new(in: …)` →
`nondet.time`** (keyword args are not positional).

**∅-not-parent fallback**: `time_new`/`random_new` with arguments and `sql_verb`
answer ∅ where upstream's contract would answer the parent. Both agree with the
oracle where the oracle narrows; `sql_verb` is unreachable from `core.yml` and
pinned as such.

Two facts the probe note did not record, both MEASURED and both now pinned as
tests rather than assumed:

- `alias $a $b` proves **`global.read` AND `mutate.static`** — the alias targets
  are `GlobalVariableReadNode`s and upstream's walk descends into them.
- a `@@cvar` used as a mutated receiver proves **`global.read` AND
  `mutate.static`** for the same reason.

## 5. Deviations from the spec, with reasons

1. **`time_new` / `random_new` count POSITIONAL arguments, not all arguments.**
   The spec offered "counting every lowered arg as positional is the UNDER-safe
   reading" — that was a concession to a port riding the LOWERED AST, where the
   keyword hash is gone. This collector walks Prism, where `KeywordHashNode` is
   a node type, so it implements upstream's `grep_v` rule exactly.
   `Time.new(in: "+09:00")` → `nondet.time`, which is a MATCH, not an OVER.
2. **The `[]=` suppression of § 3** — not in the spec, measured into existence,
   strictly UNDER-safe, and derived from the catalogue rather than listed.
3. **The report emits `direct:` as well as the four graded keys.** Ungraded
   (`effects_diff.py` reads three keys), it is exactly what slice 2 computes,
   and it is what made the per-origin composition probe possible.
4. **Both fixture COMMENT fixes were widened.** The spec named
   `owns_what_it_mutates` and `mutates_its_argument`; both comments now state
   what the oracle actually reports and why, since an implementer reading the
   old text would have built the wrong ownership rule. Fixtures unchanged — the
   self-test still reports 46/46 MATCH.

## 6. Gate results (bare)

| gate | result |
|---|---|
| `cargo test --workspace` | **PASS** — 430 + 279 + 251 + 94 + 58 + 48 + 47 + 24 + 9 + 4 + 3, 0 failed (40 new `effects::*`) |
| `CARGO_TARGET_DIR=$(mktemp -d) cargo clippy --workspace --all-targets -- -D warnings` | **PASS**, exit 0 |
| composition probes (§ 4) | **PASS** — 55/55 exact, 0 OVER on either project |
| `harness/effects_diff.py --self-test` | **PASS** — 46/46 MATCH on all four projects |
| `harness/effects_diff.py` | **PASS** — `MATCH=12 UNDER=34 OVER=0 DM=0`, per corpus 3/13 · 4/11 · 5/6 · 0/4 |
| `rigor check` byte-identity vs the master baseline, mastodon/app | **IDENTICAL** — stdout + stderr, default and `RAYON_NUM_THREADS=1`, exit 1 both arms |
| `harness/run_snapshot.rb` | **PASS** — 98 fixtures, 407 matched, 2 registered divergences, **0 unregistered** |
| `harness/fp_audit.py --gaps --sweep` | **0 FP / 9,204 files**, gap set unchanged vs the branch-point baseline |
| `harness/vendor_effects.py --check` | **PASS** — all three files match the pinned source |

## 7. Debt this slice leaves, by blocker

Unchanged from the probe's § 6c, now measured against a real port arm rather
than a synthesised one:

- **slice 3 (taint)**: 27 methods sitting at `UNDER:extra-taint`. Turning the
  bit on with everything else identical is the whole difference between 12 and
  39 MATCH.
- **slice 4 (transitive)**: 3 methods / 4 labels — `Pipeline#run`,
  `Recursive#mutual_a`, `Taint#literal_send`. The only `under:missing-label`
  rows.
- **slice 6 (declared)**: 4 methods, the whole of `04_declared`, currently
  withheld by the self-defense rather than reported wrong.

Carried forward as slice-3 work: the unit's `&blk` parameter name, dropped here
because only `opaque_callable?` and `visit_block_argument` read it and both
produce taints; and the `Span → NodeId` bridge the probe's § 5c designs, which
slice 2 provably did not need.
