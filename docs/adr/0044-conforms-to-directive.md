# Read `%a{rigor:v1:conforms-to _Interface}`: presence tier only, silent wherever the reference may not load, build or resolve

Status: accepted 2026-09-25 (issue [#129](https://github.com/rigortype/rigor-rs/issues/129); pin `e59b7b89`)

## Context

A class or module in the project's own RBS can assert that it satisfies a
structural interface:

```rbs
%a{rigor:v1:conforms-to _ClosableStream}
class Gate
  def close: () -> void
end
```

The reference (`RbsExtended::ConformanceChecker`) checks every such directive
once per run and reports two rows, both `:warning` and both positioned at the
ANNOTATION in the `.rbs`, because no Ruby `def` exists to carry the row:

| rule | when | message |
|---|---|---|
| `rbs_extended.unsatisfied-conformance` | the class's RBS definition lacks ≥1 required member | ``… but does not provide required method: `#closed?`. …`` |
| `dynamic.rbs-extended.unresolved` | the name resolves to no loaded interface (#928 promoted it from `:info`) | ``… but interface `_X` is not loaded. …`` |

`v0.3.9` (#976) also started shipping a capability-role catalogue
(`data/capability_roles/capability_roles.rbs`: `_Closable`, `_RewindableStream`,
`_ClosableStream`, `_FileDescriptorBacked`, `_Callable`) in every default run.
It loads after the project's signatures, one interface at a time, skipping any
name the project already declares. The port did not read the directive at all,
so it emitted neither row and left the catalogue un-vendored.

## Decision

**Port tier A (presence) and the unresolved row. Leave tier B (signature
subtyping) unported. Vendor the catalogue and load it per declaration.** Where
the port cannot prove the reference loaded, built and resolved the same
things, it stays silent. A missed row is a coverage gap; an extra row is a
false positive.

### What the scan is, measured on the oracle (fresh temp cwd per probe, `--no-cache`)

1. **Gate.** The scan runs only when the config CONFIGURES `signature_paths:`
   (`PoolCoordinator#project_signature_paths?`). The default `sig/` is loaded
   but never scanned, so a configless run (every sweep, every existing fixture)
   cannot change.
2. **Stream.** The rows come after the per-file stream, in `env.class_decls`
   order: a class's first declaration first (bundled before project, project
   files sorted by path), then each of its declarations' annotations in order.
   The severity profile re-stamps them (`strict` makes the unsatisfied row an
   error), and so do `severity_overrides:`, family keys included. `disable:`
   does NOT drop them, not even `disable: [all]`, because the reference's
   `disable:` filter only sees the per-file stream.
3. **Required members** follow `RBS::DefinitionBuilder#build_interface`
   (pinned with rbs 4.2.0): the included interfaces' members first, LAST
   include first and recursively; then the interface's own members in
   declaration order, with an alias's target placed before the alias.
4. **Provided members** are what `RBS::DefinitionBuilder#build_instance`
   returns, ported as its own walk (`rbs_instance_has`): own defs (private
   ones included), `self?` defs, attributes and aliases, then included and
   prepended modules and interfaces recursively (`define_instance`). A class
   adds its superclass's whole definition. A module adds only its self
   types' OWN definitions, default `Object`, which brings `Kernel` but not
   `BasicObject`: `#==` and `#!` are missing on a module. A member the walk
   cannot decide (an unresolvable reference, an unknown entry) silences the
   whole row, because a shorter or longer list is as wrong as a spurious
   row. The shared `qualified_class_has_method` is NOT used: it answers
   "present" when unsure, which is safe for an undefined-method witness but
   shortens the list here.
5. **Resolution** follows the directive's class namespace prefixes, longest
   first, then the bare name, with a leading `::` stripped.
   `Outer::_I` is not visible from a top-level class.

### Silences the port must reproduce (all oracle-measured)

| the reference is silent because… | port mechanism |
|---|---|
| the class's definition fails to build: a member (def / attr / alias name) declared twice across its declarations, core reopens included (`class String; def upcase: …`); an alias to nothing; an unknown superclass or mixin; a failure on any ancestor | `ConformanceBuilder` records every instance member name per class across ALL sources. A duplicate, or a superclass written two ways, marks the class unbuildable, and so does any unbuildable class on its ancestor chain. Aliases are resolved at scan time. An incomplete chain reads as "present". |
| the whole `sig/` FILE is quarantined (`add_project_parsed_decls` rescues `DuplicatedDeclarationError`): a class/module kind clash, or a redeclared interface, type alias, constant (top-level or nested) or global | Any duplicate of those kinds marks the file suspect, and the prior declarer too when it is a project file, since the port's `read_dir` order is not the reference's sorted order. Suspect files lose their annotations; their classes turn unbuildable and their interfaces ambiguous. |
| a class/module reopened with different type parameters (`class Set` against core's `Set[unchecked out A]`: `GenericParameterMismatchError`), and every class below it (`Hash` / `Struct` subclasses through `Enumerable`) | each declaration's `(variance, unchecked)` shape is compared with the first one's, across all sources; a bound or default on either side counts as a mismatch |
| an interface whose definition fails to build (the reference then reports "not loaded") | the port stays silent instead, which is a gap |

The reference collects project files into a set of expanded paths, so a file
reached through two `signature_paths:` entries (`[sig, ./sig]`,
`[sig, sig/sub]`) loads once. The port dedupes the same way.

Two things do NOT silence the row, and the port matches both: an overloading
reopen (`def to_s: … | ...`) and a singleton-side duplicate.

### Load-set guard (port-only)

"Not loaded" is a false positive if the reference's environment holds an
interface the port never reads. A presence row is a false positive if that
environment gives the class a member the port cannot see: `libraries: [json]`
reopens `Object`. So the whole scan stands down when the config names any of:

- `libraries:`, `bundler:` or `includes:`;
- a `.bundle/config` or `vendor/bundle/`, which trigger the gem-`sig/` walk;
- a plugin the port does not bundle.

A bundled plugin keeps the scan on; the oracle agrees with activesupport
loaded.

### Scope boundaries

- The catalogue feeds ONLY the conformance interface table. It stays out of
  the leaf-keyed `interface_method_names` that
  `call.argument-type-mismatch` reads, so no other rule's surface moves.
- rbs-inline is an accepted divergence. In an environment that has the
  `rbs-inline` gem, the reference folds annotated Ruby defs into the RBS
  environment. A member declared only inline would then make the reference
  silent where the port reports. The default install (and every oracle run
  here) has no `rbs-inline`. This is the same exposure the project-`sig/`
  witnessing of ADR-0033 already carries.
- The rows are positioned by the annotation's byte offset into the `.rbs`
  text, which rides along as the finding's source. A column after non-ASCII
  text on the same line would count bytes where RBS counts characters. The
  directive is written at a line start in practice.
- The path is absolute, as the reference's is: config-resolved
  `signature_paths:` are absolute there.

### Tier B: not ported

Tier B means checking that a PROVIDED member's signature is a behavioural
subtype of the required one: covariant return, contravariant params, arity,
and keyword-requiredness. Upstream argues it is FP-safe because both sides
are authored RBS, it uses single-method-type only, and it skips `Dynamic`.
Porting it faithfully needs the reference's `RbsTypeTranslator` and
`Type#accepts` over arbitrary RBS types. The port has neither; it does not
implement `def.override-return-widened` either. An approximation would be new
FP surface with nothing to measure it against. Tier B is recorded as the next
coverage gap.

## Consequences

- A project that configures `signature_paths:` and writes the directive now
  gets the reference's rows byte for byte (message, line:column, order,
  severity, `source_family: builtin`), except under the stand-downs above.
- `harness/lib.rb` learns PROJECT fixtures. A fixture shipping both a sidecar
  and a `.sig/` stages the sidecar as the cwd's `.rigor.yml`, because the
  reference resolves a `--config` file's relative `signature_paths:` against
  that file's directory. Rows positioned in the staged `sig/` are compared,
  keyed by `file`. Pre-existing snapshots are byte-identical.
- Fixture `112_conforms_to_directive` is the gate: 8 rows plus 9 silent
  controls, all MATCHED live. `fp_audit.py` cannot see this surface, because
  it runs configless.
- `UPSTREAM.md` step 3 now re-syncs `crates/rigor-index/vendor/capability_roles/`.
  A byte-identity test against the pinned submodule guards it.

## Measured outcome (2026-09-25, `e59b7b89`, Ruby 4.0.6)

The first measurement ran on a Ruby 3.3 sandbox with a shim. The audit re-ran
everything on Ruby 4.0.6 (rbs 4.2.0): 47 hand-built projects, reference vs
`target/release/rigor`, each in a fresh temp cwd with `--no-cache`, JSON
compared field by field (path, line, column, rule, severity, message,
`source_family`, order, exit code). Details:
[`docs/notes/20260925-conforms-to-audit.md`](../notes/20260925-conforms-to-audit.md).

- The audit found **four FP families**, all now fixed and gated by fixture
  112 and unit tests. First, generic reopens (`class Set`, `module
  Enumerable`) did not silence the class or its descendants. Second, a
  module's surface included `BasicObject`, so its list was too short.
  Third, an incomplete chain read as "present", which also shortened lists.
  Fourth, a file reached twice was reported twice.
- After the fixes, **no probe has a port-only row**. The remaining
  differences are all reference-only rows (gaps):
  - quarantine ordering: the port marks both files suspect, and an
    ambiguous interface silences its users;
  - a class also declared in a file that duplicates a bundled declaration;
  - diamond interface includes;
  - `Comparable`'s self-type interface;
  - a module aliasing a `BasicObject` method;
  - tier B (e.g. `initialize` arity);
  - the load-set guard (`libraries:`, bundler, unbundled plugins).
- Gates: `harness/run.rb` live and `run_snapshot.rb` report 0 unregistered.
  Every pre-existing snapshot is byte-identical under the project-fixture
  harness. `fp_audit.py --gaps --sweep` reports 0 FP / 9,337 files /
  3,829 gaps, identical to master.
