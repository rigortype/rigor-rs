# A position-aware top-level env for the `check` rules

rigor-rs does not type a top-level local read from the state at the read's own
position, with joins at loop and block exits and through `ensure`. Since #148
(issue #133), the `check` rules' top-level env instead widens to `Dynamic`
every top-level local that a nested construct rebinds. That is a decline, not
the reference's mechanism. The reference joins every `next` / `break` path into
the continuation (upstream #1248 `loop_iteration`, #1215
`evaluate_invocation`). So the port gives up rows where a rebound local is read
afterwards, and at a few sites it reports a less precise message.

```ruby
w = "s"; [1].each { |e| w = e }; x = w; x.zzz   # reference: … for "s" | 1   port: silent
w = "s"; w.zzz; [1].each { |e| w = e }          # reference: … for "s"       port: silent (use before the rebind)
w = 5; if $c; w = 5; end; "abc".center(w).lenght # reference: for " abc "   port: for String
```

## Why this is out of scope

The change it would reverse has no footprint on real code. This was measured on
2026-09-25 at pin `e59b7b89`, over 9,337 files in all 8 corpora of the
standing sweep. The port's full diagnostic tuples (path, line, column, rule,
**message**) were compared across three builds: just before #148 (`877ff4f`),
the #148 merge (`c509e58`), and master (`8be037a`). The three outputs were
identical: 170,716 rows each, with 0 rows lost, 0 gained, 0 message changes and
0 FPs. The pipeline was checked against #148's own fixture, which does differ
across the builds (14 rows before, 7 after). The zero is therefore real: the
shape barely occurs in the corpus. Rails-app and library code lives inside
`class` / `module` / `def`, where the port reads no top-level env.

Building it is a deep flow change: per-position reads, and joins at every loop,
block, jump and `ensure` exit on the top level. It would be ADR-backed, and
exactly the kind of flow work AGENTS.md rules out without an `fp_audit --gaps`
prediction that it closes something. This one predicts nothing.

The one narrow, clearly correct regression from #148 is tracked separately and
stays in scope: a block parameter or block-local that shadows the top-level
name (#166).

## What would change the decision

A top-level-script-shaped corpus in the standing sweep (scripts, `Rakefile`s,
`bin/` tools), measured the same way. Re-open if the full-tuple diff shows rows
lost, or messages drifted, against the reference. Porting the reference's
jump-path joins for method bodies would also change the cost side, because the
same machinery would then exist to reuse at the top level.

## Prior requests

- #152: "Position-aware top-level env: recover the coverage #148 traded, and the
  `begin … ensure` next-path row"
