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

Fixture `harness/corpus/112_string_lookup_fold_guarded_arg.rb` has 40 firing
rows and 13 silent ones, every one oracle-measured at the pin.

| section | shape | oracle | master | branch |
|---|---|---|---|---|
| 1 (s1-s14) | literal lookups folding to a String / Integer | fires `for "a"` / `for 1` | fires `for String` / `for Integer` | fires, same row ✓ |
| 2 (n1-n9) | literal lookups folding to nil | fires `call.undefined-method` for nil | fires `for String` / `for Integer` | fires, same rule ✓ |
| 2 (n10) | `"abc"[99].upcase` | fires | silent (gap) | fires ✓ new |
| 3 (t1-t3) | `if "abc"[99]` / `.index("z")` / `"abc"[0]` | always-falsey / truthy | silent (gap) | fires ✓ new |
| 3b (d1-d5) | Float index, Regexp, Range, multibyte | fires on the folded value | fires | fires (fold declines) ✓ |
| 4 (g1-g13) | class-guarded parameter, nilable return | **silent** | fires (FP) | silent ✓ |
| 5 (c1-c8) | precise write, rebind, non-nilable, erasure family, a25/a31 | fires | fires | fires ✓ |

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

## Corpus evidence

The standing sweep set's paths are the maintainer's machine, so `fp_audit.py
--sweep` cannot run in this container. Six of the eight members are on GitHub
and were shallow-cloned at their current HEADs: `mastodon/app`, `mail`,
`TheAlgorithms/Ruby`, `concurrent-ruby`, `net-ssh` and `haml/lib`.
`gitlab-foss` (gitlab.com) and `dependabot-core` were not cloned.

* **Port-vs-port diff**, with release binaries of master and of this branch run
  over the same explicit file lists (2220 files): **0 removed, 0 added,
  0 message changed** on every corpus. The diff tool is not vacuous. Pointed at
  fixture 112, it reports exactly the 10 section-4 rows removed and the 4 new
  rows added.
* **`fp_audit.py --gaps`** over the same six directories, against the pinned
  reference, reports **0 FP candidates on every corpus**:

  | corpus | files | reference | rigor-rs | matched | FP |
  |---|---|---|---|---|---|
  | TheAlgorithms/Ruby | 192 | 34 | 14 | 14 | 0 |
  | haml/lib | 52 | 9 | 5 | 5 | 0 |
  | net-ssh | 181 | 145 | 125 | 125 | 0 |
  | concurrent-ruby | 345 | 5812 | 5715 | 5715 | 0 |
  | mastodon/app | 1254 | 452 | 431 | 431 | 0 |
  | mail | 196 | 8501 | 8488 | 8488 | 0 |

  The port's output on these files is identical to master's (the diff above),
  so no per-corpus matched count can have gone down. These are shallow clones
  of today's upstream HEADs, not the maintainer's checkouts, so the counts are
  not comparable to `harness/CORPUS.md`'s baselines.

## Gates

* `cargo test --workspace`: PASS (1320 passed).
* `cargo +1.88.0 clippy --workspace --locked -- -D warnings`: clean, and with
  `--all-targets` too.
* `ruby harness/run.rb`: **PASS**, 597 matched, 47 gaps, 0 unregistered. That
  is 557 + fixture 112's 40 rows, with the gap count unchanged, so no existing
  row was lost.
* `ruby harness/run_snapshot.rb`: PASS with the same numbers.
* `ruby harness/snapshot.rb`: 2 written (112 is new; 106's trailer grew by four
  lines, a line shift only), 110 unchanged.
* `python3 harness/docs_check.py`: PASS.
