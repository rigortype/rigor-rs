# 2026-11-18 — #448 frame-local `$_`/`$~` writes, `$@` reads; DefinitionContext placement

PR #613 merged as `bc9a11f`. The effects collector now ports the
reference's special-variable sets exactly (`unit_scan.rb:40-59`):
`UNCOLOURED_READS = {$~, $_, $!, $@}`; writes suppress only `{$~, $_}` and
only when `!shared_slot` — a `define_method` body runs on the defining
body's slot, so it keeps `global.write`. The write arm also gained
`GlobalVariableTargetNode` (`for $g`, masgn, `rescue =>`), which upstream
already listed and the port had missed.

The same probes exposed unit-placement drift, so the PR ports
`DefinitionContext` as `BodyCtx` (`self_kind × definee_singleton`):
`def_body` / `module_call_body` / `singleton_class_body` / `for_child`'s
block arms (eval blocks, `class << obj`, anonymous builders
`Class.new`/`Module.new`/`Struct.new`/`Data.define`), carried through both
walks. Master had filed units the reference does not file (`def m;
define_method(:x)`, `def obj.x`, top-level `define_method`/`attr_*`) and
mis-keyed `def self.m; define_method(:x)` as `C.x` for `C#x`.

`effects --full` byte-identical to the oracle on every fixture (issue rows,
the whole match/backtrace family on both sides, ~20 placement shapes,
10 adversarial reviewer fixtures). `effects_diff.py` 0 OVER / 0
DECLARED-MISMATCH, MATCH=384 / UNDER=320; reviewer ran six installed gems:
MATCH=2414 / UNDER=1174 / **0 OVER**. `fp_audit --sweep` not runnable on
this host (0/8 corpora present); the diff is private to `effects::collect`
and unreachable from `check`.

Review: Opus 5.5 `Approved`; Fable design companion `sound`. Follow-ups
filed: #614 (`LocalOwnership.allocation?`/`constructor?` needs the
`singleton:` thread — proven-lane OVER), #615 (rooted `class ::Path` must
replace the unit prefix), #616 (dead unnameable-header arm, stale
citations, `module_call` gate placement). Residual: typer-free blanket
taints keep eval-heavy bodies non-exhaustive (UNDER, by design).
