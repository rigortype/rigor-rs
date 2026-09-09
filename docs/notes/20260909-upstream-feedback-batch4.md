# Upstream feedback, batch 4 — found at the `v0.3.4 → v0.3.8` re-pin (2026-09-09)

Batches 1–3: [20260716](20260716-upstream-feedback.md), [20260807](20260807-upstream-feedback-batch2.md),
[20260826](20260826-upstream-feedback-batch3.md). Everything below was measured against the
PINNED reference at `ffb456b0` (`v0.3.8`) — and, for the bisects, against detached worktrees
of that same submodule checkout — from a fresh temp cwd per run with `--no-cache` and both
`-I` libs (`UPSTREAM.md` hazard 1). Host: Ruby 4.0.5, rbs 4.2.0.

> **Filed.** Item 1 → upstream [#870](https://github.com/rigortype/rigor/issues/870)
> (characterise) + [#872](https://github.com/rigortype/rigor/issues/872) (fix), item 2 →
> [#871](https://github.com/rigortype/rigor/issues/871). Three further findings from the
> same re-pin, which this note did not carry and which were verified at the `v0.3.8` tag
> AND at master `5f394719` before filing, are § 3–5 below:
> [#877](https://github.com/rigortype/rigor/issues/877) (rooted `::RUBY_VERSION` guard),
> [#878](https://github.com/rigortype/rigor/issues/878) (`->` body writes),
> [#879](https://github.com/rigortype/rigor/issues/879) (arity on an unenumerable receiver).

## 1. `rigor check` on rufo's `lib/rufo/formatter.rb` does not finish in 25 minutes (23 s for the whole gem at `v0.3.4`) — PR #547

The standing 9204-file sweep's `mail` corpus (the gem plus its vendored bundle, 874 files)
took **4,161 s** at `v0.3.8` against **130 s** at `v0.3.4`. Per-gem timing over the
vendored bundle found the whole difference in one gem, and then in one file:

| invocation (`rigor check --format json --no-cache …`) | `v0.3.4` | `v0.3.8` |
|---|---|---|
| `mail` corpus, 874 files, one batch | 130 s | 4,161 s |
| `vendor/bundle/ruby/4.0.0/gems/rufo-0.18.2` (11 files) | 23 s | > 300 s (timeout) |
| `rufo-0.18.2/lib/rufo/formatter.rb` alone (4,221 lines, 101 KB) | — | > 1,500 s (timeout) |
| every other vendored gem in the bundle, one batch each | — | ≤ 12 s (rdoc-7.2.0: 96 s) |
| `haml/lib` (51 files) | 2.0 s | 2.2 s |

So it is not a general slowdown; it is one file. A `sample` of the running process shows the
time in `rb_ary_push` / `ary_ensure_room_for_push` / `rb_ary_cancel_sharing` and GC sweep —
array growth churn, i.e. a superlinear walk, not a hang. RSS stays ~500 MB.

**Bisected** (`git bisect run` between `v0.3.4` and `v0.3.8`, probe = "finishes
`formatter.rb` within 120 s") to **`acd35612` — "Bind user-method call args per parameter
instead of bailing per signature" (PR #547, closes #524)**, in `v0.3.7`. `formatter.rb` is a
single 4,000-line class of mutually recursive `visit_*` methods with optional and keyword
parameters — exactly the shape #547 newly binds at every call site — so the per-parameter
binder appears to re-infer the callee return interprocedurally without a memo or a budget
that bounds the recursion.

Reproduce:

```sh
gem fetch rufo -v 0.18.2 && gem unpack rufo-0.18.2.gem
cd "$(mktemp -d)" && time ruby -I <rigor>/lib -I <rigor>/plugins/rigor-rbs-inline/lib \
  <rigor>/exe/rigor check --format json --no-cache <path>/rufo-0.18.2/lib/rufo/formatter.rb
```

`v0.3.6` (`aca4d22a`) finishes; `acd35612` and everything after it does not within the
budget. Related open perf issues: #775 ("bring `rigor check lib` allocations back toward the
v0.3.6 18.8M"), #820. This port's handling: none needed — rigor-rs checks the file in well
under a second — but the standing sweep now costs 70 minutes instead of 6, all of it in this
file, until the regression is fixed. `harness/CORPUS.md` records the per-corpus wall time.

## 2. Observation, not a defect: `VersionGuard` folds against the ANALYZER's Ruby, so the diagnostic set is host-dependent

ADR-47 WD5 (#627, `d20d6f90`) decides `RUBY_VERSION` / `RUBY_ENGINE` guards from the
interpreter running `rigor`, by design (`version_guard.rb`'s "The reference Ruby" comment
says why `target_ruby` is not consulted). Measured consequence for a differential port: the
same file yields different `check` output under Ruby 3.3 and Ruby 4.0 wherever a guard
straddles them (`… if RUBY_VERSION < "3.4"` is dead on 4.0.5 and live on 3.3), and
`Psych::VERSION` depends on which psych gem the host resolves. rigor-rs mirrors the
envelope with a fixed host pair (`4.0.5` / `ruby`, overridable by `RIGOR_RUBY_VERSION` /
`RIGOR_RUBY_ENGINE`) and declines `Psych::VERSION`. Worth a line in the manual's
"what rigor reads from its own runtime" list, if such a list exists; nothing to fix.

## 3. A version guard written `::RUBY_VERSION` is not folded ([#877](https://github.com/rigortype/rigor/issues/877))

`VersionGuard.read_operand` routes on the NODE class, so the rooted spelling of a
PREDEFINED constant lands in `read_version_constant`'s curated `VERSION_CONSTANTS` gate
and is declined — while `Source::ConstantPath.qualified_name_or_nil` resolves it to
exactly `"RUBY_VERSION"`, the name `read_predefined` recognises. `::Psych::VERSION` (the
curated one) works, so the asymmetry is inside the feature. Dead arm reports;
bare twin silent. No live guard instance in our corpora, but the spelling occurs
(nokogiri `version/info.rb`, sass `util.rb`).

## 4. A local write inside a `->` body never binds ([#878](https://github.com/rigortype/rigor/issues/878))

`StatementEvaluator`'s dispatch table has `BlockNode => :eval_block` and no
`LambdaNode` entry, while `ExpressionTyper` types `LambdaNode`. So the body is walked,
reads enclosing bindings and reports on a literal receiver, but its own writes never
join a scope: `->(y) { y = 1; y.typo }` is silent where `lambda { |y| y = 1; y.typo }`,
`proc`, and an ordinary block all report `for 1`. Precision lost by spelling.

## 5. `call.wrong-arity` still enumerates a receiver `undefined-method` declines ([#879](https://github.com/rigortype/rigor/issues/879))

#739/#742's `unenumerable_receiver?` has one caller; arity reads the narrower
`unbounded_receiver_surface?`. On one receiver in one run the analyzer declines to say
which methods exist and asserts how many arguments one takes. **What we could not show
is half the report**: a project-module override produces nothing (arity needs an
RBS-declared signature), the `Class`/`Module` half does not reproduce for arity, and
neither does `raise` — so of the eight callers only arity-on-an-RBS-module speaks, and
we found no corpus instance. Filed with that stated, because upstream's own comment
asked for evidence before widening.

