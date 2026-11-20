# #672: rooted class-guard operands stay top-level (PR #708)

## Problem

The lowering renders a rooted `::C` unrooted (`ConstantRead.name` is the lenient
string; `rooted` is a separate flag), so `resolve_constant_as_written`'s
`strip_prefix("::")` arm was dead: a rooted class-guard operand walked the
lexical ladder — `x.is_a?(::Stat)` inside `class File` narrowed to `File::Stat`
and reported `call.undefined-method` the reference never emits.

## Change

The reference's two rooted arms (`Narrowing.rooted_class_predicate_name`,
`lexical_class_name`) ported across the three consumers (`is_a?`/`kind_of?`/
`instance_of?`, `case`/`when`/`in`, `===`):

- A rooted operand answers `C` only when an authoritative surface knows the
  top-level class, declining outright otherwise — no scopes built, the subject
  keeps its carrier and its slot records on both edges
  (`RootedOperand::{Predicate, CaseEq}`).
- `class_edge` resolves a rooted operand's bare name when `guard_constant`
  declines; `class_ordering` orders only on `toplevel_classes`; the rooted
  shadow-decline reads declaration names only (`discovered_decls`), and is
  single-segment so a qualified `::Outer::Inner` keeps narrowing.
- The index grew the reference's `discovered_classes` census: meta-new factory
  writes at every position (`K = Struct.new`/`Class.new`/`Data.define` under
  or/path/`self::`/`::` writes, eval-block selves, `begin/rescue` and
  `while false` carriers), nested `class A; class M` declarations, and
  synthesized `::` prefixes.

## Measured outcome

- All brief rows + controls at parity on the live reference (fixture 175 pins
  r1–r12); each review round re-probed its rows on both engines.
- Sweep (`fp_audit --gaps --sweep`) **0 FP** on `fc10b277`, `d714dcf0`, and the
  final merged head `270bd782` (CI run 44690340700 green; `mergeStateStatus`
  CLEAN). Final gap map `215/138/59/18/8/2` vs master's `212/138/59/18/8/2`:
  the +3 `call.undefined-method` rows are dependabot-core
  `instance_of?(::Bundler::Source::Git)` guards that round-7's
  `knows_class → knows_discovered_class` swap over-declines (the leaf key was
  the bug; the bundled-name surface went with it) — coverage loss only,
  filed with rows + direction as **#714**.
- The review loop ran eight adversarial rounds — past review.md's two-round
  cap; the cap's escape (file everything outstanding, land) is what closed the
  arc: **#709/#710/#714** carry the filings.

## Oracle invocation (machine note)

The v0.4.8 oracle needs **mise ruby 4.0.5 first on PATH** (floor: ruby >= 4.0,
host gems rbs 4.2.0 / prism 1.9.0). A mise-shim `python3` rebuilds PATH inside
its own env and silently runs the reference under system ruby 2.6 — every
corpus then reports INVALID, not FP-free. Sweep with
`PATH=<mise>/installs/ruby/4.0.5/bin:$PATH <mise>/installs/python/latest/bin/python3 harness/fp_audit.py --gaps --sweep`.
