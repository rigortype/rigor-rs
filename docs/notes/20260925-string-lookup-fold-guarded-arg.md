# A class-guarded argument stops pinning the nilable RBS slot (issue #121)

Closes [#121](https://github.com/rigortype/rigor-rs/issues/121), residue 1 of
[`20260909-generic-dispatch-untyped-arg.md`](20260909-generic-dispatch-untyped-arg.md).
The issue prescribed an order, and this change follows it:

1. teach the fold core `String#[]` / `#slice` / `#byteslice` / `#index`;
2. let tier 3 give up the flat nilable slot, gated on something other than
   untypedness;
3. after that, the class-guard exclusion in the untyped allow-list no longer
   decides this shape, so it stays exactly as it is.

Every row was measured against the PINNED reference at `e59b7b89` (`rigor
0.3.9`) on ruby 4.0.5 with rbs 4.2.0. Each probe ran in its own fresh temp cwd
with `--no-cache` and both `-I` libs (`UPSTREAM.md` hazard 1). Before any probe
counted, `ruby harness/run.rb` on master reproduced the committed baseline:
557 matched, 47 gaps, 0 unregistered.

## Step 1 — the fold core learns the four lookups

The reference folds all four by running the real method on the literal
(`STRING_BINARY` has `index`; the `String` catalog admits `[]`, `slice` and
`byteslice`; the ternary path goes through the catalog alone). Measured, it
folds every arity, `nil` included: `"abc"[99]` → `nil`, `"abc"[4, 1]` → `nil`,
`"abc".index("", 4)` → `nil`, `"abc".index("c", -1)` → `2`. It also folds a
Float index (`"abc"[1.5]` → `"b"`), a Regexp, a Range and a multibyte receiver
(`"héllo"[1]` → `"é"`, `"héllo".byteslice(1)` → `"\xC3"`).

`folding::fold_str_lookup` covers the part the core can do byte-exactly:

| call | arity folded |
|---|---|
| `[]`, `slice` | `(Integer)`, `(Integer, Integer)`, `(String)` |
| `byteslice` | `(Integer)`, `(Integer, Integer)` |
| `index` | `(String)`, `(String, Integer)` |

The receiver and every String argument must be ASCII, so a character offset is
a byte offset and `byteslice` is the same operation as `slice`. Anything else
declines, and the RBS answer lands on the same row exactly as before:

* a Float index or offset (Ruby truncates it);
* a Range or Regexp argument (neither is a `Scalar`);
* a multibyte string;
* every argument kind Ruby raises on (`byteslice("a")`, `[nil]`, `index(1)`).

Declining is never a lost row. Before this change every such call already
typed as the bare `String` / `Integer`, and the reference's folded value lands
on the same `(rule, line, column)`.

What the fold changes, measured:

* The seven `call.undefined-method` rows in the issue's brief keep their
  `(rule, line, column)`. Only the rendered receiver moves (`for "a"`,
  `for nil`, `for 1`), and it now matches the reference's text.
* A folded `nil` fires **`call.undefined-method` for nil**, not
  `call.possible-nil-receiver`. That is the reference's rule, and fixture 112
  section 2 asserts it by rule id. `"abc"[99].upcase` is a new matched row,
  because the bare `String` answer knew `upcase`.
* `if "abc"[99]`, `if "abc".index("z")` (always falsey) and `if "abc"[0]`
  (always truthy) now report `flow.always-truthy-condition`. All three are new
  matched rows that closed coverage gaps.

## Step 2 — a guarded parameter gives up the flat nilable slot

Tier 3's decline (`rbs_dispatch_declines_on_untyped_arg`) gains a second arm.
When the flat slot is NILABLE (every overload returns `C?`), an argument
qualifies if `Typer::arg_is_guarded_parameter` holds:

* the argument is a bare local read;
* once guards of the exact shape `root.is_a?(C)` / `kind_of?(C)` /
  `instance_of?(C)` are stepped over (`local_reach`'s new `skip_class_guards`
  mode), only the untyped carrier reaches the read.

Why that is the right gate. The reference narrows such a parameter to the
guard's `Nominal` (or leaves it `Dynamic` where the guard does not dominate the
read), and it can never constant-fold that. So the call's carrier is the RBS
join, and with every overload returning `C?` the join is `C | nil`, on which no
negative rule fires. That includes a method `C` HAS: `"abc"[u].upcase` under
the guard is reference-silent, with no possible-nil-receiver either. So the
decline to `Dynamic[top]` loses nothing the port fired on before.

Why the gate is narrower than "the argument is guarded":

* **A precise write reaching the read** (`u = 1; return unless
  u.is_a?(Integer)`) can leave the reference a `Constant` to fold, and it fires
  (`"abc"[u]` → `"b"`, control c1). The Kernel-fold rows a25/a31 ride the same
  refusal, and they are untouched: that allow-list is not edited.
* **A chain over the root** (`"abc"[u <=> 1]`) can answer a union of literals,
  which the reference does fold.
* **Only the nilable case asks.** A typed argument can narrow the erasure
  family (`Array#product`, `#zip`, `String#scan`) back to overloads the
  reference fires on (controls c4-c6).

## Rows

Fixture `harness/corpus/112_string_lookup_fold_guarded_arg.rb` has 45 firing
rows and 19 silent ones, every one oracle-measured at the pin. Sections 6 and 7
were added by the local verification pass and the adversarial review below.

| section | shape | oracle | master | branch |
|---|---|---|---|---|
| 1 (s1-s14) | literal lookups folding to a String / Integer | fires `for "a"` / `for 1` | fires `for String` / `for Integer` | fires, same row ✓ |
| 2 (n1-n9) | literal lookups folding to nil | fires `call.undefined-method` for nil | fires `for String` / `for Integer` | fires, same rule ✓ |
| 2 (n10) | `"abc"[99].upcase` | fires | silent (gap) | fires ✓ new |
| 3 (t1-t3) | `if "abc"[99]` / `.index("z")` / `"abc"[0]` | always-falsey / truthy | silent (gap) | fires ✓ new |
| 3b (d1-d5) | Float index, Regexp, Range, multibyte | fires on the folded value | fires | fires (fold declines) ✓ |
| 4 (g1-g13) | class-guarded parameter, nilable return | **silent** | fires (FP) on 10; g6/g8/g12 already silent | silent ✓ |
| 5 (c1-c8) | precise write, rebind, non-nilable, erasure family, a25/a31 | fires | fires | fires ✓ |
| 6 (i1-i3) | literal index/offset past `i32` | fires on the real value | fires `for String` | fires, same value ✓ |
| 6 (f1-f3) | `&.` on a folded nil / on `nil` | silent | fires (FP) | silent ✓ |
| 6 (f4) | `"abc"[0]&.m` | fires `for "a"` | fires `for String` | fires ✓ |
| 7 (top level) | a lookup reading a mutated / `+=` / `-=` local | silent | silent | silent ✓ (declines) |
| 7 (p1) | `"abc".center(4097)` | fires `for literal-string` | fires `for String` | fires, same row ✓ |

## Residues — still port-only false positives, recorded not chased

Measured at the pin: reference-silent, port still fires.

1. **`case u when Integer then "abc"[u]`** and **`Integer === u`**.
   `local_reach` refuses both guard shapes outright. Admitting them needs the
   `when`/`===` operand to resolve to a CLASS, not a value constant, because an
   equality narrowing could leave the reference a `Constant` to fold.
2. **A chain over the guarded root** (`"abc"[u.to_i]`). It is excluded on
   purpose, for the reason given above.
3. **A guard after a conditional rebind** (`u = 1 if u.nil?; return unless
   u.is_a?(Integer)`). A precise member reaches the read, so the gate refuses.
   The reference's `Integer | 1` does not fold here, but the port cannot tell
   that apart from a union it would fold.
4. **Unrelated, found while probing:** a chained call after a
   `call.argument-type-mismatch` fires on the port and not on the reference
   (`"abc"[nil].zz`, `"abc".byteslice("a").zz`, and `"abc".index(1).zz`, where
   the reference also reports no mismatch). This is a separate pre-existing
   family, not touched here.
5. **Found by the local pass, all pre-existing and identical on master.** In
   each case the flat slot answers `String` / `Integer` where the reference
   cannot fold. The shapes are:
   * a guarded parameter read through an alias (`v = u; "abc"[v].m`);
   * a mutated local receiver (`s = +"abc"; s << "d"; s[4].m`);
   * an interpolated or adjacent-literal receiver (`"a#{1}c"[5].m`,
     `("ab" "c")[2].m`);
   * an argument kind with no overload (`"abc"["b", 1].m`, `"abc".index(98).m`);
   * a Bignum argument (`"abc"[100000000000000000000].m`, which raises in Ruby).
6. **Message only, pre-existing.** `scalar_inspect` renders a String constant
   with Rust `Debug`, so control characters spell differently from the
   reference (`"\u{1b}"` against `"\e"`, and `"\0"` against `"\u0000"`). The
   fold only makes this reachable through more calls. The gate keys on `(rule,
   line, column)` and is unaffected.

## Local verification pass (2026-09-25, maintainer machine)

The first cut was written in a container that could not run the standing sweep.
It was re-verified locally on ruby 4.0.6 and rbs 4.2.0, with the submodule at
the `e59b7b89` pin. 106 fresh-dir probes against the reference found two defects
in the fold's inputs, and both are fixed here:

* **Integer literals past `i32` lowered to `0`.** Prism's binding exposes only
  `TryInto<i32>`, and the lowering used `unwrap_or(0)`. The fold turned that
  into wrong values: `"abc"[9223372036854775807]` answered `"a"` where Ruby and
  the reference say `nil`. The same bug was already live on master
  (`100000000000000000000.m` rendered `for 0`). `IntegerLit.value` is now
  `Option<i64>` and is read from the digit view. A Bignum is `None`, which types
  as a nominal `Integer` and never pins a scalar, a shape key or a fold.
* **`nil&.m` fired `call.undefined-method`.** The reference's
  `safe_navigation_receiver` turns a receiver that is exactly nil into `bot`, so
  it stays silent, and a `T | nil` union flows through unchanged. The port had no
  such arm. On master this was a position FP for `nil&.m` and
  `"abc"[99]&.m` (`for String`). The fold made the second one read `for nil`.
  It is now ported for the undefined-method rule only. Arity still fires on
  `nil&.to_s(1, 2, 3)`, as the reference does.

An adversarial review of that state (an Opus subagent, about 150 probes)
returned **BLOCK**, and I reproduced its finding. The top-level flat env
(`build_toplevel_env`) keeps a local's FIRST literal across `<<`, `+=`, `-=`,
branch writes and block writes. Before, a lookup on such a local typed as
`String`, so the stale value never changed the receiver's class. The fold can
answer `nil`, so the class flipped. For example, `buf = ""; buf << "hello";
buf[0].upcase` fired `for nil`, and `x = 99; x -= 98; "abc"[x].upcase` fired as
well. The reference and master are silent on both. Two more fixes follow:

* **A lookup that reads a local declines.** If the receiver or an argument
  subtree contains a `LocalVariableRead` (`reads_local`), the RBS answer stands.
  Inside a `def` the env is empty, so nothing is lost there. At top level the
  cost is `s = "abc"; s[5].m` becoming a gap again, as on master.
* **`center` / `ljust` / `rjust` wider than 4096 skip the sidecar.** This ports
  the reference's `string_pad_blow_up?`. The widened integer lowering had
  brought `"abc".center(3_000_000_000)` into reach, and it took 70 s and 10 GB.
  The same problem existed on master with a width of 2e9.

A re-review of the fix returned **MERGEABLE**, with no FP that is new against
master in 22 attempts to reach a stale local without a `LocalVariableRead`
(`(s += "x")[0]`, multi-assign, ivars/globals, mutated constants and others).
It raised two further points:

* **The first cut of the gate scanned the whole arena for every lookup.** That
  is quadratic: 20,000 `x = "abc"[0]` lines took 15.4 s, against 1.2 s on master.
  `LoweredAst` now keeps the sorted start offsets of its local reads, and
  `reads_local_within` is a binary search. The same file now takes 0.9 s.
* **The pad cap is slightly stricter than the reference.** The reference caps
  only the one-argument form, so it still folds `"abc".ljust(4097, "-")`. The
  port declines it too, deliberately. Otherwise the sidecar would build a
  3e9-wide string. The diagnostic keeps its row, and only the rendered receiver
  differs.

The flat env's staleness is older than this PR and reaches other folds too.
`s = "ab"; s << "c"; [1, 2][s.length].succ` is a port-only FP on master. The
real fix is flow-sensitive top-level typing (ADR-0022), which is out of scope
here.

The integer widening also cost four rows that master matched only by accident.
Master lowered a Bignum to `0`, so a Bignum hash key, `big - big` and `if
big_local` happened to fold to the reference's answer. They are gaps now, and
there are no FPs.

The probes also found more port-only FPs at the same position on master. They
are pre-existing, and each is recorded as a residue above.

## Corpus evidence

`python3 harness/fp_audit.py --gaps --sweep` ran over all eight
`harness/sweep-corpora.yml` members with a freshly built release binary. The
result was **0 FP candidates, 9,337 files and 3,829 gaps**. That is identical to
master's baseline (`harness/CORPUS.md`). It was measured three times: on the
branch as it arrived, after the first two fixes, and after the review fixes.

The first cut also ran a port-vs-port diff over shallow clones of six members,
which reported 0 removed and 0 added. The sweep supersedes it.

## Gates

Re-run locally after the fixes:

* `cargo test --workspace`: PASS (1322 passed).
* `cargo +1.88.0 clippy --workspace --all-targets --locked -- -D warnings` in a
  fresh target dir: exit 0.
* `ruby harness/run.rb` and `ruby harness/run_snapshot.rb`: PASS, 602 matched,
  0 unregistered, with the gap count unchanged from master.
* `ruby harness/snapshot.rb` reproduced every committed snapshot on ruby 4.0.6.
  Only 112 was rewritten, for sections 6 and 7.
* `python3 harness/docs_check.py`: PASS.
