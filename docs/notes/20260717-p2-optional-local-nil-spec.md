# P2 — optional-local-nil slice: `Regexp.last_match` source (2026-07-17)

Probe-first coverage slice for `call.possible-nil-receiver`, targeting the
2026-07-17 gitlab-foss lib possible-nil gap classification (169 gaps). The task
hypothesis was "a local assigned a **core-RBS OPTIONAL return** (`Array#find`/
`#min`/`#pop`, `MatchData#[]`, `Regexp.last_match`, `String#slice`) then
dereferenced in straight-line flow". **The probe REFUTED the broad hypothesis
and CONFIRMED one clean sub-cluster.**

## Probe findings (the mechanism map)

rigor-rs already has the nilable-local flow substrate
(`nilable_receiver_snapshots` / `nil_flow_*` in `crates/rigor-infer/src/lib.rs`),
which threads a straight-line `local -> non-nil core arm C` fact, descends block
bodies with a fresh env, and gates the fire on: method ABSENT on NilClass ∧
PRESENT on the concrete arm `C` (`check_nil_receiver`). The keystone FP-safety
property is that the arm `C` must be a **concrete core class** we can name — the
reference fires far more widely because its `union_method_present_on_non_nil?` is
**permissive on a `Dynamic` arm**, so a `Dynamic | nil` receiver fires for it.
rigor-rs deliberately cannot mint `Dynamic | nil` (that is the FP cliff).

Classifying all 169 gaps by receiver source (heuristic trace of the receiver
local's assignment; `harness`/scratchpad script):

| bucket | count | closeable? |
|---|---|---|
| `method_return` (project/AS method → `Dynamic\|nil`) | ~86 | NO — needs interprocedural inference + Dynamic-arm firing |
| `other_assign` (mostly `present?`/`blank?` on `Dynamic\|nil`) | ~43 | NO |
| `coll_optional` (`.first`/`.dig`/`.children` on a Dynamic receiver) | ~17 | NO — receiver is Dynamic |
| `service_execute` (`r = Svc.new(..).execute` in begin/rescue) | ~8 | NO — begin/rescue nil + project return |
| **`lastmatch` (`Regexp.last_match` / MatchData)** | **~7** | **YES** |
| `param_or_ivar` | ~5 | NO |
| `str_optional` (`.match`/`.slice` on a Dynamic/regex-literal receiver) | ~2 | NO — receiver not typed |
| `env` (`ENV[key]`) | ~1 | NO — ENV untyped + present? clear |

Two firing-guard facts verified against the live reference (config-less core):

- **`present?` / `blank?` / `presence` do NOT narrow** (throttle.rb:29,
  base_svg_chart.rb:146, token_partition.rb:34 all still fire after such a
  "guard"). But this does NOT help rigor-rs: those guards appear inside
  `if`/`unless` MODIFIERS or `&&`/`||`, which the substrate treats as UNMODELED
  and clears ALL facts regardless (decline backstop) — the guard-LIST membership
  never matters there. De-guarding the list alone closes 0 real gaps.
- **`if x` / `unless x` / `x.nil?` DO narrow** — reference stays silent after
  them; the substrate's clear-all backstop matches (under-fire, FP-safe).

The broad-hypothesis methods miss for concrete reasons the probe pinned:
`Array#find`/`#min`/`#pop` return `Elem?` (arm is the element type = unknown,
`method_return_nilable` = None, and the arm is not nameable → gate 4 can never
fire); `MatchData#[]` and `String#match` return None from `method_return_nilable`
because their overloads DISAGREE on `(class, nilable)` (`#[]` has a `Range ->
Array` overload); `ENV#[]` receiver is untyped. `String#slice`/`#byteslice`/
`String#[](Range|Regexp,Int)` ALREADY mint `String|nil` today (verified), but the
real corpus sites for them sit behind an `if`/`unless`/`begin` that clears.

## The implemented sub-cluster: `Regexp.last_match`

All 7 closeable gitlab-foss lib gaps share ONE shape — a `Regexp.last_match`
call (a core SINGLETON returning an optional), bound to a local, then
dereferenced straight-line inside the same `gsub`/`gsub!` block:

- `click_house/dictionary_credentials_handler.rb:19` — `content =
  ::Regexp.last_match(2)`; `content.gsub(...)` — `last_match(Int) -> String?`.
- `gitlab/help/hugo_transformer.rb:138` — `tabs_content = ::Regexp.last_match(1)`;
  `tabs_content.gsub(...)` — `last_match(Int) -> String?`.
- `gitlab/ci/variables/collection.rb:142,143` — `match = Regexp.last_match`;
  `match[0]`, `match[:key]` — `last_match() -> MatchData?`, deref `#[]`.
- `gitlab/help/hugo_transformer.rb:156,157,158` — `match_data = ::Regexp.last_match`;
  `match_data[1]`, `match_data[2]`, `match_data.begin(0)` — `MatchData?`, deref
  `#[]` / `#begin`.

### Source recognition (fold conditions)

A new arm in `nilable_source_class`, matched BEFORE the `class_name_of` receiver
resolution (the receiver is a `ConstantRead`, which types to a `Singleton` whose
`class_name_of` is `None`, so the existing path bails):

- receiver is `Node::ConstantRead { name: "Regexp" }` (both `Regexp` and
  `::Regexp` lower to this bare name) — a syntactic gate on the CORE constant,
  matching the reference resolving `Regexp.last_match` against core RBS; and
- method is `last_match`, and
  - **zero args** → arm `"MatchData"` (`() -> MatchData?`), or
  - **exactly one `IntegerLit` / `StringLit` / `SymbolLit` arg** → arm `"String"`
    (`(int|name) -> String?`), or
  - **anything else** → DECLINE (unknown / non-literal arg — never guess).

`knows_class("MatchData")` and `"String"` are both true; `#[]`/`#begin`/`#gsub`
are present on their arms and absent on `NilClass`, so `check_nil_receiver`'s
gates 3/4 pass exactly for the corpus derefs.

### Decline conditions (FP backstop, all inherited from the substrate)

- ANY `if`/`unless`/`begin`/`&&`/`||`/multi-assign between the bind and the use
  clears the fact (unmodeled → decline). This is why `token_partition.rb` /
  `auth.rb` / `throttle.rb` (all behind such a construct) stay silent — matching
  that the substrate can only ever fire on genuinely straight-line flow.
- A guard (`if m`, `m.nil?`, `m&.x`) narrows and clears → matches the reference
  staying silent after a real narrowing guard.
- Non-literal / multi arg to `last_match` → decline (return class not decidable).
- A project constant coincidentally named `Regexp` is not a realistic hazard
  (the reference resolves the same core method); `::Regexp` is explicitly
  top-level and the syntactic name gate mirrors the reference's resolution.

## Not implemented (declined, with reason)

- `Array#find`/`#min`/`#pop` (`Elem?` arm unnameable), `MatchData#[]` /
  `String#match` (overload disagreement → arg-aware resolution needed, +FP
  surface), `ENV#[]` (untyped receiver + present? clear), the `Dynamic | nil`
  clusters (present?/try/blank? on a permissive arm — the FP cliff). Each is a
  deep, per-cluster effort for a handful of gaps, consistent with
  `docs/notes/20260706-flow-frontier-exhausted.md`.

## Expected deltas

gitlab-foss lib possible-nil 169 → ~162 (−7), 0 FP required. mastodon/app and
gitlab app/models: 0 change expected (no straight-line core `Regexp.last_match`
cluster), 0 FP required.
