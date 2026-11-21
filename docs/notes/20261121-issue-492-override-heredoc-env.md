# #492: override-replaced parents + heredoc closure env — measured outcome

PR #789 (approval head `4f3d81be`, final head `9053c669` after the #790
rebase; oracle `bbea1bac` / v0.4.8). Two port-only `check` FPs closed;
item 3 (the parenthesised-assignment column) already matched since #468.
Rebased twice mid-arc (#786 ownership, #790 stage order) — every gate
re-run per rebase, no interaction FP.

## What landed

- **Item 1 — `def.override-visibility-reduced`** (upstream #2013
  `contest_replaced`): a parent's `def` replaced by `attr_*` /
  `define_method` / `alias` (same body, reopening, or folded
  cross-file) contests the slot; the overridden-parent visibility read
  declines unless a later certain write settles it. Rebind census:
  `superseded_def_nodes` (same-file non-`def` replacer after an
  earlier def, cleared by a later def), file-scoped `settled`
  (`private :x`-family sends, `private attr_reader` members, a
  `private def` argument, a reinstating def), `uncertain_definer`
  (supersede declines every read; the replaced mark declines only
  visibility-reading ones). `driver.rs`'s `visibility_replaced` shares
  the settle semantics.
- **Item 2 — heredoc-interpolation env** (#473 follow-up): a block or
  interpolation inside a heredoc body reads the env at the closure
  (`flow_eval.rs`: `CheckFlow.escaped` chains span-escaped nodes back
  to their boundary statement; `entry_descend_chain` walks the
  recorded links, a `Barrier` link takes the closure's entry env;
  flat-container subtree effects stay conservative), not the
  end-of-file flat env.

## Measured

- 13/13 probe rows identical on both engines, full tuples: the five
  replaced shapes silent; the fire controls (settle row ×2,
  reinstating def, include-M row, the reverse heredoc leak `for 1`,
  the post-rebind use); the item-3 column `@2:26`.
- Standing sweep (8 corpora, 9,441 files): **0 FP**, gap totals and
  per-rule splits identical to the pre-slice master on all 8 corpora
  (`0 FP/437=master`) — corpus-neutral both directions: 0 corpus gaps
  closed, 0 lost.
- Gates: `gate.sh`, `cargo test --workspace` (0 failed), clippy 1.88
  fresh `CARGO_TARGET_DIR`, CI 5/5 — re-run after each rebase.

## Review (round 1: `Approved`; Fable quota-blocked → default-model
companion substitute, recorded in the PR)

None blocking. **#793** is the arc's one diff-caused regression
(coverage-only, safe side): a def nested in a def body enters the
supersede census — master matched the reference there; fix hint: an
in-def guard on the insert. #794 pre-existing port-only row (supersede
keyed by the class's last segment; another namespace's same-name class
clears the mark). #795–#797 pre-existing interpolation/flow gaps
(interpolation write outflow, interpolation always-truthy, literal
`==` falsey folding). #798 design debt (one `VisibilityTable`, reuse
the ADR-127 body census for replacer names, the direct-`attr_*`
settle's `uncertain` guard — latent, probed unexploitable today).
