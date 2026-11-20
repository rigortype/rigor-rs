# #683 — loop-cap per-operand gate on plain self-writes

Issue: `x = [1,2][x] || 0` inside a `while` fired
`flow.always-truthy-condition` on `if t` — the reference caps `x`
(the self-read keeps moving inside the `||` right operand's
always-truthy-x evaluation window). PR #751, merged `2219b2e4`
(head `7e94f7b1`).

## What changed

`loop_cap_widening`'s `compound` verdict used to exempt every
`x = <&&/|| chain>` self-write (the `Node::Logical` blanket
exemption treated the whole write as the monotone `x = x || v`
shape). The fix re-derives the verdict per operand post-`live`:

- Walk the write value's `&&`/`||` tree with an x-polarity gate per
  leaf — `x || R` evaluates `R` only while `x` is falsey (bounded
  `{nil,false}` then a truthy stick); `x && R` evaluates `R` while
  `x` is truthy (unbounded); `&&`'s LEFT operand contributes only
  its falsey part to the result. A leaf behind a statically-decided
  operand never evaluates (`x = 0 || f(x)`).
- A leaf reading the name while it may be truthy compounds, unless
  (a) the read is not an assumption read — an earlier DOMINATING
  x-free write cut it (`x = 1` cuts; `x = 0 if d`, `c2 && (x = 1)`,
  `begin;…;rescue`, block writes and `x <op>=` do not — `dominates`
  is shared with `assumption_read`); (b) the leaf folds back to the
  entry binding (`vt ⊆ entry` — `x + 0`, `x[0]` via the new
  `Integer#[]` fold `0[0]→0`; a nominal codomain like
  `x.bit_length`'s `Integer` is NOT convergence); or (c) the leaf
  declines to `Dynamic` in a way the reference must share — the
  name is unbound at entry (body-first `x`'s nil seed) or the leaf
  is a receiverless call nothing resolves (no toplevel `def`, no
  Object/Kernel surface, no project `def` on the innermost lexical
  class).
- Foreign-name leaves are typed against the BODY's bindings
  (`leaf_env` overlay): a dominating write replaces the entry
  binding (`k = 1; x = x + k` types `x + 1`); a non-dominating one
  joins it (`k = 0 if d` → `x + (0|1)`); a `for`-index or
  `index_writes` receiver write of a leaf-read name, anywhere in
  the body (a `for` after the read still rebinds `k` for the next
  pass), makes the leaf unprovable → compound.
- `x <op>=` and masgn sites keep the scan's `compound` (the
  reference's `||=`/`&&=` RHS reads are ungated; masgn self-compound
  is #639's row).

## Measured outcome

65+ probe variants vs `--no-cache` reference on full diagnostic
tuples: every review counterexample matches; residual diffs are
disclosed coverage losses only (port-silent where the reference's
Dynamic-absorption converges — `x = nil` seed, `for k in [nil]`,
multi-index `for j, k` — noted on #684). Pre-existing family gaps
unchanged and filed: `loop do` block loops (#752), ivar loops
(#639). Bonus closures: three #639 "Logical RHS carve-out" rows
(`x = k || [x]`, `x = false || [x]`, `x = nil || [x]`) and
`x = x.ceildiv(2) || 0`.

Sweep: `fp_audit.py --gaps --sweep` 0 FP across 8 standing corpora
at the release binary (also validates the `Integer#[]` fold on real
code). CI green; `cargo test -p rigor-infer` 410/410.

## Review trail

Five Opus 5.5 review/delegate rounds, each probe-verified:

1. Design companion (Needs fix → `440943f4`): dominance for the
   assumption cut; drop `vt ⊆ widen(entry)` (a nominal codomain is
   not convergence — `x.bit_length`); resolution-based receiverless
   check; `Integer#[]` fold.
2. Adversarial (Approved at `2d762682` + findings → `f8e00d69`):
   conditional writes cutting the assumption (same dominance fix);
   leaf typed with pre-loop values of body-written names
   (`leaf_env`); receiverless calls resolving to project `def`s /
   Kernel names (enclosing-class resolution).
3. Fix-confirmation (Needs fix → `c2bc8cec`): conditional foreign
   writes must JOIN the entry binding (`k = 0 if d`).
4. Fix-confirmation (Needs fix → `e7e413fa`, `be83d985`): any
   `for`-index write of a leaf-read name is unprovable, wherever it
   sits — a `for` after the read still rebinds `k` on the same pass.
5. Micro re-check (`7e94f7b1`, Approved): clippy `nonminimal_bool`
   refactor confirmed semantically neutral.
