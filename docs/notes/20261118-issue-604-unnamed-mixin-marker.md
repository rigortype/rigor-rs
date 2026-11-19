# #604: UNNAMED mixin marker (`"?"`) — computed class-body `include` opens the class

Upstream v0.4.8 **#2991** (computed class-body `include`) and **#3006** (computed
`send` taints), PR #612 → merged 2026-11-18. Closes #604; follow-ups filed as
#617 (singleton-side `?` reading), #618 (`lin` doesn't follow listed-name
edges), #619 (`def` inside `instance_eval` census gap).

## What upstream changed

`MixinAccumulator` gained a second marker: `UNNAMED = "?"`, always written as
`UNNAMED_LIST = ["*", "?"]` beside the order-wildcard `WILDCARD = "*"`. The
distinction: `*` says the mixin ORDER is unknown (`include M; prepend M`),
`?` says the module SET itself is unrecordable (`include(*MODS)`, `include m`,
a hook's `base.include M`, a computed `send(meth, M)` that could spell
`:include`). `ResolutionChain#unnamed_mixin?` reads it; `check_rules`'s
`unknown_mixin_includer?` (inside `last_resort_surface_answers?`) declines a
`call.undefined-method` when the receiver's instance chain lists `?`.

`#3006` folds computed `send`/`public_send`/`__send__` into the same taint:
`send(m, M)` with `m` bound to `:include` in the class body resolves through
`MixinAccumulator#class_body` (declaration bodies + first-level `class << self`
only — `opens_self = self && !in_singleton`) and `local_send_names`
(literal locals, twice-written constants, `each`/`each_with_index`/`map`
iterated-element names). A name bound only to non-mixin literals taints
nothing.

## Port shape (the interesting divergences)

- **Two markers, one `listed`** — `?` rides inside the same name lists as `*`,
  matching `UNNAMED_LIST`. Cost: every pre-existing reader that treated
  non-`*` entries as module spellings had to grow `mixin_marker(n)` —
  `mixin_closure_may_answer`, `singleton_raws`, `open_extend`,
  `core_singleton_mixin_may_answer`, `singleton_truncated`, the may-define
  edge readers. Missed once, a `?` resolves nowhere, lands in an `open` arm,
  and silently closes union arms the reference still fires on.
- **`hook_parameter_receiver?` ≠ `hook_receiver?`** — the reference uses the
  BARE local for `unnamed` and `computed_send_name`, the call chain
  (`base.singleton_class`, `base.class`) only for the effects question.
  Conflating them marks `?` where the reference keeps `*`, and the `?` then
  rides `extend_hook_taint` to silence extenders it shouldn't.
- **`hook_def_params` has no receiver check** — an instance
  `def included(base)` binds `base` exactly like `def self.included(base)`;
  without that, the instance half of `extend_hook_owners` is dead code.
- **`self_owner` = `owner.or_else(eval_owner)`** — `X.instance_eval` sets
  `eval_owner` but the frame `owner` stays `None`; the reference's
  `current_class` carries the eval receiver through the same walks. The
  port re-joins them at the mixin paths only (def census still reads
  `owner` — #619).
- **`extend_hook_taint`** — `["*","?"]` appended to a mark when an extended
  module's include side lists `?` and the module defines an extend-hook
  (`extended`/`extend_object` singleton-side, `included`/`append_features`/
  `prepended`/`prepend_features`/`inherited` instance-side). The literal-fold
  `definers` table misses non-literal bodies (`def self.extended(b); end`),
  so the fact lives parse-side as `AncestryFacts::extend_hook_owners`.
- **`unnamed_mixin_includer` is scalar-only** — consulted at the scalar,
  narrowed and Dynamic-carrier collection sites, never in the union path:
  `union_arm_blocks_undefined_fire?` doesn't ask
  `last_resort_surface_answers?`, so `"a" | Foo` over a `?`-open `Foo`
  still fires.

## Measured outcome

- ~60 probe rows against the v0.4.8 oracle (project `sig/` and bare): all
  matching on the merged head — the 4 brief rows, the ~50-shape review
  sweep, and ~40 round-2 boundary rows.
- Gates: `harness/gate.sh` pass; 1.88 clippy clean; CI green (5 checks).
- Sweep not runnable on the implementing machine (all `sweep-corpora.yml`
  corpora absent); disclosed in the PR body.
- Review: round-1 adversarial gate **Needs fix** (2 real regressions caught:
  the hook-param/chain confusion and the `?`-as-name leak); design
  companion found 3 more frame-model gaps (eval_owner, instance hook defs,
  `opens_self`). Round-2 **Approved** at `5d9fedc`.
