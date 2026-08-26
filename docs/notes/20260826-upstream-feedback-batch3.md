# Upstream feedback, batch 3 — reference-side observations from the effects-port arc (2026-08-26)

Batch 1 is [20260716-upstream-feedback.md](20260716-upstream-feedback.md), batch 2
is [20260807-upstream-feedback-batch2.md](20260807-upstream-feedback-batch2.md).
Everything below was found while porting the effect system
([ADR-0043](../adr/0043-effect-system-port-parity-model.md)) on 2026-08-25/26 and
was **live-verified against the PINNED reference** on 2026-08-26: `reference/rigor`
populated in this worktree from the main checkout's tree at pin `b10bd5df`
(`v0.3.4`), never the network, never `REFERENCE_RIGOR_DIR`. Every command below is
the harness's own invocation form:

```sh
ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib \
  reference/rigor/exe/rigor <subcommand> …
```

(`UPSTREAM.md` hazard 1.) `.rigor/cache` was cleared (`rm -rf .rigor`) before every
run below unless the note says otherwise — findings 1's whole point is what happens
when it *isn't*.

Nothing below was dropped for failing to reproduce — the two assigned candidates
and one additional source-level observation all verified as described. Two other
things a re-read of this session's 2026-08-26 notes surfaced were deliberately
*excluded* as not findings, rather than reported and dropped: the caller-side
`declared:` lane mechanism and `Registry#known?`'s implied-ancestor rule are both
already the documented, spec-pinned contract on the reference's own side, not
surprises — they are recorded as this port's own operating facts in
[20260826-effects-s1-catalogue-probe.md](20260826-effects-s1-catalogue-probe.md)
§ 7 and § 1a rather than repeated here as upstream feedback.

## 1. `rigor effects` accepts no `--no-cache`, and its results ARE cached per-cwd

Every other analysis entry point takes `--no-cache` (`check`, `coverage`); `rigor
effects` does not, though it shares the same persistent `.rigor/cache` (on by
default, keyed by cwd — `UPSTREAM.md` hazard 2) as every other subcommand.

Repro (single file, fresh cwd):

```ruby
# lib/manifest.rb
class Manifest
  def say
    puts "hello"
  end
end
```

```yaml
# .rigor.yml
paths:
  - lib
effects:
  paths:
    - lib
```

```sh
$ ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib \
    reference/rigor/exe/rigor effects --full --no-cache --format=json
invalid option: --no-cache
$ echo $?
64
```

Observed vs expected: `rigor effects`'s option surface is `--format`, `--full`,
`--no-tolerated-effects` only (`lib/rigor/cli/effects_command.rb:95-102`, read at
the pin); `--no-cache` exists on `check` (`check_command.rb:487`) and `coverage`,
not here. Running the same manifest a second time with no source change and no
`--no-cache` (the plain `rigor effects --full --format=json` form) leaves a
populated `.rigor/cache/analysis.run-effects/` behind — confirmed by listing the
directory after the run:

```sh
$ find .rigor/cache -maxdepth 1 -type d
.rigor/cache/analysis.run-effects
.rigor/cache/analysis.run-diagnostics
.rigor/cache/rbs.environment
.rigor/cache/rbs.known_class_names
.rigor/cache/plugin.source_rbs_synthesizer
```

Since the tool runs *in the project directory* by design, that cache persists
across runs by construction. For a differential harness that wants a genuinely
cache-free `effects` arm — the way it can force one on `check` — there is no lever
but deleting `.rigor/` between runs by hand.

**Mitigating fact, verified by reading the cache-key composition (not just cited):**
`Effects::Identity.descriptor` (`lib/rigor/effects/identity.rb:71-85`) composes
onto the run's base diagnostics descriptor, whose `engine` slot pins
`Rigor::VERSION` + schema and whose `engine-source` slot digests the checkout's own
`lib/` (`lib/rigor/analysis/run_cache_key.rb:71-94`, added for issue #285 "so
editing `lib/rigor/inference/*.rb` … "), plus the catalogue's own content digest.
So a pin bump, or any edit to the reference's own source, *does* invalidate the
effects cache even without a flag — confirmed independently by editing
`lib/manifest.rb`'s body (`puts "hello"` → `File.read("/tmp/x")`) and re-running
without clearing `.rigor/`: the reported effect moved from `io.output.stdout` to
`io.fs.read`, no stale answer. The gap is narrower than "the cache never
invalidates" — it's "there's no flag to force a clean run *within* one version,"
which is exactly the case a differential harness (comparing two engine versions,
or the same version across `.rigor.yml` edits within a session) needs and does not
get for free the way `check --no-cache` gives it.

Our port's handling: `harness/effects_diff.py` clears `.rigor/cache` itself around
every oracle invocation (`shutil.rmtree`), so this repo's own measurements are
unaffected; the finding is upstream's CLI surface, not a defect any current
rigor-rs behavior depends on. See
[20260826-effects-s1-catalogue-probe.md](20260826-effects-s1-catalogue-probe.md)
§ 8a, where this was first probed.

## 2. The posture tier silently proves nothing on constants the reference itself cannot resolve — question, not a claimed bug

`rigor effects`'s posture tier (`unit_scan.rb:429`'s `posture_allowed?`) answers a
class's default effect for an uncatalogued selector only when the *receiver*
types exactly — and the reference's own typer cannot resolve every constant its own
effect catalogue (`data/effects/core.yml`) ships a posture for. Measured: **8 of
the catalogue's 80 posture-carrying classes** name a constant the reference types
`dynamic`: `Net::HTTP`, `Net::SMTP`, `Net::FTP`, `OpenSSL::SSL::SSLSocket`,
`Fiddle::Handle`, `Fiddle::Function`, `PTY`, `SOCKSSocket`.

Repro (single file, fresh cwd, same `.rigor.yml` as above):

```ruby
# lib/manifest.rb
class Manifest
  def net_http_posture
    Net::HTTP.zz_uncatalogued_zz
  end

  def pty_posture
    PTY.zz_uncatalogued_zz
  end

  def file_posture
    File.zz_uncatalogued_zz
  end

  def net_http_row
    Net::HTTP.get(URI("http://example.com"))
  end
end
```

```sh
$ ruby -I reference/rigor/lib -I reference/rigor/plugins/rigor-rbs-inline/lib \
    reference/rigor/exe/rigor effects --full --format=json
```

Observed:

```json
"Manifest#file_posture":     { "effects": ["io.fs"], "exhaustive": true,  "causes": [] },
"Manifest#net_http_posture": { "effects": [],        "exhaustive": false, "causes": [["dynamic-receiver","unsupported_syntax"]] },
"Manifest#pty_posture":      { "effects": [],        "exhaustive": false, "causes": [["dynamic-receiver","unsupported_syntax"]] },
"Manifest#net_http_row":     { "effects": ["io.net.http"], "exhaustive": true, "causes": [] }
```

`File` — a class the reference's typer resolves — answers its `io.fs` posture for
an uncatalogued selector, `exhaustive: true`. `Net::HTTP` and `PTY` — both
catalogued with non-empty postures (`io.net.http` and `io.process` respectively) —
answer **∅**, non-exhaustive, `dynamic-receiver`, for the identical uncatalogued-
selector shape. The fourth case shows the asymmetry is confined to the posture
tier specifically: `Net::HTTP.get`, a *row* (not a posture) on the very same
class the typer cannot resolve, still proves `io.net.http` cleanly — rows and the
34-name `universal:` list are unaffected; only the class-default posture answer
requires an exact receiver type.

We are not asserting this is a defect. It may well be intentional conservatism —
proving a class-wide default from an *inferred* receiver type is a stronger claim
than proving a row-based catalogue entry from a *matched selector*, and declining
where the receiver itself is unknown is a defensible position for a tool whose
own stated value is "false positives outrank worst-case static reading." What we
can say without guessing at intent: the catalogue (`core.yml`) ships postures for
all 80 classes uniformly, with no marker distinguishing the 8 whose constant the
analyser's own RBS environment cannot type from the 72 it can — so a catalogue
consumer has no way to know, short of probing every catalogued class against the
live typer the way this repro does, that a tenth of the posture-carrying classes
are unreachable through that tier. Worth asking upstream: is this the intended
contract (the posture tier is deliberately typer-exact-only), and if so, would a
per-class marker in `core.yml` (or a `rigor effects --full` diagnostic naming the
unreachable classes) be in scope — the same way `rigor doctor` already surfaces
other "this feature is silently inert for you" conditions?

Our port's handling: rigor-rs found the identical asymmetry as a *live over-claim*
— the shipped collector answered posture regardless of whether the pinned
reference's typer could — and dropped the posture tier for every receiver rather
than trying to replicate the reference's exact-typing gate
([20260826-s106-posture-over-fix.md](20260826-s106-posture-over-fix.md)); rows and
`universal:` selectors are untouched. A generated corpus
(`harness/effects-corpus/05_posture`, derived from the vendored catalogue so it
tracks a re-vendor automatically) now gates this: before the fix, `OVER=7` on
exactly the 7 of the 8 classes above with a non-empty posture (`Fiddle::Function`'s
posture is the empty label set, so it cost nothing); after, `OVER=0`.

## 3. Minor: `TaintCause`'s doc comment is stale, and 2 of its 10 declared causes have no producer at the pin

Source-level observation, not a CLI probe — the reproduction is reading the file
and grepping the tree, both against the pinned checkout:

```sh
$ cat reference/rigor/lib/rigor/effects/taint_cause.rb
```

```ruby
    # Nothing produces causes yet; the collector of #379 is the first writer.
    module TaintCause
      ALL = %w[
        dynamic-receiver dynamic-send method-missing unresolved-self-call
        opaque-callable unknown-ownership plugin-attribution
        template-not-analysed collector-error budget
      ].freeze
```

The comment is stale: #379 (`rigor effects`'s own collector) is what's live at
this pin — `docs/type-specification/effect-labels.md:91` itself says so ("Partly
implemented as of this writing. The proven lane, the exhaustiveness bit and the
taint causes are computed by the collector of #379") — and it demonstrably does
produce causes:

```sh
$ grep -n 'taint(' reference/rigor/lib/rigor/effects/unit_scan.rb
261:        taint(row.taint, row.key) if row.taint
262:        taint("plugin-attribution", row.key) unless row.discharge?
371:        taint("plugin-attribution", key)
435:        return taint("opaque-callable") if opaque_eval?(node)
450:        return taint("opaque-callable") if opaque_callable?(node, record)
458:        return taint("dynamic-receiver", record.cause) if record&.dynamic
474:        return taint("dynamic-send") unless selector
511:        taint("unresolved-self-call", node.name.to_s)
530:        taint("opaque-callable")
535:        return taint("unknown-ownership") if labels.nil?
```

Two more producers exist outside this file: `scanner.rb:195`
(`Summary.tainted("collector-error", method_name)`, the collector's own rescue
path) and `lib/rigor/plugin/effect_attribution.rb:78`
(`TAINT_CAUSES = %w[template-not-analysed opaque-callable]`, the plugin layer).
That accounts for 8 of the 10 declared causes. Grepping the full pinned tree
(`lib/` and `plugins/`) for the remaining two turns up **no producer for
`method-missing` or `budget` anywhere**:

```sh
$ grep -rn 'method-missing' reference/rigor/lib reference/rigor/plugins
reference/rigor/lib/rigor/effects/taint_cause.rb:19:  method-missing
$ grep -rn '"budget"' reference/rigor/lib reference/rigor/plugins
# (no output)
```

Low-priority and possibly intentional — a closed enum can reserve members for a
producer that hasn't landed yet (`method-missing` reads like a natural companion
to the `dynamic-send` / `opaque-callable` dispatch-failure family; `budget`
reads like it's meant for the engine's own inference-cutoff guards, which exist
elsewhere under a different name — `lib/rigor/inference/budget_trace.rb` — but
don't yet feed effects). Flagging only because the doc comment actively
contradicts the file it's attached to at this pin, which is the kind of thing a
one-line fix closes for good.

## Open-issue review (standing pre-bump chore, `docs/CURRENT_WORK.md` Now/Next)

Read against this session's own notes (no network): every upstream issue our
notes track as filed is either already recorded closed, or — newly confirmed this
session — addressed at the `v0.3.4` pin without our ledger ever having recorded
the closure. Nothing in the list below is still open.

| issue | what we asked for | status | evidence |
|---|---|---|---|
| #316 | toplevel `def` capturing a same-named DSL method cross-file | **Fixed**, already recorded closed ([20260823-repin-v034.md](20260823-repin-v034.md)) | `CHANGELOG.md:133`, PR #340, landed `v0.3.3` |
| #317 | `Array.new(n) { block }` element type ignoring the block | **Fixed**, already recorded closed | `CHANGELOG.md:127`, PR #336 |
| #318 | `defined?` operand analyzed as evaluated code | **Fixed**, already recorded closed | `CHANGELOG.md:129`, PR #337 |
| #319 | `Class.new do…end` block body scoped at top level | **Fixed**, already recorded closed | `CHANGELOG.md:125`, PR #338 |
| #320 | `class << Const = Object.new` singleton body dropped | **Fixed**, already recorded closed | `CHANGELOG.md:131`, PR #339 |
| #321 | spec-pinning ask: suppression surveillance self-acknowledgement polarity | **NEW — confirmed addressed, not previously recorded** | `spec/rigor/analysis/check_rules/suppression_spec.rb:156-177` pins exactly this behavior, citing "Issue #321" by number in-line |
| #322 | spec-pinning ask: `raise`'s singleton/instance operand asymmetry | **NEW — confirmed addressed, not previously recorded** | `spec/rigor/analysis/check_rules/raise_non_exception_spec.rb:95-118`, a `describe "singleton vs instance operand asymmetry (pinned)"` block that quotes our note by name ("`docs/notes/20260716-upstream-feedback.md` item 4") |
| #323 | spec-pinning ask: duplicate-hash-key's Float raw-source-slice label | **NEW — confirmed addressed, not previously recorded** | `spec/rigor/analysis/runner_spec.rb:4257-4269`, citing our note by name ("item 5") |
| #194 | stale-installed-gem plugin hijack (`UPSTREAM.md` hazard 1) | **Root mechanism fixed; hazard doc is now stale for the default case** | see below |

### #194 in detail

`UPSTREAM.md`'s hazard-1 section (and this project's harness) treats a bare
`ruby -I reference/rigor/lib` invocation as unsafe because RubyGems could resolve
the auto-wired `rigor-rbs-inline` require against a stale *installed* `rigortype`
gem's plugin copy instead of the checkout's own. At the `v0.3.4` pin this is no
longer how the auto-wired default resolves. `lib/rigor/plugin/loader.rb:64-77`
(`Loader.bundled_plugin_path`, landed as "#194 slice 2 (ADR-93 WD5)") now computes
`<ENGINE_ROOT>/plugins/<gem>/lib/<gem>.rb` and requires bundled plugins —
including the auto-wired default — **by that absolute, engine-anchored path**
rather than by gem name, specifically so "a stale installed `rigortype` gem can
never displace the engine's own versioned copy through RubyGems name resolution."

Live-verified on this machine, which has exactly the hazard's precondition — an
installed, several-versions-stale `rigortype` gem:

```sh
$ gem list rigortype
rigortype (0.2.4)

$ ruby -I reference/rigor/lib reference/rigor/exe/rigor plugins --format=json
```

```json
{
  "plugins": [{
    "gem": "rigor-rbs-inline",
    "status": "loaded",
    "version": "0.1.0",
    "path": ".../reference/rigor/plugins/rigor-rbs-inline/lib/rigor-rbs-inline.rb"
  }]
}
```

— note this ran with **only** `-I reference/rigor/lib`, no `-I
reference/rigor/plugins/rigor-rbs-inline/lib`, and still resolved to the pinned
checkout's own copy rather than the installed 0.2.4 gem's. `rigor doctor` (slice
3, PR #200) also ran clean (`rbs_environment: healthy`, no skew warning) — expected,
since slice 2 means there is no skew left for it to catch in this configuration.

This does **not** mean #194 the tracking issue is necessarily closed on GitHub —
the CHANGELOG still phrases slice 3's doctor check as catching "the engine↔plugin
version skew behind #194," present tense, and slice 2's own comment notes a
trimmed/single-binary install still falls back to gem-name resolution. But the
specific mechanism `UPSTREAM.md` hazard 1 and this repo's `-I
reference/rigor/plugins/rigor-rbs-inline/lib` workaround exist to defend against —
the *default* auto-wired plugin losing to an installed gem — no longer reproduces.
Keeping the defensive `-I` flag costs nothing and this note does not touch
`UPSTREAM.md`, but a human may want to fold this into hazard 1's text (or drop the
flag) the next time that file is edited.

## Dropped candidates

None. Both assigned candidates (§ 1, § 2) verified as described. § 3 was not
assigned — it was added because it reproduced cleanly on a read of the pinned
source and is cheap to report — and it is flagged as minor throughout; a human
may reasonably decide it isn't worth filing on its own.
