# `-> self` on instance methods (2026-07-31)

The port resolved the late-bound RBS returns (`instance` / `self`) only where a
SINGLETON declared them (`Time.now: () -> instance`) or where a BLOCK overload
did (`Array#each { } -> self`). An ordinary instance method declaring `-> self`
— the spelling core uses for the whole mutating and re-tagging family — fell
through to `None` ⇒ `Dynamic`, and every call chained off it went unwitnessed.

Measured outcome: **1 coverage gap closed, 0 FP across 9204 files.**

## Measure first (AGENTS.md), and what the prediction got wrong

The textual prediction was **negative**: scanning all 377 gap sites on
gitlab-foss `lib` + mastodon `app` for a receiver chain passing one of the 137
non-block `-> self` instance methods found 20 candidate lines, and reading them
showed nearly all were coincidental (`.new`, `.merge`, `.to_i` matching by name,
not by the RBS return that actually applies).

The implementation then closed a gap the prediction had NOT flagged:

```ruby
# gitlab-foss lib/gitlab/database/queue_error_handling_concern.rb:28
[error.message]
  .concat(error.backtrace)     # Array#concat: (*array[Elem]) -> self
  .join("\n")                  # was Dynamic ⇒ the chain died here
  .truncate(MAX_LAST_ERROR_LENGTH)   # ActiveSupport-only; now witnessed
```

Lesson for the next prediction: a chain breaks at the FIRST unresolved link, so
grepping for the `-> self` method next to the diagnostic's own column misses the
case where the break is several links upstream. Predicting on chains needs the
type, not the text — which in practice means building the fold and diffing the
gap set, as here.

## Implementation

`method_signature` gains a `ret_self` flag beside the existing `ret_instance`.
The two must stay apart because they differ on the SINGLETON path: `def self.x:
() -> self` returns the class OBJECT (unspellable in the flat return slot ⇒ it
keeps declining), while `-> instance` there means an instance of it.

On the instance path either flag now stores the `SELF_RETURN` sentinel, resolved
at lookup to the class the accessor was QUERIED with — the receiver, not the
declaring class, so `Symbol#freeze` is a `Symbol` and not an `Object`. That is
the same seam the rbs-4.1 `-> instance` work used, so this slice is ~10 lines on
top of it.

## Gates

| gate | result |
|---|---|
| `harness/run.rb` | **77 fixtures** (new: 77), 0 FP, 3 gaps (the pre-existing `rule: null` parse ones) |
| `harness/run_snapshot.rb` | PASS |
| `cargo test` | green (rigor-index 69 → 70) |
| clippy, fresh `CARGO_TARGET_DIR` | clean |
| `fp_audit.py --gaps --sweep` | **0 FP / 9204 files**; gitlab-foss `lib` **329 → 328**, every other corpus unchanged |

Fixture 77 pins the three shapes (`Array#concat`, `String#force_encoding`,
`Array#push`) plus a NEGATIVE CONTROL — a singleton `-> self` (`Struct.new`)
must stay `Dynamic`, or the fold would have been written on the wrong axis and
no positive fixture would notice.

## Honest value

One gap. The `-> self` surface is wide (137 non-block instance methods across
the vendored core+stdlib) but real code rarely chains off it in a way that
reaches a diagnostic — the same shape the flow-frontier notes keep finding. It
lands anyway because it is *parity*, not speculative precision: the pinned
oracle already resolves these, so every one of them was a standing divergence
waiting for a corpus that happens to chain through it. The typer surfaces
(`annotate`, `type-of`, hover, completion) get the precision immediately.
