# Edge-aware scopes and fact buckets

Status: accepted

Every expression is analysed in an input `Scope` and produces output scopes for five edges; scopes are immutable snapshots partitioned into named fact buckets with targeted invalidation rules. This is a parity surface: the normative semantics are ported faithfully from [control-flow-analysis](../../../../ruby/rigor/docs/type-specification/control-flow-analysis.md); only the Rust representation is ours ([ADR-0002](0002-diagnostic-set-parity.md)).

## Five output edges

Each expression produces scopes for:

- **normal** — falls through to the next statement;
- **truthy** — holds when the expression is truthy (used by `if`, `&&`);
- **falsey** — holds when the expression is falsey (used by `unless`, `||`);
- **exceptional / non-returning** — `raise`, `throw`, `exit`, calls whose inferred return is `bot`;
- **unreachable** — represented by `bot`; the scope is bottom.

These scopes carry both positive and negative facts. Joins merge facts conservatively.

Short-circuit threading follows the reference spec exactly:

- `a && b` analyses `b` in the **truthy** scope of `a`.
- `a || b` analyses `b` in the **falsey** scope of `a`.
- `!a` swaps truthy and falsey scopes.
- `unless a` uses the same condition facts as `if a`, then swaps branch destinations.
- `case`, pattern matching, and chained `elsif` thread negative facts from earlier arms to later arms.

## Immutable snapshots and structural sharing

Each `Scope` is an immutable snapshot keyed by control-flow edge. Joins, narrowing, and invalidation produce new snapshots through structural sharing rather than in-place mutation. The public surface of `Scope` does not expose buckets directly; consumers ask for facts about a target.

## Fact buckets and targeted invalidation

Within a snapshot, facts are partitioned into six named buckets that mirror the reference's categories:

| Bucket | Contents | Invalidated by |
|---|---|---|
| `local-binding` | Local variable bindings | Assignment to that local only |
| `captured-local` | Outer locals writable by a live closure | Closure escape or unknown invocation |
| `object-content` | Hash keys, instance variables, shape members | Any unknown call that may mutate the receiver |
| `global-storage` | Constants, class variables, globals | Any cross-scope mutation |
| `dynamic-origin` | Facts about `Dynamic[T]` values | Unknown calls, escape |
| `relational` | Multi-target relational facts | Any participating target's bucket changes |

An unknown method call sweeps `object-content` and `global-storage` while leaving `local-binding` intact. This matches the reference's targeted-invalidation rule verbatim.

## Equality narrowing and trust levels

Equality narrowing trust levels are a parity surface. The reference defines four tiers; rigor-rs implements them in order:

1. **`equal?` identity facts** — value fact while the reference itself remains stable; invalidated by reassignment, alias-escaping mutation, or unknown calls.
2. **Built-in literal-domain equality** — trusted only for finite literal sets of `String`, `Symbol`, `Integer`, booleans, and `nil`, and only when the receiver dispatch target is known and the receiver domain is already compatible.
3. **`Float` literal narrowing** — **refused** by default. A relational fact may still be recorded for diagnostics, but Float values are never narrowed to a literal domain.
4. **`Range`, `Regexp`, `Module`, `Class`, `===`-based comparisons, user-defined `==` / `eql?` / `===`** — remain relational facts until RBS metadata or a plugin declares true-edge and false-edge effects.

Negative facts are domain-relative: a negative fact removes values from the already-known positive domain and MUST NOT introduce a new positive domain from the right-hand side. Example: `v: untyped; v != "foo"` stays `Dynamic[top]` with a relational fact, NOT `Dynamic[String − "foo"]`.

## Purity policy and built-in mutation summaries

Methods are impure by default. Purity becomes effective only from an authoritative source: core RBS distributed with rigor-rs, accepted ordinary RBS files, or explicit `rigor:v1:pure` annotations on `RBS::Extended`.

The v1 milestone ships built-in mutation, purity, and call-timing summaries for a fixed class set: `Array`, `Hash`, `String`, `Set`, `IO`, `StringIO`, `File`, `Tempfile`, `Pathname`, and `Logger`. Each summary records per-method receiver-mutation status, argument-mutation status, block call timing, and purity declaration. Classes outside this set follow the impure-by-default policy.

## Break-binding propagation

`break` inside a loop must not lose its scope. rigor-rs implements a **break-sink stack**: entering a loop body pushes a fresh accumulator; `BreakNode` evaluation appends `(scope, break_value_type)` to the active sink and returns a `bot` continuation; the loop's exit join folds every collected break scope's local bindings into the continuation scope alongside the normal-exit scope. A stack handles nested loops because `break` targets the innermost enclosing loop (top of stack). Syntactic over-approximation of break-path writes is rejected as not FP-safe for `possible-nil-receiver`. Source: [20260615-loop-break-binding-propagation-design](../../../../ruby/rigor/docs/notes/20260615-loop-break-binding-propagation-design.md).

## Considered options

- **Reimplement control-flow scopes independently with a different edge / bucket design** — rejected: control-flow behaviour is observable through diagnostics; any divergence is a parity break ([ADR-0002](0002-diagnostic-set-parity.md)).
- **Mutable scope threaded through a visitor** — rejected: immutable snapshots with structural sharing are the reference's design and prevent aliasing bugs.
- **Blanket "any unknown call invalidates all facts"** — rejected: the reference explicitly requires targeted invalidation so local-binding facts survive ordinary method calls.
