# Upstream feedback from the v0.3.0-RC port arc (2026-07-16)

Porting the v0.3.0-RC surface into rigor-rs required probing the reference
(`47ec8625`) at a byte level — every rule's full edge matrix, every message,
every anchor. That process surfaced a handful of findings worth returning to
upstream. Everything below was **live-verified against the RC** on 2026-07-16;
items we could not reproduce were dropped (notably: the historical "stale
`.rigor` cache in the same cwd" lesson from our harness discipline did NOT
reproduce at the RC with same-size same-second rewrites — no report).

## 1. Inconsistency: `Kernel.p` declines the fold, `Kernel.format` folds

Verified probe (single file, fresh cache):

```ruby
a = Kernel.p(42)          ; a.frobnicate   # reference: SILENT (no fold)
b = Kernel.format("%d", 1); b.frobnicate   # reference: fires `for "1"`
c = Kernel.sprintf("%d", 2); c.frobnicate  # reference: fires `for "2"`
```

`kernel_dispatch.rb`'s `explicit_foreign_receiver?` guard declines the
`p`/`pp` identity fold for an explicit `Kernel.` receiver, but the
`format`/`sprintf` constant fold accepts the same receiver spelling. Both are
`module_function`s with identical call-form semantics, so one of the two
policies is presumably unintended. Suggested upstream action: pick a polarity
(both fold or both decline) and pin it with a spec either way — a port cannot
tell which one is the contract.

## 2. UX gap: `# rigor:disable-next-line` is silently ignored — and the new
suppression surveillance cannot see it

Verified: `x = foo() # rigor:disable-next-line call.undefined-method` neither
suppresses anything nor triggers `suppression.unknown-rule`/`suppression.empty`.
Mechanically, the hyphen after `disable` fails LINE_SUPPRESSION_PATTERN's
`\s+`, and also fails BARE_SUPPRESSION_MARKER's `(?![\w-])` lookahead — so the
comment is invisible to the whole suppression subsystem, surveillance included.

RuboCop users type `disable-next-line` reflexively; this is exactly the "your
suppression has no effect and you'll never know" failure mode
`suppression.unknown-rule` was built to catch (rule_catalog rationale). Small
suggested fix: let the bare-marker regex also match `rigor:disable-<word>`
variants other than `-file` and emit a surveillance diagnostic (either a new
`suppression.unknown-marker` or reuse `suppression.unknown-rule` with the
marker as the token). One-line spec candidates:

```ruby
# rigor:disable-next-line call.undefined-method   -> should warn, today: nothing
# rigor:enable call.undefined-method              -> same class of silence
```

## 3. Pin the suppression surveillance's self-suppressibility

Verified: `x = 1 # rigor:disable call.bogus-rule suppression.unknown-rule`
emits nothing — the surveillance diagnostic flows through `filter_suppressed`
like any other, so a comment can acknowledge its own typo complaint. This is
elegant and probably intentional (no special case, no regress risk since the
surveillance ids are themselves known tokens), but nothing in the specs pins
it. A port (or a future upstream refactor that reorders surveillance vs
filtering) can silently flip this. Two-line spec suggested.

## 4. Pin the `raise-non-exception` singleton/instance asymmetries

Probe-verified behaviors that are easy to "simplify" away accidentally —
our port nearly did, twice:

- `raise Object`, `raise Class`, `raise Comparable` **fire** (the singleton
  path has no `RAISE_UNEXACT_INSTANCE_CLASSES` exclusion, orders
  `:superclass`/`:disjoint` as illegal, and does not exclude modules), while
  `raise Object.new` is **silent** (instance path excludes the unexact
  carriers) and instance-path `:superclass` stays `:unknown`.
- Modules fire here but are hard-excluded in `shadowed-rescue-clause`
  certification — opposite polarities in the same release, both correct, both
  looking like a bug until you read both collectors.

If these asymmetries are contract (they seem deliberate: a `singleton(Object)`
operand is exact knowledge, an `Object`-typed instance is not), a short spec
per row — `raise Object` fires / `raise Object.new` silent / `rescue Kernel`
never certifies — would make them survive refactors. Today they are implied by
implementation structure, not pinned by name.

## 5. Pin duplicate-hash-key's label rendering split

Verified: symbol/string keys canonicalize in the message (`:a` even for `a:` /
`:a =>` source; `"a"` via inspect), but Integer/Float/bool/nil keys render the
**raw source slice of the repeat node** — `{ 1.0 => x, 1.00 => y }` reports
key `` `1.00' `` (and the two DO collide, `Float#eql?` on the same f64). The
code comments say the slice is intentional; a spec asserting exactly the
`1.0`/`1.00`-collide-and-label case would keep a well-meaning "canonicalize
all labels" cleanup from changing emitted messages.

## 6. Oracle-verified fixture matrices upstream may want as specs

The port's differential harness now carries compact fixtures whose expected
outputs were generated from (and byte-verified against) the RC. Where upstream
spec coverage is thinner than the matrix, these are ready-made:

| fixture (this repo, `harness/corpus/`) | pins |
|---|---|
| `44/45_duplicate_hash_key*` | kind partition (`1` ≠ `1.0`, `"a"` ≠ `:a`), 1.0/1.00 collision, splat-neither-creates-nor-rescues, KeywordHashNode args, nested-literal isolation, triple-dup all-cite-first |
| `46/47_return_in_ensure*` | proc-fires vs lambda/`->`/define_method-barrier vs plain-block-fires; nested begin/ensure single-count; toplevel form |
| `48/49_suppression_*` | per-token diagnostics sharing the comment anchor, family/alias/non-check-family known-token set, self-suppression |
| `50_mutation_widening` | the load-bearing NEGATIVE control: no mutation ⇒ always-truthy must still fire; mutator-in-nested-case-in-block widens; `freeze` (pure self-returner) does not widen |
| `52_scalar_key_hash_shape` | last-wins keeps first position/last value; hashrocket vs colon rendering split |
| `53_kernel_constant_folding` | the 4096-byte `STRING_FOLD_BYTE_LIMIT` boundary (1000-byte folds exact, ~5000 declines), `%%`, malformed-directive decline |
| `54/55_raise_non_exception*` | the full verdict matrix incl. kwargs-vs-braced-hash (`raise(a: 1)` silent, `raise({a: 1})` fires) and all-illegal union |
| `56/57_shadowed_rescue*` | module exclusion, project-class-without-superclass opacity, multi-class partial coverage, nested-begin isolation, multi-earlier " and " message |

## 7. Method note: a second implementation is a cheap oracle for both sides

The differential harness caught bugs in **both** directions this arc: two
rigor-rs FPs (the never-ported MutationWidening; a short-name RBS-index
superclass pollution our shadowed-rescue probes exposed), and items 1–3 above
on the reference side. None of items 1–3 required reading rigor's internals to
*find* — only cross-checking two implementations' outputs on generated edge
matrices. If upstream ever wants a fuzz-ish differential gate, the probe
corpus in this repo's `docs/notes/20260716-v030-*.md` specs is the seed list.
