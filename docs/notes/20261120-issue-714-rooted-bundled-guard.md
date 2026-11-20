# #714 — a rooted guard operand naming a bundled class narrows again (PR #716)

Filed off PR #708's fix-confirmation sweep: with round 6 of #672
(`ecf1d835`) on master, `fp_audit --gaps` on dependabot-core showed +3
`call.undefined-method` rows vs pre-#708 master —
`bundler/helpers/{v2,v4}/lib/functions/version_resolver.rb:48:41` and
`bundler/lib/dependabot/bundler/file_parser.rb:317:28`, all
`undefined method 'revision' for Bundler::Source::Git` inside an
`instance_of?(::Bundler::Source::Git)` then-arm.

## The mechanism (and why the fixtures could not see it)

The reference's `rooted_class_predicate_name` answers a rooted operand
through `class_known_to_scope?` — an OR of
`environment.class_known?` (bundled RBS + project `sig/`) and
`scope.discovered_classes.key?` (in-source decls + meta-new writes).
The port's `guard_constant` rooted arm answered the two surfaces in
SEQUENCE: `knows_discovered_class` first (assert-only, no narrowing),
`constant_names_a_known_class` second (mintable). Dependabot's
`monkey_patches/git_source_patch.rb` reopens
`module Bundler; class Source; class Git` in-source, so the per-run
census holds the qualified name and the RBS-known path was demoted —
the guard's narrowing (and every row it fires) silently died. A
single-file probe is blind to it: the census is per-RUN, so the
demotion only engaged with the monkey patch in the same run. The
harness fixture cannot hold the shape either — it needs one file to
declare a bundled-RBS path and another (or the same, see below) to
guard on it.

## The fix

The rooted arm consults `constant_names_a_known_class` FIRST (mint),
with `knows_discovered_class` demoting only project-only names to
assert-only; `knows_class`'s leaf key stays out of both arms (round 6:
`class A; class M` never shadows `::M`), and
`resolved_static_constant`'s one-segment shadow decline (rounds 7–8)
is untouched. Blast radius beyond the 3 rows, verified at review with
no FP: top-level single-segment reopenings (`class String` +
`is_a?(::String)`) and project-`sig/` qualified names also regain the
mint.

## Measured

- Standing sweep (8 corpora, release): **0 FP both sides**; gap map
  `215/138/59/18/8/2 → 212/138/59/18/8/2` — full gap-dump diff shows
  EXACTLY the 3 brief rows gone, 0 new, 0 moved (master-with-#708
  restored to the pre-#708 `v0.4.7` numbers).
- Fixture 175 gains r35/r36 (in-source `module URI; class HTTP` +
  `is_a?`/`instance_of?(::URI::HTTP)` — the reference narrows both
  shapes; a one-file declaration reproduces the per-run census
  coupling) and r37 (must-still-fire control: `::File::Stat` with no
  in-source decl). Full-fixture probe: both engines byte-identical, no
  pre-existing row moved; snapshot regen 1 written / 173 unchanged.
- One caveat recorded for future reviews: `fp_audit` keys on
  (path, line, col, rule) without messages, so the 3 rows' message
  identity was hand-probed (full tuples).

## Follow-ups

- #717 — a minted guard inside a compound condition does not narrow
  its right side (`x.is_a?(C) && x.frob`, `… && true`): pre-existing,
  decl-independent.
- #718 — consumers that never read the minted nominal: safe-nav reads,
  `while` bodies, loop-pass values, `||` union guards: pre-existing,
  decl-independent.
- #709 already tracks the family-level remediation: the port keeps two
  guard-operand resolvers where the reference resolves through one
  (`rooted_class_predicate_name` / `lexical_class_name`); the two-path
  drift is exactly how this defect landed (the `when`/`===` path was
  order-free all along).
