# #453: one-pass Logical edges in `subject_edge`

PR #645 → merged 2026-11-18 (`84f66cb`). Closes #453. Follow-ups #646
(multi-statement parenthesized predicate) and #647 (`&&` right operand still
typed on an Empty left edge) are pre-existing FPs, byte-identical on master.

## What was slow

`subject_edge`'s `Logical` arm re-evaluated the left operand on the
non-sequential edge (`&&` falsey / `||` truthy) with no memo. Nested
alternating `&&`/`||` grew ~1.6× per term. Master `1c2594a` release `check`
on `harness/gen_nested_logical_guard.py`:

| terms | master | this PR |
|---|---|---|
| 26 | 0.47 s | 0.08–0.20 s |
| 34 | 14.05 s | 0.10–0.20 s |
| 68 | — | 0.08–0.20 s |
| 400 | — | 0.31 s |
| 68, every operand reads the subject | > 30 s | 0.19 s |

The loops×calls quadratic in `loop_edge_overlay` was already gone on master
(triage of #453). Beyond ~1,000 terms the remaining cost is not this walk:
`z = <same 2,000-term expr>` takes the same 8.5 s as the `if` form (#424).

## Port shape

Reference `analyse_and` / `analyse_or` (`narrowing.rb`) already return
`[truthy, falsey]` from one left-hand `analyse`. `subject_edges` does the
same; `subject_edge` picks one edge.

```
&& : (seq(lt, rt), j(lf, seq(lt, rf)))
|| : (j(lt, seq(lf, rt)), seq(lf, rf))
```

`seq` now takes two `Edge`s — the old `FnOnce` was always forced. Const join
`j` still lets `Keep` absorb (a scope-table entry the join drops unless
every path holds it). Atoms tabulate both polarities; `!` swaps the pair.

Call sites that still ask for both edges via two `subject_edge` calls
(`if_edge_overlay`, `class_narrowing`) are a constant 2×, not exponential.

## Measured outcome

- Nested-guard brief row and must-still-fire controls match the reference
  (full tuples).
- Port-vs-port JSON identical: 42 probes, mastodon/app + 32 gems (2,182
  files), gitlab-foss/lib (4,892 files / 1,299 diagnostics).
- `fp_audit.py --gaps --sweep`: 0 FP / 8,251 (4 corpora on this host).
- Snapshot 0 unregistered. Opus Approved (Fable companion: one-pass is the
  right wholesale port).
