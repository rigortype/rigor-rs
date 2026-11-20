# #347: Psych safe_load visitor deaths in `.rigor.yml` — the crash-mirror slice

PR #713 → merged 2026-11-20 (`13786130`). Closes #347; the design
follow-up is filed as #715 (generated tag-differential, then a ToRuby
arm table). Five review rounds: round 1 Approved-with-findings;
rounds 2-4 each **Needs fix** — every newly modelled arm missed a
control-flow shape the flat scan did not mirror; round 5 Approved on
the one-line coarse rule.

## What diverged

`YAML.safe_load_file(absolute, aliases: false)` — the reference's
`.rigor.yml` load — dies uncaught where the port's serde_yaml pass ran
or exited 64: `Psych::AliasesNotEnabled` on any alias node, and
`Psych::DisallowedClass` on scalars the restricted `ClassLoader`
rejects (Date/Time/Symbol and the tagged `deserialize` arms).
`config::psych_error::safe_load_rejection` walks libyaml's own event
stream for the first document and yields the first visitor death in
document order. Two invariants carry the design: parse precedes visit
(a syntax error anywhere in the document declines the candidate — the
drain always reaches the document's end), and every arm the scan
cannot model stops RECORDING rather than guessing — the `!ruby/*`,
`!omap`-SEQUENCE and matching-kind empty-name collection bails; the
`Float()` / `Encoding.find` / untagged FLOAT-arm scalar stops; and a
`!str`-family mapping stops after its children when any non-`str`
member exists (`init_with`'s `instance_variable_set` raises for all
but the plain-String/valid-name shapes).

## The instructive part — four rounds of one failure

Each fix's newly modelled arm revealed another arm whose control flow
the flat scan did not mirror:

- R2: the bail `break`ed the event loop (a later syntax error stopped
  outranking an earlier recorded death), and `!omap` sequences /
  empty `!map:`/`!seq:` names are NoMethodError shapes, not
  descend-into ones.
- R3: an unmodelled death (`ArgumentError`, `FrozenError`) let the
  port name a LATER sibling the reference never visits.
- R4: the `StrMapTracker`'s "completes" predicate guessed per shape —
  fifteen `init_with` crash shapes fell through it (`NameError` on
  `'@1'`-shaped member keys; `FrozenError` on tagged/nil `str`
  values).

The terminal fix deleted the precision: stop whenever a non-`str`
member exists — a superset of every crash shape whose only cost is
priced coverage (the benign members-exist shapes load against a
reference Date). The lesson is the zero-FP bar itself: a per-shape
completeness guess is a counterexample waiting for its reviewer; a
superset decline cannot be wrong. #715 is the structured way to buy
the precision back.

## Probing hazard (cost an hour; bit twice)

`mise exec ruby@4.0.5 -- python3 harness/probe.py` does NOT give
probe.py's nested `ruby` the mise ruby — the reference boots under
system 2.6 and dies on `Data.define`, which coincidentally "matches"
every crash row while failing every row that should complete a check
(a false 25-pass/16-fail over the standing battery). Prepend
`PATH=$HOME/.local/share/mise/installs/ruby/4.0.5/bin`; and
`fp_audit.py --sweep` needs mise python `3.15.2` (the only one with
`import yaml`).

## Measured

0 FP on every head (`fce6e22a`, `d12b7550`, `f0039005`, `d6db7f22`);
gate green (docs_check, `cargo test -p rigor-cli`, run_snapshot 0
unregistered FP); 41/41 standing parity rows; the 45-row probe battery
shows the port naming a class exactly where the reference does;
clippy 1.88 clean in a fresh `CARGO_TARGET_DIR`. Coverage gaps
unchanged (215/138/59/18/8/2 — the 215 vs 212 is #708's corpus): this
is a crash mirror, not a coverage slice. Residuals, all declining-side
and disclosed in the PR body: `!ruby/*` tags exit 64 where the
reference exits 1 (serde rejects them), and the declined shapes load
where the reference crashes NoMethodError/ArgumentError/FrozenError.
