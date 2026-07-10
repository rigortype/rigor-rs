# Coverage frontier re-measured (2026-07-11) — bounded wins exhausted

After the sig-gen arc closed + the MCP `sig_gen` tool landed, I re-ran
`harness/fp_audit.py --gaps` to decide the next work by MEASURED gap frequency
(AGENTS.md: "never build a coverage slice without a valid-mode gap count").

## The numbers (0 FP everywhere — the zero-FP bar holds)

| corpus | files | ref | rigor-rs | matched | FP | gaps |
|--------|-------|-----|----------|---------|----|----|
| mastodon/app/models | 248 | 115 | 108 | 108 | 0 | 7 |
| mastodon/app | 1236 | 459 | 397 | 397 | 0 | 62 |

`mastodon/app` gaps by rule:
- `call.undefined-method` — 33 (the TAPPED-OUT lever, [[undefined-method-lever-exhausted]])
- `call.possible-nil-receiver` — 26 (flow substrate)
- `flow.always-truthy-condition` — 2 (flow substrate)
- `call.argument-type-mismatch` — 1 (unimplemented rule; rare on Rails)

## What the possible-nil gaps actually are (characterized, not guessed)

Sample (mastodon/app/services, `fetch_link_card_service.rb`):
```ruby
linked_account = ResolveAccountService.new.call(...) if extractor.author_account.present?
...
if linked_account.present?
  @card.author_account = linked_account if linked_account.can_be_attributed_from?(domain) || ...
```
The reference flags `linked_account.present?` (163) AND
`linked_account.can_be_attributed_from?` / `.local?` (166-167) as possible-nil.

**Root cause = conditional-assignment nilability.** `local = expr if cond`
leaves `local` **nil** on the paths where `cond` was false (Ruby's
undefined-local-is-nil), so `local : typeof(expr) | nil` regardless of the RHS
type. The reference tracks this and flags every call on the local — even the
nil-SAFE `.present?`. It also does NOT narrow inside `if local.present?` (it
still flags 166-167). rigor-rs does not model conditional-assignment nilability,
so it types `linked_account` non-nil and stays silent.

This is NOT [[possible-nil-fold-gated]]'s already-landed nilable-local flow
(string/array slice + nilable-RBS return), NOR ADR-0041's project-method nilable
return — the nilability here is from the CONDITIONAL ASSIGNMENT, not the RHS
return type. It is a genuine flow-substrate feature with real FP risk: to be
zero-FP, rigor-rs must reproduce the reference's exact narrowing semantics
(what narrows the nil away, what doesn't — the reference deliberately does NOT
narrow via `.present?` here), or it will over-fire.

## Conclusion — the frontier is deep-substrate, not bounded

Every remaining coverage lever is deep or tapped out:
- undefined-method: receiver-typing coverage exhausted (measured, memory).
- possible-nil / always-truthy: need the flow substrate (ADR-0022/0038) —
  conditional-assignment nilability, AR-RBS nilable returns, ivar whole-class
  flow, loop narrowing. Each is opt-in, ADR-backed, FP-risky, one-at-a-time.
- argument-type-mismatch: an unimplemented rule, but ~1 gap on Rails (the ~30 in
  the older survey were algorithm-heavy corpora); low Rails ROI, needs param-type
  comparison against RBS (params are lenient today, ADR-5).

**There is no cheap bounded FP-safe coverage win left.** This re-confirms the
[flow-frontier note](20260706-flow-frontier-exhausted.md) with fresh 2026-07-11
numbers. The genuine next tracks are LARGE and directional:

1. **Flow / ScopeIndexer substrate** — the biggest lever: unblocks
   possible-nil + always-truthy (the 28 gaps here, and the survey-wide clusters)
   AND `sig-gen --params=observed` (the substrate-blocked note). Multi-session,
   ADR-backed. The single highest-leverage remaining track.
2. **More productization** — LSP §12 two-tier (watched-files invalidation,
   debounce, worker pool); `rigor_coverage` MCP tool needs the ADR-63
   mutation-backed coverage command (large).
3. **A specific flow slice** — e.g. conditional-assignment nilability alone,
   built on the existing flow substrate with an exact-narrowing investigation +
   the fp_audit gate. Bounded-ish but FP-risky; needs the full delegation
   protocol (investigate narrowing semantics → spec → implement → audit).

Recommendation: option 1 (flow/ScopeIndexer substrate) is where the leverage is,
but it is a deliberate multi-session commitment — start with an investigation +
ADR, not a speculative slice. Option 3 is the smallest concrete step INTO that
substrate (close the 26 possible-nil gaps' dominant cause).
