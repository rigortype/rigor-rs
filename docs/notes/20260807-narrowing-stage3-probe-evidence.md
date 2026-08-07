# Class narrowing, stage 3 — measured coverage frontier (2026-08-07)

PR #63 shipped stages 1–2 (`is_a?`/`kind_of?`/`instance_of?` guards on
`if`/`unless`/ternary, single-static-constant `case`/`when`) and closed 11
gaps. This note is the MEASURED input for stage 3: which shapes the reference
narrows that rigor-rs still declines, probed live rather than guessed, plus how
many real gaps each shape is worth.

Companion: [the position rule](20260807-block-narrowing-position-rule.md) — the
FP side of the same investigation. Everything below is the COVERAGE side: every
row is `ref=1, rs=0`, so closing it is FP-safe by construction.

## Probe results (pin `v0.3.1`, fresh cwd, `--no-cache`; rigor-rs `d6aa6e5` release)

Guard shapes — the narrowed call is `v.frobnicate_zzz`:

| # | shape | ref | rs | |
|---|---|:--:|:--:|---|
| c1 | `v.frobnicate_zzz if v.is_a?(String) && v.length > 2` | 1 | 0 | conjunction: guard is one CONJUNCT |
| c2 | `v.is_a?(String) && v.frobnicate_zzz` | 1 | 0 | narrowing into the `&&` RHS |
| c4 | `return if !v.is_a?(String)` then use | 1 | 0 | NEGATED guard + early return |
| c6 | `when Hash, String then v.frobnicate_zzz` | 1 | 0 | multi-condition → UNION `Hash \| String` |
| c7 | `h.last.frobnicate_zzz if h.last.is_a?(String)` | 1 | 0 | single-hop CHAIN receiver |
| c3 | `return unless v.is_a?(String)` then use | 1 | 1 | already covered |
| c8 | `raise ArgumentError unless v.is_a?(String)` then use | 1 | 1 | already covered |
| c5 | `while v.is_a?(String)` then use in body | 0 | 0 | reference does NOT narrow — nothing to do |

Statement forms — the guard is an ordinary `when String`, and what varies is
the form CONTAINING the narrowed use:

| # | shape | ref | rs | |
|---|---|:--:|:--:|---|
| d1 | `cache[v] ||= v.frobnicate_zzz` | 1 | 0 | index op-assign RHS |
| d2 | d1 with a nested `if … else … end` as the RHS | 1 | 0 | the mastodon archetype below |
| d4 | `@x = v.frobnicate_zzz` | 1 | 0 | ivar-write RHS |
| d3 | plain nested `if` inside the `when` | 1 | 1 | already covered |

`class_flow_stmt` models `Statements`/`LocalVariableWrite`/`MultiWrite`/
`LocalVariableOpWrite`/`Call`/`Return`/`If`/`Case`/`Definition`/`ClassDef`/
`ModuleDef`; every other statement hits the `other` arm, which widens and
**clears all facts with no descent**. d1/d2/d4 are that arm firing — the fact
is discarded before the RHS is ever looked at.

## Why this matters more than it looks — the archetype is one of these

mastodon `app/lib/activitypub/case_transform.rb:18` is the shape the census
named as mechanism 1's archetype, and it is STILL open after PR #63:

```ruby
when String
  camel_lower_cache[value] ||= if value.start_with?('_misskey') || …
                                 value
                               else
                                 value.underscore.camelize(:lower)   # gap
                               end
```

The `when String` narrowing is computed correctly; it is thrown away by the
index op-assign (d2) before the use is reached.

## Gap accounting (from the verified 1168-row census, 2026-08-07)

Of the undefined-method gaps whose 8-line window contains a class guard, after
PR #63:

| bucket | rows | note |
|---|---:|---|
| unmodeled containing form (d1/d2/d4 family) | 27 | the largest; the archetype above is in it |
| `&&` conjunction (c1/c2) | 12 | 8 pure, 4 combined with another tag |
| negated guard (c4) | 10 | 5 pure, 5 combined |
| chain receiver (c7) | 6 | 1 pure, 5 combined |
| multi-condition `when` (c6) | 1 | |

Tags overlap, so these are not additive; treat them as per-shape upper bounds
in the sense the [chain-gap prediction rule](../AGENTS.md) means it — predict
by TYPE, then build and DIFF the gap set. Total distinct window-candidates
remaining: 49.

## Shape of the stage

Two independent halves, either buildable alone:

- **3a — more guard shapes**: `&&` conjunction (narrow from any conjunct, and
  into the RHS), negated guards feeding an early return, single-hop chain
  receivers (reference `narrowing.rb:1805` + `expression_typer.rb:1057`
  `method_chain_narrowing_for`), multi-condition `when` as a UNION receiver.
  Each needs its own decline set; the union receiver in particular means
  `check_narrowed_call` must witness against an INTERSECTION over arms, not a
  single class.
- **3b — record uses inside unmodeled statement forms**: descend the RHS of
  ivar writes, index/attribute op-assigns and friends to record USES before
  clearing, exactly as `LocalVariableWrite` already does. This grants NO new
  facts — it only stops discarding one before reading the expression — so it
  is the cheaper and safer half, and it owns the biggest bucket.

`while` (c5) is measured NOT to narrow in the reference: do not build it.
