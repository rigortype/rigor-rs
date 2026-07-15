# Binding spec — Kernel `p`/`pp` identity, scalar-key HashShape, Kernel constant-folding (v0.3.0 RC)

Oracle: reference `47ec8625`. Sonnet investigation (source read + paired
reference/rigor-rs probes, 2026-07-16).

**Structural prerequisite discovered:** rigor-rs's `Typer::type_call` is only
reached for `Node::Call { receiver: Some(_) }`. A receiverless call (`p x`,
`format(...)`, `String(42)`, `Integer("42")`, …) falls through `type_of`'s
catch-all to `Dynamic[top]` and is **never dispatched**. Changes 1 and 3 share
one prerequisite: a new implicit-self dispatch entry (`type_implicit_self_call`)
keyed off `receiver: None`. Change 2 needs no prerequisite.

---

## 1. Kernel `#p` / `#pp` identity typing

Reference: `lib/rigor/inference/method_dispatcher/kernel_dispatch.rb`.

| arity | result |
|---|---|
| 0 args | `Constant[nil]` |
| 1 arg | **identity** — the arg's type object unchanged (pins preserved) |
| N args | `Tuple[type(a1), …]` |

Guards (decline ⇒ fall to RBS `-> untyped`):
- explicit foreign receiver — **including `Kernel.p(42)`** (probe: reference
  does NOT fold it) — automatic in the port since the new path is
  `receiver: None` only;
- user redefinition (`def p` toplevel / on the receiver class). rigor-rs has no
  scope object ⇒ sanctioned conservative substitute: a file-wide scan for ANY
  `def p`/`def pp` — if found, decline that name file-wide (under-emit, safe);
- splat/forwarding args ⇒ decline (arity unknown). Verify what rigor-parse
  lowers a splat call-arg to before wiring.
- A block does NOT block the fold.

Probe results (reference vs rigor-rs): closes real gaps p01 `p 42` → fires
`for 42`; p02 `p(1, "a")` → `for [1, "a"]` (Tuple rendering already
byte-matches); p03 bare `p` → `for nil`; p04 `pp 42`; p09 block form;
p10 `p({a: 1})` → `for { a: 1 }`. Must-stay-silent: p05 `Kernel.p(42)`,
p07 shadowing toplevel `def p` (falls to user-method inference, NOT closed
here), p08 splat, p11 Dynamic arg. p06 (`Foo.new.p(42)` w/ own `def p`) is a
pre-existing unrelated message gap (`for Integer` vs `for 42`) — out of scope.

---

## 2. Scalar-key HashShape

Reference commit `cdf57cfe`: `ALLOWED_KEY_CLASSES` widened to
`[Symbol, String, Integer, Float, TrueClass, FalseClass, NilClass]`;
duplicate keys now **last-wins** (was: degrade to `Hash[K,V]`) — position =
first insertion, value = last occurrence; equality = `Hash#eql?` (`1` ≠ `1.0`;
`1.0` == `1.00` — same f64, free in Rust).

Rendering (`describe(:short)`): Symbol/String keys keep colon form
(`a: 1`, `"k": 2` — even `"k with space": 1`); other scalars use hashrocket
(`1 => 2`, `nil => 0`). Erasure UNCHANGED: record form requires all-Symbol
keys; otherwise `Hash[K,V]` where Integer/Float widen to nominals but
`true/false/nil` stay literal (`Hash[Integer | nil | true, …]` probe-confirmed).
ShapeDispatch projections (`[]/fetch/dig/has_key?/slice/except/values_at/
invert`) widen their key guard to the full set.

Probe classification: h01/h02/h03/h06/h10 message-only (both fire, receiver
renders `Hash` in rigor-rs); h07 (`h[1].f` etc.) / h08 `.fetch` / h09
`.has_key?` are REAL set gaps — **rigor-rs has no HashShape projection tier at
all** (only `fold_tuple_projection`); h03 shows rigor-rs currently degrades
even all-Symbol dup-key hashes (fixed as a side effect of last-wins).

rigor-rs attachment points:
- `ShapeKey` (rigor-types/ty.rs:119) — add `Float(bits)`, `Bool(bool)`, `Nil`
  (`Int(i64)` exists but is unreachable today); f64 keys via `to_bits()`
  matching `Scalar::Float`'s existing convention.
- `Typer::hash_shape_or_hash` (rigor-infer/lib.rs:~400) — add
  Integer/Float/True/False/Nil key arms; replace decline-on-duplicate with
  in-place value overwrite (keep first position).
- `describe_named` HashShape arm (rigor-types/display.rs:63-77) — per-key
  separator: Sym/Str `": "`, else `" => "`.
- `erase_hash_shape` (display.rs:244) — record guard stays Symbol-only;
  extend the degraded-key-union rendering (Float→nominal, Bool/Nil→literal).
- NEW `fold_hash_shape_projection` mirroring `fold_tuple_projection`
  (`[]`, `fetch`, `dig`, `has_key?`, `slice`, `except`, `values_at`, `invert`)
  in `type_call` tier 2.
- FP guards: never emit an RBS record for non-Symbol keys (invalid RBS);
  last-wins must keep FIRST position or multi-key messages diverge.

---

## 3. Kernel constant-folding (`format`/`sprintf`, `String()`, `Hash()`, `Integer()`, `Float()`)

Reference: kernel_dispatch.rb (+ `23743e06`/`6a7fcd0d`/`207c807c` for
Integer/Float's original contract — those predate the RC but rigor-rs never
had them either; same greenfield entry point).

- `format`/`sprintf`: template AND all args Constant → run the real formatting
  at fold time → `Constant[String]`. Malformed directive (rescue) or result >
  `STRING_FOLD_BYTE_LIMIT = 4096` bytes → decline to the literal-string LIFT
  (still `String`-ish carrier; diagnostic still fires with coarser receiver
  rendering `literal-string`/`String`), never Dynamic, never panic. `%%`
  handled; no-arg `format("hello")` folds. Needs a small Ruby-`sprintf`
  directive interpreter in Rust (Rust format! grammar is NOT compatible).
- `String(v)`: fold only for Constant scalars in the safe set (rigor-rs's full
  `Scalar` = Int/Str/Sym/Bool/Nil/Float — all safe; reference also lists
  Rational/Complex which rigor-rs lacks). Ruby `to_s` semantics — reuse
  `named_float` (display.rs:323) for Float formatting. `String(Foo.new)`
  (user to_s) must decline the exact fold.
- `Hash(v)`: HashShape → pass through; `Constant[nil]` or empty Tuple → empty
  HashShape; else decline.
- `Integer(s)`/`Float(s)`: constant-string parse per Ruby grammar
  (decimal-int-only for Integer per `INTEGER_REFINEMENT_PREDICATES`).

Probes f01–f12 all: reference fires undefined-method on the folded constant,
rigor-rs emits nothing — every one is a real set gap. Boundary probes: f09
1000-byte fold exact; f10 ~5000-byte declines to lift; f03 non-constant arg
still fires `for String` via the lift; f04 `format("%d","x")` fold-time error
→ lift.

---

## Sequencing

1. implicit-self dispatch entry + `p`/`pp` (headline set-gap win, small).
2. scalar-key HashShape (independent; message parity + projection tier).
3. Kernel folding (largest raw gap count; needs the sprintf interpreter —
   its own slice).
