# #140 — `tap`/`then`/`yield_self` modelled as calling their block exactly once (PR #180, merged `4d52215`)

Upstream model (rbs_dispatch.rb): the block param list binds by the same
`arm_of`/`join` table multiple-assignment uses — `self` gets the receiver's
NOMINAL class plus its own type arguments (`SelfSubstitute`), and extra params
on an Array receiver auto-splat the element join. Exactly-once means `break`/
`next`/`redo` arms are reachable and the call's value is the receiver unless
the block definitely exits.

What the port grew over eight review rounds:

- Block-local params and `;`-locals shadow outer pins (`|` joins skipped);
  `(nil)` parens don't enter the `&.` literal-nil fold (`paren_unwrapped`).
- `block_splat_table` ports `arm_of`/`join` per param slot — Tuple/Array[T]/
  opaque/nil/unknown arms; element JOIN per slot (safe side).
- `block_self_type` binds nominal `Integer`/`NilClass`/`Array[1|2]`/… not the
  value pin — killed a family of `always-truthy` FPs on `x == <lit>`.
- Union receiver arms get the same reopen check as scalars:
  `project_declares_method_through_ancestors` walks project include/prepend/
  superclass ancestry; `alias`/`alias_method`/`define_method`/`attr_accessor`
  reopenings are discovered via `Node::Alias` + `pending_aliases`.
- `Union#describe(:short)` sorts members by rendered text → `"s" | 1`.
- **`analyze_files` discovery widening** (three iterations): worklist =
  `expand(config.paths | argv) > files` only; declared `paths:` resolve
  against the config dir; expansion drops `BUILTIN_EXCLUDES + exclude:`
  (fnmatch, leading-period rule, directory-expanded entries only); undecidable
  patterns (`[` or `\`) decline the whole widening; argv keeps one analysed
  item per occurrence; `argv_roots: None` on the config-`paths:` fallback so
  bare `check`/`baseline`/`diff`/`triage` never widen. Lesson: a discovered
  declaration can CAUSE diagnostics, so "harmless extra discovery" is not a
  safe lean.

Deferred: #190 (`;`-local decline), #195 (ternary narrowing, pre-existing),
#198 (bare `--config` analyzed roots), #199 (scalar `paths:`), #201 (analyze-side
exclude + glob period), #202 (`for Integer` literal-return drift), #203 (text
format `[rule]`/`N error(s)` summary), #181 (generalised break-arm unions).

Gates at merge: CI 5/5 on `5adffbc`, `run_snapshot` 0 unregistered FP,
`fp_audit --gaps --sweep` 0 FP over 8 corpora.
