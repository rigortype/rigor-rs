# The generic RBS dispatch stops pinning under an untyped argument (issue #118)

Closes [#118](https://github.com/zonuexe/rigor-rs/issues/118) — residue 1 of the
`v0.3.4 → v0.3.8` re-pin
([`20260909-repin-v038-infer-families.md`](20260909-repin-v038-infer-families.md)).
The re-pin ported upstream #521 / PR #537 (`3d5dddbb`) for the Kernel folds only;
this note ports it into the GENERIC receiver dispatch (tier 3 of
`Typer::type_call`).

Everything below was measured against the PINNED reference at `ffb456b0`
(= `v0.3.8`), one fresh temp cwd per run, `--no-cache`, both `-I` libs
(`UPSTREAM.md` hazard 1). `RS` = the port fires and the oracle is silent (a false
positive); `BOTH` = both fire (a must-still-fire control); `REF` = the oracle
fires and the port does not (a coverage gap).

## The issue's premise was half right — the port does not "pin the first"

Issue #118 predicted the port answers "the first arity/block-matching overload in
declaration order". It does not: rigor-rs has no per-call-site overload selector
at all. Tier 3 reads a per-method FLAT SLOT (`CoreData::method_return`) that
`method_signature` collapses under an ALL-overloads-agree discipline — a strictly
more conservative rule than upstream's join over the arity-matching subset, so
the port can never pin an arm the reference's join would widen away.

What it does instead is answer a BARE `Nominal[C]` where the reference's join is
something else. Two mechanisms, both measured, both live:

1. **The nil bit is dropped.** `String#[]`'s four overloads all return `String?`
   — they AGREE, so the flat slot records `("String", nilable: true)` and
   `method_return` hands tier 3 the class alone. The reference answers
   `String | nil`, which no negative rule witnesses on. This is issue #118's own
   row.
2. **Agreement is measured after ERASURE.** `method_signature` compares
   `type_name_str`, i.e. the head class NAME. `Array#product`'s
   `Array[[E, X]]` and `Array[Array[E | U]]` both erase to `Array`, so the slot
   "agrees" on `Array`; the reference translates them to two distinct types and
   joins them to `Dynamic[union]`. This one IS #521's own defect class (its
   commit cites `[true] * n`), reached here through `Array#product`, `Array#zip`
   and `String#scan`.

The reference's own `annotate` shows both, side by side with the port's:

```
                          reference (ffb456b0)                     port b419d6c
"abc"[u]                  String?                                  String
"abc".scan(u)             Dynamic[Array[Array[String?] | String]   Array
                                  | Array[String]]
[1, 2].product(u)         Dynamic[Array[Array[1 | 2 | Dynamic]]    Array
                                  | Array[[1 | 2, Dynamic]]]
[1, 2][u]                 1 | 2                                    Dynamic[top]
"abc".center(u)           literal-string                           String
```

## Why untypedness is the gate, and not nilability

A NILABLE return alone does not make the reference silent. With a LITERAL
argument the reference CONSTANT-FOLDS the call and witnesses on the folded value:
`"abc"[0]` is `"a"`, `"abc"[1..]` is `"bc"`, `"abc".byteslice(1)` is `"b"`,
`"abc".index("b")` is `1` — all four fire on the reference, and the port's bare
`String` lands on the same `(rule, line, column)` row. Even the nil-valued folds
fire: `"abc"[99]` is `nil` on the reference and it still emits
`call.undefined-method` (naming `nil` instead of `String` — the row matches, the
message does not).

That was not a guess. A first cut that declined on EVERY nilable tier-3 return
was built and measured: it closed every false positive below and cost **five
matched rows** in the probe set (`"abc"[0]`, `"abc"[1..]`,
`"abc".byteslice(1)`, `"abc".index("b")`, `"abc".slice(1)`) plus the five
nil-valued twins. The fixture corpus happened not to contain any of them
(487/538 unchanged), which is exactly the kind of green gate that hides a
coverage loss.

So the gate is the SAME reference-untyped allow-list the Kernel folds use
(`Typer::arg_is_reference_untyped`, fixture 99 / 105): an untyped argument is
what makes the reference unable to fold the call, which is what leaves the union
standing. Every literal-argument row above is preserved by construction.

## What was implemented, and where

**`crates/rigor-index/src/rbs.rs`** — `OverloadSignature` gains two fields, both
filled by `method_overloads` at ingestion:

* `block_required: bool` — `mt.block().is_some_and(|b| b.required())`, the port
  of `OverloadSelector.overload_requires_block?` (an OPTIONAL `?{ … }` block does
  not count, matching upstream).
* `return_form: String` — the overload's return type in its verbatim written
  form, whitespace-normalised through the new `normalized_written_form`. An
  unavailable source slice becomes a per-overload sentinel so it never compares
  equal to anything (the conservative direction).

**`crates/rigor-infer/src/lib.rs`** — tier 3 of `Typer::type_call` consults two
new predicates before interning the flat slot's `Nominal[C]`:

* `rbs_join_is_one_bare_nominal(class, method, argc)` — the index half. `false`
  when the flat slot's return is NILABLE, or when the candidate overloads do not
  all declare the same verbatim return. Candidates are the overloads with no
  required keyword (upstream `rejects_keyword_required?`), no REQUIRED block (a
  block-bearing call never reaches tier 3), and an arity envelope admitting
  `argc`. `true` when the method retains no overload shapes, or when no candidate
  matches the arity — the reference then falls back to a single overload
  (`overloads.find { !requires_block } || overloads.first`) and pins it, exactly
  as the flat slot does.
* `rbs_dispatch_declines_on_untyped_arg(…)` — the arena half, run ONLY when the
  index half says the answer is at risk (`arg_is_reference_untyped` is two arena
  scans per argument and tier 3 is the hot path). It answers `true` when some
  argument both types `Dynamic[top]` AND passes the allow-list.

On `true` the call types `Dynamic[top]` instead of `Nominal[C]`.

**Why `Dynamic[top]` and not `Dynamic[union]`.** Upstream wraps the union in
`Dynamic` and says the wrapper is load-bearing (a bare union licenses the
negative rules to fire on arms the runtime never takes). The port cannot build
that union: it has no RBS type translator, which is the whole reason tier 3 reads
a flat slot. Upstream's own rule for that case is to decline — "a candidate whose
return does not translate leaves the join incomplete — decline (fail-soft to
Dynamic downstream) rather than answer a join missing an arm" — so declining to
`Dynamic[top]` IS the ported behaviour. The cost is display precision in
`annotate` / `coverage`, never a diagnostic.

Candidate selection is deliberately PERMISSIVE where the retained shapes are
coarse: a trailing positional does not raise the minimum arity, and no
per-argument type filtering is applied. Admitting an extra candidate can only
turn agreement into disagreement, i.e. make the port answer LESS — FP-safe by
construction.

## Rows

### RS — closed by this change (13 of 13 measured)

| row | shape | oracle | port before | port after |
|---|---|---|---|---|
| g1 | `"abc"[u].typo` — **issue #118** | silent | fires `for String` | silent ✓ |
| g2 | `"abc".slice(u).typo` | silent | fires `for String` | silent ✓ |
| g3 | `"abc".byteslice(u).typo` | silent | fires `for String` | silent ✓ |
| g4 | `"abc".index(u).typo` | silent | fires `for Integer` | silent ✓ |
| g5 | `"abc".rindex(u).typo` | silent | fires `for Integer` | silent ✓ |
| g6 | `"abc".getbyte(u).typo` | silent | fires `for Integer` | silent ✓ |
| g7 | `"abc".byteindex(u).typo` | silent | fires `for Integer` | silent ✓ |
| g8 | `[1, 2].assoc(u).typo` | silent | fires `for Array` | silent ✓ |
| g9 | `[1, 2].rassoc(u).typo` | silent | fires `for Array` | silent ✓ |
| g10 | `(1.5 <=> u).typo` | silent | fires `for Integer` | silent ✓ |
| g11 | `[1, 2].product(u).typo` | silent | fires `for Array` | silent ✓ |
| g12 | `[1, 2].zip(u).typo` | silent | fires `for Array` | silent ✓ |
| g13 | `"abc".scan(u).typo` | silent | fires `for Array` | silent ✓ |

g11/g12/g13 are the erasure family; the other ten are the nilable family. For
g11 and g12 the port's candidate set is byte-for-byte the reference's: at arity 1
`Array#product` keeps `[X] (array[X]) -> Array[[E, X]]` and
`[U] (*array[U]) -> Array[Array[E | U]]`, and `Array#zip` the analogous pair —
exactly the two arms the reference's `annotate` shows it joining.

### BOTH — must-still-fire controls, all preserved

| row | shape | oracle | port after |
|---|---|---|---|
| g14–g18 | `"abc"[0]`, `"abc"[1..]`, `.byteslice(1)`, `.index("b")`, `.slice(1)` | fire on the FOLDED value | fire ✓ |
| g19–g21 | the same with a nil-valued fold (`"abc"[99]`, `"abc"[9..]`, `.index("z")`) | fire `for nil` | fire ✓ (`for String` / `for Integer`) |
| g22/g23 | `"abc".center(u)`, `.ljust(u)` — one matching overload | fire `for String` | fire ✓ |
| g24/g25 | `("abc" * u)`, `("a%sb" % u)` | fire `for String` | fire ✓ |
| g26/g27/g28 | `.delete_prefix(u)`, `.tr(u, "b")`, `.sub("a", u)` | fire `for String` | fire ✓ |
| g29 | `"abc".split(u)` — a block overload the call does not engage | fires `for Array` | fires ✓ |
| g30/g31 | `[1, 2].take(u)`, `.join(u)` | fire | fire ✓ |
| g32 | `{ a: 1 }.merge(u)` | fires `for Hash` | fires ✓ |
| g33 | `1.gcd(u)` | fires `for Integer` | fires ✓ |
| a13 | `"abc".center(u)` — the issue's own control | fires | fires ✓ |

Beyond the fixture, a generated 125-row sweep over the `String` / `Array` /
`Hash` / `Integer` / `Float` receiver surface with an untyped argument
(`[]`, `slice`, `fetch`, `sample`, `dig`, `pack`, `zip`, `combination`, the
operator spellings, …) diffs to **exactly the 13 RS rows going silent** — no
BOTH row lost, no REF row added.

### REF — pre-existing gaps, unchanged

`[1, 2].first(u)` (a14), `[1, 2].sample(u)`, `[1, 2].last(u)`,
`{ a: 1 }[u]`, `{ a: 1 }.fetch(u)`, `[1, 2].fetch(0)`, `[1, 2].dig(0)`,
`"abc".gsub(u)`, `"abc".start_with?(u)`, `[1, 2].each_slice(u)`,
`1.5.round(u)` and the rest of the p7 REF set are untouched: the flat slot
already declined on them before this change.

### Both-silent before and after

`[1, 2][u]` (a16, the issue's row 3), `([true] * u)` — the shape upstream's
commit message cites — and `s.to_s[0]`. The flat slot already declined.

## Corpus evidence

The standing sweep set (`harness/sweep-corpora.yml`, all eight corpora present)
was run with the RELEASE binary built before and after, over the same explicit
file list — **9204 files, 158449 diagnostics before, 158449 after, diff empty**:
0 removed, 0 added, 0 message changed. The change moves nothing on real code,
which is consistent with issue #118's own statement that no sweep corpus
exercises the shape. The diff tool is not vacuous: pointing it at the 125-row
generated probe alongside `haml/lib` reports exactly the 13 rows going silent and
nothing else, so an empty diff on the corpora is a measurement, not a no-op.

`fp_audit.py --gaps --sweep` was deliberately NOT run (since upstream #547 the
`mail` corpus alone takes ~70 minutes); a port-vs-port diff is the right
instrument here anyway, because this change can only ever remove diagnostics and
so cannot add a sweep false positive.

Cost, same binaries, same file lists: mastodon/app 0.73s → 0.75s (1236 files),
gitlab-foss/lib 2.50s → 2.62s (4676 files). The index half of the gate runs
first, so `arg_is_reference_untyped`'s arena scans only happen for a call whose
flat-slot answer is actually at risk.

## Gates

* `cargo test --workspace --offline`: PASS (exit 0).
* `CARGO_TARGET_DIR=/tmp/rigor-clippy-target-118 cargo clippy --workspace
  --all-targets --offline -- -D warnings`: clean (exit 0), fresh target dir.
* `ruby harness/run.rb`: **PASS**, 106 fixtures, **507 matched, 50 gaps, 0
  unregistered extras** — 487/50 at `b419d6c` plus the new fixture's 20 rows,
  with the gap count unmoved.
* `ruby harness/run_snapshot.rb`: PASS, same numbers, exit 0.
* `python3 harness/docs_check.py`: PASS.
* `ruby harness/snapshot.rb`: 1 written (the new fixture), **105 unchanged**.

New fixture `harness/corpus/106_generic_dispatch_untyped_arg.rb` — 13 silent
rows and 20 firing controls, every one oracle-measured at the pin.

## An environment hazard this session paid for

Mid-session the oracle silently changed underneath the probes: a nix garbage
collection removed the ruby the earlier measurements ran on (4.0.5) and the
`rbs` 4.2.0 gem with it, so `ruby` fell through to homebrew 4.0.6 with a broken
`rbs` 4.0.2 native extension. The failure mode is NOT a clean error — the
harness reported `Reference diagnostics: 0` and "507 unregistered false
positives", and a naive `GEM_PATH` repair silently fell back to rbs **3.10.0**,
which regenerated three committed snapshots WRONG (fixture 66's
`expected int | _ToInt` became `expected int`, the rbs-4.1 bounded-type-param
difference; fixtures 82 and 103 each lost a row). Fixture 103 is additionally
HOST-RUBY-dependent — `f15` asserts `RUBY_VERSION == "4.0.5"`.

The recovery, for the next agent who hits it:

```sh
# 1. the pinned rbs (UPSTREAM.md § "The local Ruby must resolve the rbs the pin
#    bundles"), into a private GEM_HOME so nothing global is touched
GEM_HOME=/tmp/gems405 <ruby-4.0.5>/bin/gem install rbs -v 4.2.0 --no-document
# 2. run everything with that ruby FIRST on PATH
export PATH=<ruby-4.0.5>/bin:$PATH GEM_HOME=/tmp/gems405 GEM_PATH=/tmp/gems405:...
ruby -I reference/rigor/lib reference/rigor/exe/rigor --version   # rigor 0.3.8
gem list rbs                                                     # 4.2.0 newest
```

`ruby harness/run.rb` reproducing the documented baseline is the only proof the
oracle is the pinned one. **Check it before believing any probe**, and re-check
it after a surprising snapshot diff — a snapshot regeneration under the wrong
gem is indistinguishable from a real behaviour change in the diff alone.

## Residues

1. **A class-GUARDED argument still fires.** `return unless u.is_a?(Integer)`
   then `"abc"[u].typo` is reference-SILENT (the reference still answers
   `String?`; narrowing the ARGUMENT does not make the RETURN non-nil) and the
   port still fires, because `arg_is_reference_untyped`'s local arm excludes a
   guarded root by design — rows a25/a31 of the Kernel-fold arc depend on that
   exclusion. Twins measured: `.byteslice(u)`, `.index(u)`. Closing them needs
   the nilable union itself in tier 3, gated on something other than
   untypedness, and that costs the ten folded-literal rows above unless the
   port's constant folder learns `String#[]` / `#slice` / `#byteslice` / `#index`
   first. Deliberately out of scope here; it is a PRE-EXISTING false positive,
   not one this change introduces, and it is named in fixture 106's trailer.
2. **The carrier is `Dynamic[top]`, not the reference's union.** `annotate` shows
   `Dynamic[top]` where the reference shows `String?` or `Dynamic[Array[…] |
   Array[…]]`, and `coverage` counts it imprecise. No diagnostic depends on it.
   Building the real union needs an RBS type translator (the flat slot's absence
   is the same gap).
3. **`return_form` compares SOURCE TEXT.** Two overloads spelling the same type
   differently (`::String` vs `String`) read as a disagreement and decline. That
   is a coverage loss in the FP-safe direction; no measured row hits it.
4. **An `(?) -> untyped` overload is not retained** by `method_overloads` at all,
   so it cannot contribute a disagreement. 18 occurrences in the vendored RBS,
   all of them either inside a block clause or on a single-overload method whose
   flat slot already declines — none reachable.
5. **The standing sweep (`fp_audit.py --gaps --sweep`) was NOT run** — since
   upstream #547 the `mail` corpus alone takes ~70 minutes. The corpus evidence
   here is a port-vs-port release-binary diff over the whole standing set, which
   is what this change can move: it only ever removes diagnostics, so it cannot
   add a sweep false positive.
