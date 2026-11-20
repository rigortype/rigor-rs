# #696 — ShapeDispatch's String refinement carriers (PR #730)

The reference's `integer_to_s_projection` / `float_to_s_projection`
(`method_dispatcher/shape_dispatch.rb`) answer `Refined[String,
:decimal_int]` (`decimal-int-string`) and `Difference[String, ""]`
(`non-empty-string`) for `Integer#to_s` / `#inspect` and
`Float#to_s` / `#inspect` on nominal receivers. The port answered the flat
RBS `String` — a message drift at the same (rule, line, col) key the
snapshot harness cannot see. `Refined` and `Difference` were dormant
`Type` variants: nothing minted them, so first use also meant first
consumers — `class_name_of` erasure, canonical-name display, mutation
widening — all of which had to agree before the name could print.

## What landed

- `refinement_projection` in `call_dispatch.rs`, ahead of the RBS return
  tier: `integer_to_s_projection` (no arg / `Constant[10]` →
  `decimal-int-string`, any other single arg → `non-empty-string`, >1
  declines), `float_to_s_projection` (`non-empty-string` floor),
  `dispatch_integer_range`'s `to_s` (non-negative lower bound + static
  base only), `dispatch_integer_binary_from_arg` (`Integer * 0` → `0`),
  the `decimal_int` `refined_self` rows (`upcase`/`downcase`),
  `empty_removal_projection` on `non-empty-string` (size →
  `positive-int`, `empty?` → `false`, preserving unaries, `+`, `*`
  count), `dispatch_string_binary_from_arg` for every String-typed
  receiver, and `SIZE_RETURNING_NOMINALS` (size selectors →
  `non-negative-int` on String/Array/Hash/Set/Range nominals — fixed the
  adjacent `chars.size` drift too).
- `describe_named`: `Refined` canonical names via
  `RefinementId::canonical_name` (`Refined::CANONICAL_NAMES`); empty-witness
  `Difference` names (`non-empty-string`, `non-zero-int`,
  `non-empty-array[T]`, `non-empty-hash[K, V]`).
- `class_name_of` erases `Refined`/`Difference` to the base nominal —
  the reference's `erase_to_rbs`, so chained dispatch and witnessing
  see the base class.
- `widen_mutated_binding`: `widen_refined` → `String` base under ANY
  String mutator; `non-empty-string` keeps its witness only under the
  `EMPTY_PRESERVING` mutators (new `STRING_MUTATOR_EMPTY_PRESERVING` in
  `rigor-parse`, identical to upstream's — verified against
  `string_mutation.rb`).

## Measured

- Every probed row matches the oracle: the brief rows, the chained
  projections, `IntegerRange#to_s`, mutations (`<<`/`upcase!` keep
  `non-empty-string`; `clear`/`[]=`/`strip!` strip it; `Refined` widens
  under everything), `v.to_s`/self-returners erase, `+`/`*` arg rows.
  `rigor annotate` on the repro is byte-identical.
- `run_snapshot.rb` 0 unregistered FP; `fp_audit --gaps --sweep` **0
  FP** with gap totals identical to master (437) — keyed output is
  unchanged on the corpora; this is message-text fidelity the keyed
  diff cannot see. Review-measured corpus message drift: **47 → 46**
  (one `gitlab-foss` row, `for String` → `for non-empty-string`, 0
  regressions).
- clippy 1.88 clean; CI green on `194d34f7` (merge head).
- **Brief row 3 does not reproduce as recorded**: the 5001-digit
  literal gives `for String` on the current pin, and the port is silent
  there on master too (oversized `Constant` fold decline) → #737.
- **sig-gen**: review found the PR also fixes a master miss
  (`def zero: () -> 0`); byte-identical on the carriers.

## Residuals filed

#733 (`always-truthy` optimistic-origin guards — `empty? && ARGV.first`),
#734 (signed `IntegerRange` receivers type as bare `Integer`, so
`(-h.size).to_s` mints where the reference declines), #735 (truthiness /
`is_a?` / `===` narrowing on the new carriers collapses to
`Dynamic[top]` — coverage lost vs master), #736 (`+ "a"` Constant-arg
compat + `to_s(10.0|10r)` numeric-equality base), #737 (oversized
literal `to_s` → untyped), #738 (`inspect(10)` wrong-arity FP,
pre-existing). `tr!`/`tr_s!` translator precision stays a documented
decline (arg types aren't threaded into `widen_mutated_binding`); alias
repros added to #600. `empty?` on plain `Nominal[String]` remains
untyped (pre-existing). `FloatRange#to_s` (`numeric-string`),
`Difference[Integer]` and `Intersection` receivers are not minted.

## Design notes

- `RefinementId` is a bare `u32` with a `(base, predicate)` name table —
  mirrors the reference's `predicate_id` + `CANONICAL_NAMES` pair map.
  Only `DECIMAL_INT` is minted; the display falls back to the base for
  unregistered pairs rather than guessing a `base & pred?` spelling.
- Union absorption is *not* needed: the reference's
  `absorbable_member?` drops only FloatRange/Tuple/HashShape/sequence
  members, never `Refined`/`Difference`/`IntegerRange`, so keeping both
  members (`"a" | non-empty-string`) matches (design companion,
  substituted to swe-2 after the Fable routes failed).
