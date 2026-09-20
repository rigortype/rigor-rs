# The 26 qualified-surface holes at `v0.3.8` — three closure defects, not one ordering bug

**2026-09-09.** Pin `ffb456b0` (`v0.3.8`), rbs 4.2.0 / ruby 4.0.5. Follow-up to
[the declared-but-unwitnessed investigation](20260909-declared-unwitnessed-gem-classes.md)
§1, which measured the holes but named the wrong cause.

**Outcome: 26 → 0** on the 1202-class / 185 625-probe diff, with the witnessable
count UNCHANGED at 590 — the holes were closed by making the surface complete,
not by silencing a class. Over the full 1361-class declared set (the parent's
namespaced-only filter hid five more `corrections` rows and three others),
**42 → 9**; the 9 residual are two unrelated mechanisms, recorded in §4.

## 1. The parent note's root-cause sentence is wrong

It reads:

> `ingest_embedded` merges [`overlay/`] **last** — so the port's qualified
> ancestor flattening does not propagate a late overlay reopen down one more
> subclass level. That is an index-side ancestor-closure defect.

There is no ordering bug and no flattening pass to be out of order with.
`ingest_embedded` already loads `core/`+`stdlib/` then `overlay/` (deliberately,
mirroring `rbs_loader.rb`), and the qualified ancestor chain is computed LAZILY
at query time by `qualified_ancestors` — after every merge, always. Nothing about
load order can strand a member.

The 26 holes are **three independent defects**, and only the first has anything
to do with the overlay:

| # | mechanism | holes (namespaced / all-1361) |
|---|---|---|
| A | an ABSOLUTE nested declaration name was qualified as if it were nested | 22 / 22 |
| B | `prepend` was not ingested at all | 1 / 5 |
| C | a module's SELF-TYPE constraint was not part of its own surface | 3 / 6 |

## 2. A — `module ::Kernel` inside `module Gem` became `Gem::Kernel`

`overlay/rbs_shims/rubygems.rbs` writes the `gem` shim as an ABSOLUTE reopen
nested inside the `Gem` module:

```rbs
module Gem
  module ::Kernel
    def self?.gem: (String, *String) -> void
  end
end
```

`qualified_name` built the key from the lexical `enclosing` prefix plus the
node's own namespace path and leaf — but never consulted
`TypeNameNode::namespace().absolute()`. So the decl landed on a phantom
`Gem::Kernel` entry, and `gem` was absent from every qualified chain that passes
through the real `Kernel`.

Why the SHORT map hid it: the short key is the LEAF (`"Kernel"`), so the reopen
DID merge onto the real short `Kernel` entry. That is exactly why
`qualified_class_has_method("Gem::Dependency", "gem")` answered `true` —
`Gem::Dependency`'s short-key walk reaches `Object → Kernel` and finds it there —
while `Bundler::Dependency` answered `false`: its short superclass `Dependency`
is the merged `Bundler::Dependency` ⊕ `Gem::Dependency` composite whose walk
never reaches `Object`, so it fell through to the qualified walk, which had no
`gem` anywhere on it. The parent note read that asymmetry as "the overlay member
did not propagate one more subclass level down"; it is really "the short map
caught the member and the qualified map filed it under a name nothing looks up".

A second, dormant consequence: `short_to_qualified["Kernel"]` held TWO keys
(`Kernel` and `Gem::Kernel`), so `resolve_short_unambiguous("Kernel")` declined
as ambiguous. Its one live caller guards with `classes.contains_key` first, so
nothing observable rode on it — but the fix removes the ambiguity too.

**Fix:** `qualified_name` drops the lexical prefix when `namespace().absolute()`.
Two lines. It is the whole of the 22-hole family.

## 3. B and C — two members the ingest never modelled

**B — `prepend`.** `collect_members` handled `include` and `extend` but had no
`Node::Prepend` arm, so `class LoadError; prepend DidYouMean::Correctable; end`
contributed nothing and `#corrections` read as PROVEN ABSENT on `LoadError`,
`Gem::LoadError`, `NameError`, `NoMethodError` and `KeyError` — five live false
positives, four of them invisible to the parent note's namespaced-only filter.
Modelled as `prepends` / `prepends_written` (twins of the `includes` pair) and
pushed AHEAD of the class in both `collect` and `collect_qualified`, which is
Ruby's MRO. The ordering is unobservable on the current vendored tree
(`Correctable` declares only `corrections`, and no prepending class declares it),
but it is what RBS linearizes, so first-definer-wins stays faithful if that
changes.

**C — module self-types.** `module PPMethods : _PPMethodsRequired` constrains
the module, and RBS folds the constraint's methods into the MODULE'S OWN
definition: the reference's `PP::PPMethods` has exactly its 11 declared methods
plus the interface's `text` / `breakable` / `group`, and **no `Object`
surface at all**. The port recorded the clause nowhere. Modelled as
`self_types_written` (resolved in the OUTER context, like a `< X` super clause)
and consulted by `qualified_class_has_method` as an additional PRESENT source for
the LEAF ONLY — never propagated to an includer, because the reference does not
propagate it either. An interface self type reads the existing
`interface_method_names` table; a class/module self type reads that entry's
flattened qualified chain; an unresolvable one answers "present" (silent).

The port stays deliberately BROADER than the reference on one axis here: it still
gives a self-typed module the implicit `Object` chain. That is the FP-safe
direction (more methods ⇒ fewer absence witnesses) and predates this slice.

## 4. The 9 residual holes (all-1361 set, out of scope here)

Both are pre-existing and unrelated to ancestor closure; neither is namespaced,
so neither is in the parent note's measurement.

- **`OptionParser#set_banner` / `set_program_name` / `set_summary_indent` /
  `set_summary_width` (4).** `alias set_banner banner=` aliases to an
  ATTR-generated writer (`attr_accessor banner`), and `instance_alias_resolves`
  resolves alias targets against `entry.methods` only, never `attr_methods`.
- **`BigMath#tan` / `log10` / `log1p` / `expm1`, `BigDecimal#to_digits` (5).**
  Not present anywhere in the vendored tree — the reference gets them from
  `data/vendored_gem_sigs/` bigdecimal extras the overlay does not carry. A
  vendoring gap, not an index defect.

## 5. Gates

- 1202-class namespaced diff (the parent note's measurement): **26 → 0 holes**,
  witnessable 590 → 590, opaque 612 → 612.
- 1361-class full declared set: **42 → 9**.
- `cargo test --workspace`: 1312 pass / 0 fail (3 new regression tests in
  `rigor-index`, each carrying a `frobnicate_zzz` control so it cannot pass
  against a silenced surface).
- `cargo clippy --workspace --all-targets`: clean.
- `harness/run.rb` + `harness/run_snapshot.rb`: 107 fixtures, **0 unregistered
  FP**, 515/563.
- `fp_audit.py --gaps --sweep`: **0 FP / 9204 files**, 8 of 8 corpora present,
  790 gaps. The 799 → 790 movement is NOT this slice's — see below.
- **rigor-rs self-diff over the same 9204 files** (this binary vs one built from
  `67bc823` with the slice reverted, separate `CARGO_TARGET_DIR`): 152 748
  diagnostics both sides, **0 silenced and 0 newly fired**. The emitted
  diagnostic set is byte-identical, so the gap movement belongs entirely to PR
  #125 (parse-error reporting), which landed on master in between.

### What that means: the slice is DORMANT today, and that is the point

`check_call`'s third disjunct still carries the
`typer.source().is_declaration_only_class(name)` conjunct, so no namespaced RBS
class is witnessed yet — which is exactly why 26 index-level holes could sit
there unmeasured, and why closing them moves nothing. This is ordering 1 of the
parent note's §4.5: **fix the closure first, then narrow the gate**, rather than
narrowing the gate and booking 26 enumerated (class, method) pairs as accepted
exposure. The sharpest of them was inside the candidate slice's own row set —
`Bundler::Dependency.new("a","b").gem("x")`, where two of the seven predicted
rows live, the reference is SILENT, and the narrowed gate would have FIRED.
The sweep could never have caught it: `gem` as an instance call on a
`Resolv::DNS::Resource::IN::*` receiver does not occur in 9204 files.

## 6. Reproduction

Same recipe as the parent note §5 — the reference dumper over
`Rigor::Environment.for_project(root: <empty tmpdir>).rbs_loader`
(`#instance_definition(name).methods.keys` for every `class_decls` key) diffed
against a throwaway `rigor-index`-linking binary piping `(class, method)` pairs
through `qualified_class_has_method`, one `frobnicate_zzz` control per class.
Ruby 4.0.5 / rbs 4.2.0 — the default 4.0.6 has a broken rbs and makes the
reference read as empty. Neither script is committed.
