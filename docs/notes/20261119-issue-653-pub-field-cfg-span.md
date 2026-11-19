# #653: `#[cfg(test)]` on a `pub` field masks only the attribute (PR #689)

Follow-up from the Opus fix-confirmation review of #601, second
`#[cfg(test)]` span over-reach in `analysis_stack_audit` (#429).

## Problem

`cfg_test_spans` masked the whole item when the word after `#[cfg(test)]`
was in `ITEM_KEYWORDS` — and `pub` is in that list. A `#[cfg(test)]` on a
visibility-qualified struct or tuple-struct field therefore took the item
extent, and `item_end` scanned past the enclosing `}` to the next `{` or
`;`, masking a whole following item. Audit hits inside it went silent:

```rust
struct S { #[cfg(test)] pub probe: u8, real: u8 }
fn run() { std::thread::spawn(|| {}); }   // was: unflagged
```

## Change

- `opens_item`: `pub` looks one word deeper, past any `(…)` visibility
  group — an item keyword opens the item, a field name or type means only
  the attribute is masked. Keeps the common `pub` case at attribute
  precision.
- `item_end` returns at any `)`/`]`/`}` the item never opened (was: only
  `;`, the matched `}`, and a `}`-stop added mid-review). A masked extent
  is now always a bracket-balanced run — the design companion found the
  `)`/`]` underflow still swallowed legal code (`#[cfg(test)] union: u8`
  on a parameter; `union` is a weak keyword).
- Masked extents ⊆ master's on every input (probed 50 by the round-1
  reviewer; both mechanisms mutation-checked).

## Left over (filed, not fixed)

- #692: keyword-headed generic parameter (`fn f<#[cfg(test)] const N>`)
  still masks the item; keyword-headed call argument masks the rest of
  its argument list; neither exists in the tree. #652's use-brace-globs
  are the same spelling-enumeration family, no shared code.

## Gates / review

`cargo test -p rigor-cli` pass; workspace audit unchanged (no allow-list
churn); run_snapshot PASS (959 matched, 0 unregistered — test-only
module); sweep 0 FP (haml corpus absent). Opus Approved b5ba1cbc, Fable
Needs fix (all items landed), Opus fix-confirmation Approved 2fbb682a.
