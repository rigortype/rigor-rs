# Upstream survey `v0.3.1` → `80aaf9bc` (150 commits, + rbs 4.1.1) — 2026-08-07

Third run of the survey recipe
([pre-flight](20260731-v031-preflight-survey.md), [+49](20260731-head-survey-and-set-op-folds.md)).
Still **no tag past `v0.3.1`**; upstream master is 150 commits ahead and its tip
bumps rbs 4.1.0 → 4.1.1.

Headline: **150 commits and an rbs bump move TWO diagnostics across 9204 files,
and rigor-rs is silent at both.** The port's measured position — 0 FP, 1193
coverage gaps — is *bit-identical* whether the oracle is the pin or master.
**Hold the pin.**

## Method — three axes, each isolated

The gemspec is still `rbs (>= 3.0, < 5.0)` on both checkouts, so the same
decomposition works. Reference self-diffs over the standing sweep set
(`harness/sweep-corpora.yml`, 8 corpora / 9204 files), `--no-cache` + a fresh
temp cwd per invocation, each checkout's own plugin path pinned
(`UPSTREAM.md` hazards 1–2). Master is a **worktree of the pinned submodule**
at `origin/master` — the pin itself never moved.

Full 2×2 (checkout × rbs), not the 1×2 the earlier surveys ran, so an
interaction between the two axes could not hide.

> **rbs selection changed mechanism.** `GEM_HOME`/`GEM_PATH` (the earlier
> surveys' method) also changes which *other* gems resolve: it made the
> reference abort on dependabot-core with `Unable to load parser >= 3.3.7.2`,
> silently costing 1650 files / 138 870 diagnostics from the measurement.
> A load-path override (`ruby -I <scratch>/gems/rbs-4.1.1/lib -I <its ext dir>`)
> selects rbs 4.1.1 and touches nothing else — RBS resolves its own stdlib root
> relative to its lib dir, so the signatures come with it. All eight corpora
> then measure under both rbs versions.

## Axis A — upstream logic: 2 diagnostics on 9204 files

`v0.3.1` vs `origin/master`, both under the same rbs (identical under 4.1.0 and
under 4.1.1):

| corpus | files | `v0.3.1` | master |
|---|---|---|---|
| mastodon `app` | 1236 | 458 | 458 |
| gitlab-foss `lib` | 4676 | 1373 | 1373 |
| survey `mail` | 874 | 7198 | **7198** (+1 / −1) |
| survey `Ruby` | 192 | 35 | 35 |
| survey `dependabot-core` | 1650 | 138870 | 138870 |
| survey `concurrent-ruby` | 345 | 5803 | 5803 |
| survey `net-ssh` | 180 | 199 | 199 |
| survey `haml/lib` | 51 | 10 | 10 |

- **dropped** `call.undefined-method` @ rdoc `markup/pre_process.rb:161` —
  upstream #239 / `138fedd7`, the same drop the +49 survey found. rigor-rs
  silent.
- **added** `call.undefined-method` @ mail
  `spec/mail/parsers/address_lists_parser_spec.rb:32` — new. **Bisected to
  `c7f28da1` "Record Data/Struct constant declarations cross-file" (#271).**
  rigor-rs silent.

### The added one is a new upstream FP, not coverage the port owes

Minimal repro (2 files, both engines from a fresh cwd):

```ruby
# lib.rb
module Pkg
  class Parser
    Result = Struct.new(:items, :errors)
    def self.parse(text)
      r = Result.new([], [])
      text.split(",").each { |t| r.items << Item.new(t) }
      r
    end
  end
  class Item
    def initialize(name) = @name = name
    def local = @name
  end
end
# use.rb
p Pkg::Parser.parse("a,b").items.first.local
```

| | verdict |
|---|---|
| reference `v0.3.1` | no diagnostics |
| reference master | `error: undefined method 'local' for nil` |
| **rigor-rs** | no diagnostics |

`c7f28da1` was itself an FP *fix* (a nested `Data`/`Struct` constant now resolves
cross-file instead of reaching a same-named sibling). But typing the constant
also makes `Result.new([], [])` pin `items` to the **empty** literal, so
`.first` folds to `nil` and the next call errors on correct code — the array is
filled by `<<` on the next line. After a pin bump this would read as a new
coverage *gap*; it is a gap the port should not chase.

## Axis B — rbs 4.1.0 → 4.1.1: nothing, and structurally so

Reference self-diff, same checkout under both rbs versions:

| | added | dropped |
|---|---|---|
| `v0.3.1`, 8 corpora / 9204 files | **0** | **0** |
| master, 8 corpora / 9204 files | **0** | **0** |

The reason is not luck. `diff -rq` over the two gems' **entire** `core/` and
`stdlib/` trees reports **zero differing `.rbs` files**; only `lib/rbs/version.rb`
and the compiled extension differ. 4.1.1 is a library-code release, not a
signature release — unlike 4.0.3 → 4.1.0, whose 2702-line `core/` rewrite cost
the port two inference fixes.

## Axis C — port side under a 4.1.1 tree: 0 / 0

`RIGOR_RBS_CORE_DIR` pointed at a mirror built by **`harness/vendor_rbs.py`'s
own `build()` + `carry_into()`** — core/ plus the `DEFAULT_LIBRARIES` transitive
closure, **plus `overlay/`**. The overlay is the trap the pre-flight survey fell
into and it reproduces exactly here:

| tree | classes (`rigor doctor`) |
|---|---|
| embedded (4.1.0) | **660** |
| 4.1.0 mirror (control) | **660** |
| 4.1.1 mirror | **660** |
| 4.1.1 mirror, `overlay/` removed | 539 |

The 4.1.0 control mirror is diagnostic-identical to the embedded set (0 added /
0 dropped, 9204 files), so the mirror mechanism itself is neutral; the 4.1.1
mirror is then **0 added / 0 dropped** too. The recipe also self-tests:
`vendor_rbs.py <rbs-4.1.0> --check` still reports the committed tree matches its
source exactly.

`ruby harness/unbuildable_classes.rb --check` is **OK / 12 classes** in all
three environments (pin, master, and the pin under 4.1.1) — the rbs bump cannot
move that set, since it moves no signature file. (Under the `-I` override the
script's `[env]`/`[pin]` split reads 8/4 instead of 1/11: `RBS_GEM_ROOT` is
derived from `RBS::EnvironmentLoader::DEFAULT_CORE_ROOT`, which the override
repoints, so `rbs-4.1.0/sig/shims/*` misclassifies. A real bump installs the gem
and the split returns to normal. The *class set* is what the table encodes, and
it is unchanged.)

## End-to-end: what a pin bump would buy

rigor-rs (embedded, unchanged) against each reference set, same comparison
`fp_audit.py` makes:

| | FP | coverage gaps |
|---|---|---|
| vs reference `v0.3.1` (today's pin) | **0** | **1193** |
| vs reference `origin/master` | **0** | **1193** |

Bit-identical, corpus by corpus. The +1 / −1 both land in reference-only
territory, so even mail's gap count (540) does not move.

## Triage of the 150 commits

### Must follow (parity-relevant, default profile) — all already satisfied

- `138fedd7` (#239) instance/class same-name — measured drop; rigor-rs silent.
- `c7f28da1` (#271) nested Data/Struct constants cross-file — measured add;
  rigor-rs silent, and the add is an FP (above).
- `9980ef8f` (#286) branch elision no longer rests on an optimistic carrier
  (+ censuses `acf843a0` / `bd647446` / `7f7fcdd0` / `0f7ba8a1` / `75dda4f2`).
  Upstream's own FP: it deleted the branch a missed `Hash#[]` / `Array#first`
  lookup takes. Probed 6 shapes (`n = if MAP[key] then 1 else "none" end`, the
  `return unless handler` guard, `x ? x : 0`, the `unless v then v = …` refill):
  the pin fires `undefined method 'upcase' for 1`, master and **rigor-rs are
  silent**. Nothing to port — but the constraint is worth recording: the port
  has **no** `implicitly-returns-nil` handling at all (0 hits in `crates/`), so
  if it ever ports the elision it needs upstream's `OptimisticOrigin` side
  channel in the same slice, or it inherits the bug.
- `4bc2767d` (#277) widen every variable a mutation receiver can select — 0
  corpus movement, and no minimal probe of the ternary / `if` / `||` receiver
  reproduced a firing on *any* of the three engines.
- `d7105ee6` open hash shape → undeclared key is `untyped` — upstream states no
  shape it infers is open, and `check` is unaffected. Inert.
- `1416111b` (#230) `Resolv#initialize` core overlay — the only `data/` change in
  150 commits. By its own header it is a backport for rbs **< 4.1**; on 4.1 the
  overload duplicates upstream's. The port vendors 4.1.0. Behaviour-neutral.
- `a9b475f0` / `fdd8b621` invalid-UTF-8 RBS quarantine — robustness, no
  diagnostic-set effect on well-formed corpora.
- `2a5819c0` evaluates widening the `&&`/`||` value-polarity gate and **declines**
  it. A decline costs the port nothing.

### Can ignore

Plugins (no plugin engine): `2c1ece4a` `dry-validation.rule-key-mismatch`,
`2e467d1d`, `49ff1d3a`, `951b2e7e`. Inline-RBS (ADR-0035, and `info` severity is
outside the parity set): `2ce7655c`, `e9818a62`, `10ed058d`, `3a3ffadc`.
`coverage --protection --mutation` Tier 1/2 — by volume the largest block of the
150 and entirely unported: `5340a980`, `8c18e5cd`, `4e49f386`, `a656fe05`,
`63011de5`, `683bbe76`, `e4bdb1fe`, `05e2fa3d`, `01b0b7db`, `d1567ec7`,
`3962c55e`, `49b2af6d`. Cache identity: `dc5eefbb`, `ae06ffcd`, `82db67d8`.
`parameter_inference:` stays opt-in and upstream **again** declines the default
flip (`14cca5f4`, `a9546682`, `b1e98fb8`). Perf/alloc (`edb48efa`, `278f60b0`,
`fe286b37`, `684e00b3`) and ~35 handoff/changelog/doc commits.

LSP is its own arc, not a parity obligation, but two commits are candidates:
`9594732b` (#246, republish every open buffer whose text matches disk on save)
and `24d839af` (#142, publish N dirty buffers across a fork pool). Note the port
already **beat** the reference on config reload
([note](20260801-lsp-config-reload.md)).

### New precision worth porting ahead of a tag

The `#121` deterministic-fold family continues: `4dbe5a35` `Tuple#first(n)` /
`#last(n)` to the precise sub-Tuple, `49db0e21` `freeze`/`itself`/`dup`/`clone`
keeping a literal's value, `0851b0a7` `Integer#rationalize` / `abs2`,
`13e1540c` `Regexp.union` / `linear_time?`, `622c9d61` + `fb4aca11` `URI`
form-encoding folds. `4dbe5a35` is the smallest and rides the same value-pinned
`Tuple` carrier the already-ported set-op folds (`a2867efd`) landed on — a
plausible follow-up slice. **But measure first:** upstream reports 0 added / 0
removed for `49db0e21` across twenty projects, and this survey measures 0
movement for the whole family. Per AGENTS.md, do not build a coverage slice
without an `fp_audit --gaps` prediction that it closes gaps. Left for a
follow-up; nothing built here.

## Pin recommendation: **hold at `v0.3.1`**

`UPSTREAM.md`'s convention is to pin a tag, and there is still no tag. 150
commits and an rbs bump *looked* like the materially different situation that
would override the convention; measured, it is not:

- The rbs axis, which is what forced the early `v0.3.1` move, **does not exist
  here**: 4.1.0 and 4.1.1 ship byte-identical `core/` + `stdlib/`, so the
  re-vendor is a no-op diff.
- The v0.3.0-RC era bumped to a *commit* because the RC carried a rule surface
  the port needed (`suppression.unknown-marker`, the Kernel intrinsic fold).
  Master carries **no new rule in the parity set** — its one new id,
  `source-rbs-annotation-not-honoured`, is `info` and inline-RBS.
- The bump buys 0 FP → 0 FP and 1193 gaps → 1193 gaps.
- Master is an active development tip: a "replace the handoff" commit lands
  every few commits, and #286 is still being reconciled in-tree
  (`dfdced55`, `75dda4f2`, `7f7fcdd0` all correct the same handoff). Pinning to
  a moving tip re-imports that churn on every re-baseline.

### What the bump would cost when a tag does land

Sized here so the next session does not re-measure it:

1. **rbs re-vendor — zero.** `vendor_rbs.py` against 4.1.1 reproduces the
   committed tree byte-for-byte (measured). Only `PROVENANCE.md`'s version
   string and the dev machine's `gem install rbs -v 4.1.1` would move.
2. **`UNBUILDABLE_DEFINITIONS` — zero.** `--check` OK / 12 classes against
   master and under 4.1.1.
3. **Overlay re-sync — one file**, `data/core_overlay/resolv.rbs`, copied for
   provenance fidelity; inert on rbs ≥ 4.1.
4. **Re-baseline** — `snapshot.rb` + both harnesses + the sweep. No corpus
   diagnostic moves, so the expected snapshot delta is empty.
5. **The one real repo change:** master is past `9515c8f8`, so fixture 79 and
   the divergence-registry entry for #237 must be **deleted** — the ledger
   already books that as the bump's work. And `a2867efd` becoming in-pin makes a
   harness fixture for the Tuple set-op folds legal, which the +49 note
   deliberately deferred.

## Gates (survey branch, no `crates/` change)

`cargo test --offline` PASS · `harness/run.rb` + `run_snapshot.rb` PASS
(241/245, 3 gaps, 1 registered divergence, **0 unregistered**) ·
`fp_audit.py --gaps --sweep` **0 FP / 9204 files / 8 corpora** (gap table
reproduces `harness/CORPUS.md`) · `docs_check.py` PASS.

Clippy: green on the CI-pinned **1.88** toolchain (`.github/workflows/ci.yml`),
both in a fresh `CARGO_TARGET_DIR`. Local stable **1.95** additionally reports
one `collapsible_match` at `crates/rigor-cli/src/sig_gen.rs:1158` — pre-existing,
untouched by this branch, and not a CI red today; it becomes one whenever the
MSRV pin moves past 1.88.
