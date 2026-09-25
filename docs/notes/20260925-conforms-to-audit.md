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
