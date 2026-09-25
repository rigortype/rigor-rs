# `conforms-to` audit on Ruby 4.0 (PR #150, issue #129, ADR-0044)

The first implementation was measured in a cloud sandbox on Ruby 3.3 with a
preload shim for `Ractor.main?` and `Array#rfind`. This audit re-ran it on the
maintainer host, pin `e59b7b89`, Ruby 4.0.6, rbs 4.2.0, and the `rbs-inline`
gem absent.

**Method.** There were 47 hand-built projects. Each ran in a fresh temp cwd:
first the reference with `--no-cache` (checkout plugin path pinned), then
`rm -rf .rigor`, then `target/release/rigor` in the same dir, so both tools
report the same absolute paths. For each project the diagnostics JSON and
the exit code were compared: path, line, column, rule, severity, message,
`source_family`, and order.

## What the Ruby 3.3 claims missed: four FP families

Every family below produced rows only the port emitted.

| family | probe | reference | port before the fix |
|---|---|---|---|
| a generic reopen fails the build (`GenericParameterMismatchError`) | `module Enumerable`, `class Set` reopened without parameters | silent on them and on `Hash[..]` / `Struct[..]` subclasses | 4 rows |
| a module's surface is its self types' OWN definitions | module requiring `==` / `!` | lists them missing | shorter list (reads `BasicObject` through `Object`) |
| "present when unsure" | any undecidable member | — | shortens the list instead of silencing |
| a file reached twice | `signature_paths: [sig, ./sig]`, `[sig, sig/sub]` | once (expanded-path set) | twice |

**Root causes.**

- The first two are `RBS::DefinitionBuilder#build_instance` semantics that
  the port did not model. It reused `qualified_class_has_method`, which is
  built to over-approximate presence.
- The fix ports `build_instance` / `define_instance` as a separate walk that
  returns `Option<bool>`. `None` silences the whole row.
- It also compares type-parameter shapes across every declaration, and
  dedupes project files by `File.expand_path`.

## After the fix

**No probe has a port-only row.** Each remaining difference is a
reference-only row, i.e. a gap on the safe side:

- Quarantine ordering. The port marks BOTH colliding project files suspect,
  where the reference drops only the later one in sorted order. A redeclared
  interface also turns ambiguous, which silences every class using it.
- A class also declared in a file that the reference quarantines for
  duplicating bundled RBS.
- An interface diamond include (`_Ab` includes `_Za`, and `_Ord` includes
  both).
- `module Comparable`, whose self type is an interface the port resolves in
  the outer context.
- A module aliasing a `BasicObject` method (`alias eq ==`). The reference
  resolves it through `self_type_methods`; the port counts the class as
  unbuildable.
- Tier B rows (`#initialize` arity against `_Mix`). Tier B is not ported.
- The load-set guard: `libraries:`, `.bundle/config`, an unbundled plugin.

Matched exactly: the issue example; member order (last include first, alias
after its target); nested, `::` and namespace resolution; the catalogue
loading per declaration; the `strict` / `lenient` profiles,
`severity_overrides:` and family keys; `disable: [all]`; stream order after
the per-file rows; stdlib-only interfaces (`_ToJson` unresolved, `IO::_Reader`
resolved); rbs collections; `extend` vs `include` / `prepend`; operators and
setters in messages; overlapping `signature_paths:`.

## Gates (this host)

| gate | result |
|---|---|
| `ruby harness/snapshot.rb --check` (project-fixture `lib.rb`) | all 112 up to date before the fix; pre-existing snapshots byte-identical |
| `ruby harness/run.rb` live | 0 unregistered. The fixture-103 extras from the 3.3 run do not reproduce: they were a `RUBY_VERSION < "3.4"` host artifact |
| `ruby harness/run_snapshot.rb` | PASS, 0 unregistered |
| `fp_audit.py --gaps --sweep` | 0 FP / 9,337 files / 3,829 gaps, identical to master (configless, so the scan is gated off) |

The fixture was renumbered from `129_` (the issue number) to the next free
number, `112_`. It gained `d_surface.rbs` (the module row, plus the
`ClsEq` / `SetSub` silences), for 8 rows in total.

## Second round: seven families the 47 projects missed, and the allow-list

An adversarial review of the fixed branch reproduced seven more families of
port-only rows on the same host. Every one is a case where the reference's
environment or build differs from what the port assumed:

| # | family | reference | port (first round) |
|---|---|---|---|
| 1 | an RBS error the port did not model (variance, duplicate `@x`, duplicate interface member, `\| ...` with no base, `include Integer`, `< Kernel`, wrong type-argument count, an included module whose self type does not build) | silent (`instance_definition` is `nil`) | a missing-member row |
| 2 | an interface whose build fails (`include _Each` without arguments, `\| ...`, `[out T]` misuse) | "not loaded" | a missing-member row |
| 3 | the load set (`prism`, `rbs`, `rdoc` via rbs; the host's `bigdecimal` gem) | `Prism::_Visitor` resolves; `RDoc::Constant#value` exists | "not loaded"; `#value` missing |
| 4 | declaration order of bundled classes (292 of 1,361 reopens displaced) | `OptionParser` first | `JSON` first |
| 5 | the file walk | `Dir.glob`: no dot-files, no symlinked dirs | loaded them |
| 6 | `--config conf/custom.yml` with `signature_paths: [sig]` | reads `conf/sig` | read `sig` |
| 7 | `target_ruby: "3.2"` | one `configuration-error`, exit 1 | rows, exit 0 |

This round's own probes found four more, all port-only:

- **Nested include order.** `build_interface` lists the ANCESTORS pre-order
  (the last include first, each followed by its own ancestors): `_I`
  including `_B` including `_C` lists `b, c, i`. The port listed `c, b, i`.
- **Stub interfaces.** `stub_missing_referenced_types` declares an empty
  interface for a `_Missing` written in a project method type (not in
  `initialize`, which `validate_type_params` skips). The reference then
  resolves it and reports nothing; the port said "not loaded".
- **An interface under an undeclared namespace** (`interface Nope::_I`)
  fails `ensure_namespace!` upstream, so it reports "not loaded"; the port
  listed the member.
- **A duplicate declaration only the reference can see.** A project file
  declaring `Prism::VERSION: String`, or `class RDoc` against the rbs gem's
  `module RDoc`, is quarantined upstream (`rbs.coverage.quarantined-signature`)
  and its directives vanish; the port, which does not load prism or rdoc,
  kept them (3 port-only rows over 2 probes).

And one silence the first round had wrong in the other direction: an
interface DIAMOND does not raise. `interface_methods` keys its hash by
`Ancestor::Instance`, whose equality ignores the include that reached it,
so `_Za` reached twice is imported once (`am, zm, aa`).

**The fix** flips the design to an allow-list (ADR-0044 § "The
allow-list"): the port records its own model of every declaration and fires
only when the whole build closure is provably buildable and every name it
resolves provably resolves the same way upstream. Family 3 is a generated
list (`harness/conformance_load_set.rb`, 897 names: 639 reference-only
declarations, 151 type aliases, 103 constants, 2 class aliases, the
`BigDecimal` / `BigMath` surfaces); a project file declaring a listed name
counts as possibly quarantined. No core class and no bundled interface
differs, so the list costs no ordinary row. Family 4 silences every class first declared outside a
project file. Families 5–7 are fixed where they live: the project walk is
`Dir.glob` (including case folding on a case-insensitive volume, measured:
`b.RBS` loads on this host), relative `signature_paths:` resolve against the
config file's directory, and the scan stands down on a `target_ruby` outside
3.3 / 3.4 / 4.0 / `latest`.

Two facts the modelling rests on, both read in rbs 4.2.0 and probed:

- `validate_type_params` runs only for an entity that is BUILT
  (`build_instance`), not one merely included. A generic reopen (`class
  Set`) therefore poisons every build whose own method types name `Set`, but
  not `Array` through `Enumerable#to_set` (oracle: `class C < Array[Integer]`
  still fires beside `class Set`).
- The reference's `InstanceVariableDuplicationError` fires only when the
  FIRST two non-attribute insertions of a name share a declarer; a third
  defuses it (`@x` in a superclass and twice in the class builds). The port
  asks for all to be distinct, a gap on the safe side.

### Tallies (release build, one fresh cwd per project, `--no-cache`)

| probe set | projects | identical | gap-only | exit code only | port-only rows |
|---|---|---|---|---|---|
| first round (`probes1`–`6`) | 54 | 29 | 24 | 1 | 0 |
| reviewer (`pa`–`pj`, all-classes, all-interfaces) | 118 | 66 | 51 | 1 | 0 |
| this round (`q1`–`q6`) | 50 | 29 | 18 | 3 | 0 |
| **total** | **222** | **124** | **93** | **5** | **0** |

The five exit-code projects carry no port row: the reference
crashes on `prepend _Z`, rejects `target_ruby` (`"3.2"` twice, `"x"` with
exit 64), or exits 1 on its rows in the unbundled-`rigor-rails` stand-down.
No common row changed order.

| gate | result |
|---|---|
| `cargo test --workspace` | pass (1,335 tests, 1 ignored: the load-set dump) |
| clippy 1.88, fresh target dir, lib and `--all-targets` | clean |
| `snapshot.rb --check` / `run_snapshot.rb` / `run.rb` live | up to date / 0 unregistered / 0 unregistered; fixture 112 8/8 MATCHED, byte-identical |
| `fp_audit.py --gaps --sweep` | 0 FP / 9,337 files / 3,829 gaps, identical to master |
| `conformance_load_set.rb --check` (+ `--plugin activesupport-core-ext`) | OK, 897 names (plugin adds none) |
