# #639 — loop-cap for jump/once bodies, rebindable ivars, write-position `moved`

Issue: six shapes where the reference's `BodyFixpoint.converge` reaches
the cap pass and the port kept a dead-arm pin — firing
`flow.always-truthy-condition` the reference stays silent on (residual
#435 FPs, re-found in PR #634's review). PR #756, merged `1dba0801`
(heads `98462f1d` + the `85c77ce1` memo).

## What changed

- **Jump/once bodies keep the cap.** The replay gate
  `!(jumps || once || For)` in both `flow_eval_stmt` loop arms is
  UNTOUCHED — `next`/`break`/`redo` and `begin…end while` bodies still
  never replay — but the static `loop_cap_widening` scan now also runs on
  them, widening via `apply_loop_cap_widen` exactly as the reference's
  jump-independent cap decision does.
- **Ivars are tracked names.** `loop_tracked_names` unions
  `loop_body_local_writes` + `loop_body_ivar_writes`: an env-unbound ivar
  still counts when the enclosing scope binds it off a WRITE
  (`ivar_scope_writes` — `rebindable_ivar?`), its seed comes from that
  last in-scope write, and an unreplayable tracked-ivar write (an
  `UnmodeledWrite` carrying the new parse-side `IvarWrite`, or a literal
  `instance_variable_set`) forces the structural verdict inside
  `loop_continuation`. `IvarWrite` lowers `@x op= v` (compounds
  unconditionally) and `@x ||= v` / `@x &&= v` (compound only when the
  Prism value subtree re-reads the name — `rereads`).
- **`moved` types at the write's body position.**
  `loop_body_env_overlay` (hoisted from #683's inline leaf walk, now
  shared with `moved`): per read position, the join of the name's live
  writes before it — a dominating write's value replaces the entry
  binding, anything else joins it back in. An unprovable read binding
  (a `for` index, a pattern bind) now returns `None` → the leaf
  compounds; `y = [k]; k = y` is a pass-1 mover.

## Measured outcome

22 fresh-dir probe rows (`probe.py`, pin v0.4.8): identical
`(rule, line, col, severity, message)` sets on both engines. Eight live
master divergences closed: `next`/`break`/`redo`/`begin…end while`
bodies, `y = [k]; k = y`, `@pos += 1`, literal
`instance_variable_set(:@y, [@y])`, `@y ||= [@y]`. Controls still fire
on both engines (convergent `y = 2` through a `next` body; `@pos = 2`;
non-literal `instance_variable_set`; `@y ||= [k]`; `@y &&= 1`; a
non-self receiver; a nested-`def` write). The issue's own r1/r2 repros
and the (b)/(d)/(f)/(g) rows already matched on master at v0.4.8 — the
reference at this pin fires always-truthy on r2 itself; the live
divergences are the eight rows above. `for` bodies: BOTH engines fire
(the port's skip matches the reference). Gates: snapshot 0 unregistered
FP, coverage gaps 103 = master's 103 (no retraction), clippy clean
(fresh dir, pinned 1.88), `fp_audit --gaps --sweep` 0 FP.

## Time hazard (measured)

Ungating jump bodies newly reaches the PRE-EXISTING decision-gate arena
scan (smallest node span enclosing a hot decision — O(nodes) per
decision span): the 33k-line generated
`mail/lib/mail/parsers/address_lists_parser.rb` went 0.82s → 2.34s.
`85c77ce1` memoizes the gate per call (pure over `(ast, d)`, same
`min_by_key`, once instead of per fixed-point pass): 2.10x. The
reference takes **6.7s** on that file — the port stays ~4x faster than
the oracle on its worst corpus row; every other file ≤1.06x. The
first-pass O(nodes × decisions) remains: #759.

## Residuals (review round 1, probed and filed)

- **#757** — an ivar seeded from a pre-loop op-write (`@pos += 1`) or an
  entry-env-typed seed still under-caps (port fires; reference silent;
  also fires on master).
- **#758** — `ivar_scope_writes` counts lambda/block/conditional-arm
  writes as rebindable: the port caps where the reference converges and
  MISSES `always-truthy` (coverage loss on three edge shapes; NOT an FP
  — the dead-arm call diagnostic fires on both engines).
- **#759** — the decision gate's first pass is O(nodes) per decision
  span (perf follow-up; optional enclosing-span index).

## Environment notes (oracle work on this machine)

The pin needs Ruby ≥ 3.2 (`Data.define` in `bleeding_edge.rb`): probe.py,
gate.sh's snapshot step and any hand-rolled oracle invocation must run
under `PATH="$(mise where ruby@4.0.5)/bin:$PATH"` — the system 2.6
crashes at startup, which looks like an oracle "difference" until the
stderr is read. `fp_audit.py --sweep` additionally needs PyYAML:
`mise where python@3.15.2` has it, the system python3 does not.
