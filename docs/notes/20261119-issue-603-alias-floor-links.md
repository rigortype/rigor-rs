# #603: alias floor — selection links, receiver candidates, message-offset reach

PR #623 → merged 2026-11-18 (`cd03659`). Closes #603; follow-ups #625–#628.
Two review rounds (Opus adversarial + fix-confirmation, both `Approved`); a
Fable design companion ran alongside (`claude-bridge/claude-fable-5-1`,
OpenCode was quota-blocked).

## What diverged

Upstream's `alias_mutation_floor` (v0.4.8, the #2677 slice) saw three shapes
the port's copy-link machinery did not:

- `each_may_be` walks every `selection_branches` construct — `case`/`when`,
  `case`/`in`, `begin`/`rescue` (body, clauses, `else`; never `ensure`), and
  the `rescue` modifier's two operands — the port only walked
  `if`/`||`/`&&`. `g = case … when true then f …; g << 2` left `f` pinned
  and fired `for 1` where the oracle stays silent.
- `mutated_reads` runs `ReceiverAlias.candidates` on the receiver — a local
  write/op-write, an `if`/`unless`/`||`/`&&` arm's last statement, a
  parenthesized sequence's last — so `(g = f) << 2`, `(g ||= f) << 2`,
  `(c ? g : h) << 2` and `(c ? g : h)[0] = v` mark the local(s) the receiver
  may evaluate to. The port only ever marked a bare `LocalVariableRead`.
- The link reach keys on `handed_by(site) <= offset_of(call)` — the write's
  END vs the call's `message_loc` start — so a copy INSIDE the receiver
  (`(g = f) << 2`) ran before `<<` fired while one inside a later argument
  (`f << (g = h)`) did not. The port compared `write.start < call.start`,
  so the in-receiver link never reached.
- `literal_send_target` needs `call_node.block.nil?` — `g.send(:<<, 2) { }`
  is not the literal mutator.

## Port shape

- `rigor-parse` gained three carrier-identity sets (`CarrierSpans`):
  `paren_carriers`, `else_carriers` (both `BeginRescue`-shaped, unwrap to
  their last statement where a real `begin` names nothing) and
  `rescue_modifier_carriers` (`Statements{Recovered}`-shaped, operands
  contribute may-be locals where other recovered carriers name nothing).
  The design companion recommends a `BeginKind` tag plus an owned
  `RescueModifier` node instead — tracked in #625; the marks stay cheap and
  sorted for now.
- `receiver_alias_locals` answers the local candidates AND whether a
  non-local was seen (ivar read/write anywhere in the walk; cvar/gvar at
  the direct receiver, upstream `NON_ALIASED_READS`). A mixed set fails
  `reads.all?(LocalVariableReadNode)` upstream → `targets(nil)` — the
  unmodelled held/aliased set — so the span joins `non_floorable`: the
  local marks still stale (`f[0]` silent) but never propagate (`g[0]`
  keeps `for 1`).
- `IndexedFlow.mutation_offsets` maps mutator-call span → `message_loc`
  start; `alias_floor_reaches` tests `link.end <= offset` plus the
  unchanged iterated-body clause.
- `may_be_locals` answers each arm's LAST statement (review F1: the
  first draft read every statement — a non-last `s = f` floored `f` and
  reported `for String` where the oracle keeps `for "ab"`, also dropping
  `always-truthy` on `f == "ab"`). The pre-existing `if`/`||`/`&&`
  whole-arm superset is unchanged — tightening it is #625 (FP direction,
  needs a sweep).

## Measured

- The issue's four FP rows and ~130 edge probes match the oracle
  (controls: arg-writes don't reach, `begin` receiver names no local,
  `case`/`in` patterns skipped, depth cap 6 fires at 7, `ensure` never
  links, `send`-without-block still floors).
- Parity gains found in review: `(c ? g : h)[0] = 9` drops `h`'s pin;
  `(x = 1; g = f) << 2`, `(g &&= f) << 2`, `(c || g) << 2`,
  `.merge!`/`push` through selections, `g = super(f) rescue nil`.
- Gates: `gate.sh` pass (376 infer tests incl. 4 new), clippy 1.88 clean
  on a fresh target, CI green incl. live-reference snapshot parity.
- Sweep: standing corpora absent on this machine; substitutes were
  `fp_audit --gaps` on fresh `net-ssh`/`concurrent-ruby` clones (0 FP,
  gap counts identical to master) plus the reviewer's 2,034-file
  master-vs-branch diff (0 differences). Real corpora don't exercise
  these forms — the probes carry the measured outcome.

## Residuals (filed)

- #625 — `rescue`-modifier carrier walks flattened operand leaves
  (over-links a selection's predicate; needs owned `RescueModifier`),
  `if`/`||`/`&&` whole-arm superset, `BeginKind` tag suggestion.
- #626 — deferred/lambda links propagate to outer bindings,
  `targets(nil)`'s `held`/`held_part` seeds unmodelled, `removed_keys_shape`
  (`(c ? f : h).delete(:a)`).
- #627 — pre-existing adjacent FPs: ivar/gvar receivers, `for`-index
  targets, element links, compound `[]=`/`||=` through selections.
- #628 — `(c ? f : (@a = v)) << 2` floors `g` on the oracle only (a
  site-collection quirk; port keeps `g` pinned — new edge-shape FP).
