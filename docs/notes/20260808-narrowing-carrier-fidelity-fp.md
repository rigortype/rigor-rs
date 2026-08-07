# The narrowing subset argument breaks where OUR carrier is coarser (2026-08-08)

A live false positive on master, found by the stage-3b-1 build when its
value-descent reached the shape, and confirmed independently.

```ruby
def f(spec)
  h = spec || {}
  h.is_a?(Hash) ? h.frobnicate_zzz : h
end
```

| | verdict |
|---|---|
| reference `v0.3.1` | **silent** |
| rigor-rs at master `a61992f` | `undefined method 'frobnicate_zzz' for Hash` |

(The ivar-write form `@spec = h.is_a?(Hash) ? … : h` is silent on BOTH at
master, which is why only 3b-1's descent into write values surfaced it. The
sweep had no other instance, so 0 FP / 9204 files was true and uninformative.)

## Why the FP-safety argument failed

Every narrowing spec in this arc rests on one claim: we narrow ONLY a local
whose type is `Dynamic`/`Top`, mirroring the reference's `narrow_class_other`
(`narrowing.rb:2425`), which narrows Dynamic/Top and leaves every other
carrier alone. That makes us a strict subset — **provided the two engines
agree on which locals are Dynamic.**

They do not. `rigor-rs` types `Node::Logical` as `Dynamic[top]`; the
reference's `analyse_or` builds a UNION of the two operand types. So for
`h = spec || {}`:

- reference: `h` is a union ⇒ `narrow_class_other` does not apply ⇒ no
  narrowing ⇒ silent.
- rigor-rs: `h` is `Dynamic[top]` ⇒ our gate fires ⇒ narrowed to `Nominal[Hash]`
  ⇒ we witness.

The gate inverts exactly where our typing is COARSER than the reference's.
"Narrow only Dynamic" is a subset rule only when Dynamic means the same thing
on both sides; a carrier we collapse and they refine turns the rule inside out.

Second instance, same cause: a local bound from a project method whose return
tail is a `Logical` (`def mk; unknown_zzz || {}; end`) — pinned as `fp2` in
`crates/rigor-infer/src/lib.rs`'s narrowing tests alongside `fp1` above.

## Scope of the exposure

Any construct where rigor-rs produces `Dynamic`/`Top` and the reference
produces something narrower is a candidate. `Logical` is the one measured;
the fix should enumerate the others rather than patch this shape alone. This
is a general audit item, not a one-line guard.

## The fix taken

Conservative decline, consistent with how this arc has handled every other
carrier it cannot mirror: a local whose binding is (or transitively resolves
to) a `Logical` does not narrow. It costs coverage — one census row,
gitlab-foss `concurrency_limit/middleware.rb:14` — and it cannot manufacture
a diagnostic the reference lacks.

The alternative, typing `Logical` as a real union, is the correct long-term
answer and would also unlock union receivers for stage 3a-4. It is a
substrate change with consumers across `type_of`, `sig-gen`, `type-of`,
coverage and the LSP, so it wants its own slice and its own gate run — not a
patch inside a narrowing stage.

## The lesson worth keeping

Two earlier findings in this same arc were the same mistake in different
clothes: the safe-nav decline picked the wrong AXIS
([position rule](20260807-block-narrowing-position-rule.md)), and here the
Dynamic-only gate picked the wrong CARRIER. Both survived review because the
argument sounded like a subset argument. Before accepting "we do strictly
less than the reference", check that every term in the claim means the same
thing in both engines — and probe the shapes where our representation is
known to be coarser.
