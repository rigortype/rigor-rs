# Read `%a{rigor:v1:conforms-to _Interface}`: presence tier only, silent wherever the reference may not load, build or resolve

Status: accepted 2026-09-25 (issue [#129](https://github.com/rigortype/rigor-rs/issues/129); pin `e59b7b89`); amended the same day by the second review round (the allow-list)

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
3. **Required members** are `RBS::DefinitionBuilder#build_interface`'s
   `methods` keys (pinned with rbs 4.2.0): the interface ANCESTORS in
   `interface_ancestors` order — the LAST include first, each followed by
   its own ancestors (pre-order: `_I` including `_B` including `_C` lists
   `b, c, i`) — each contributing its own members in `MethodBuilder` tsort
   order (an alias after its target), then the interface's own. An ancestor
   met twice is imported once: `interface_methods` keys its hash by
   `Ancestor::Instance`, whose equality ignores the include that reached it.
4. **Provided members** are what `RBS::DefinitionBuilder#build_instance`
   returns: own defs (private ones included), `self?` defs, attributes and
   aliases, then included and prepended modules and interfaces recursively
   (`define_instance`). A class adds its superclass's whole definition. A
   module adds only its self types' OWN definitions, default `Object`, which
   brings `Kernel` but not `BasicObject`: `#==` and `#!` are missing on a
   module.
5. **Resolution** follows the directive's class namespace prefixes, longest
   first, then the bare name, with a leading `::` stripped. The reference
   takes the first candidate that BUILDS: a candidate that exists but fails
   to build falls through to the next.

### The allow-list (PR #150, second review round)

The first implementation enumerated the reference's build failures and
silenced each. The review found seven families it had missed (unmodelled
RBS errors, unbuildable interfaces, the load set, declaration order, the
file walk, `--config`, `target_ruby`), and the audit below found four more
(nested include order, stub interfaces, an interface under an undeclared
namespace, a project file redeclaring a constant only the reference has). The failure set is open-ended, so the decision flips: **a row
fires only when the reference's build is PROVABLY successful**.

`conformance.rs` records its own model of every declaration, bundled and
project alike: headers (superclass, self types, type parameters with
variance / `unchecked` / bound / default), every instance member, the named
types in every method and attribute signature, and an `unsupported` bit for
any member kind or type form it does not model. `conformance/closure.rs`
then walks the BUILD CLOSURE: `build_instance(C)` is `C`, its superclass's
whole build, a module's self types, and `define_instance(C)`, which reaches
the included and prepended modules (recursively), their self types' builds,
and the included interfaces with their ancestors. Every entity on it must
pass:

| check | reference error it rules out |
|---|---|
| every written name resolves by RBS's own rule (head segment innermost scope outward, then the root; no fallback on a later segment), to something that provably exists THERE: not a load-set divergence, a class alias, a possible stub or a synthesized namespace | `NoTypeFoundError`, `NoMixinFoundError`, `NoSuperclassFoundError`, a name bound elsewhere upstream |
| a superclass is a class, a mixin a module, a self type a class, module or interface; `prepend` of an interface stands the whole scan down (upstream the build raises a non-RBS error and the run dies) | `InheritModuleError`, `MixinClassError`, `NoSelfTypeFoundError` |
| type-argument count within `[params without default, params]`, every name in the arguments present | `InvalidTypeApplicationError`, `validate_type_presence` |
| every declaration of a class repeats the first one's parameters (a bound or default counts as a mismatch); a mismatched class also fails every build whose `validate_type_params` names it | `GenericParameterMismatchError` |
| a project class's parameters are invariant or `unchecked`, without bound or default; a built class's method and attribute types (not `initialize`) name only present, consistent classes | `InvalidVarianceAnnotationError`, `NoTypeFoundError` in `validate_type_params` |
| one original per member name across every declaration, no alias cycle, no own def over an included interface's member | `DuplicatedMethodDefinitionError`, `RecursiveAliasDefinitionError` |
| an alias's target and an overloading def's base are present when the member is defined (own members, included interfaces and modules, and on the class's OWN build its superclass / a module's self types) | `UnknownMethodAliasError`, `InvalidOverloadMethodError` |
| included interfaces' members pairwise distinct (an interface met again with the same arguments is imported once) | `DuplicatedInterfaceMethodDefinitionError` |
| no instance variable inserted twice by one declarer into one definition (a diamond include of a module declaring `@x`) | `InstanceVariableDuplicationError` |
| no ancestor cycle, every enclosing namespace declared | `RecursiveAncestorError`, `ensure_namespace!` |

An entity declared ONLY by bundled RBS (core / stdlib / overlay / catalogue /
a bundled plugin) is trusted: the pinned oracle builds every one of them,
and the load-set list below names each whose surface differs. A project
file carrying a directive (`use`, `resolve-type-names`) is not modelled. The
member sets come from the same walk, so presence is exact wherever the build
is provable. Earlier silences (duplicate members, file quarantine, generic
reopens) are now rows of that table.

### Load set: a generated list, plus the config stand-downs

The reference's default libraries load `prism`, `rbs` (and `rdoc` through
it), and on the gate host read the installed `bigdecimal` / `base64` /
`mutex_m` gems' `sig/` where the port vendors rbs's stdlib copies.
`harness/conformance_load_set.rb` builds the reference's configless
environment (`RbsLoader.build_env_for(DEFAULT_LIBRARIES, [])`), dumps the
port's model through the scan's own walk (an ignored test), and writes
`conformance/load_set.rs`: every class, interface, type alias, class alias,
constant or global only one side has, every class whose kind, arity,
buildability or instance-method NAME SET differs, and every interface whose
member LIST differs. At `e59b7b89` on the gate host: **897 names** (639
reference-only declarations — 534 classes, 80 modules, 25 interfaces — 151
type aliases, 103 constants, 2 class aliases, and the `BigDecimal` /
`BigMath` surfaces). No core class (`Object`, `Kernel`, `BasicObject`) and
no bundled interface differs. The scan never resolves through, trusts, or
orders by a listed name, and a project file DECLARING one is treated as
possibly quarantined (upstream it may collide with a declaration the port
cannot see: `Prism::VERSION: String` drops the whole file there). `--check` verifies it; `--check --plugin activesupport-core-ext`
verifies the bundled plugin adds nothing (it adds nothing). Like
`UNBUILDABLE_DEFINITIONS`, the list depends on the host's installed gems.

Config inputs the port does not mirror still stand the whole scan down:
`libraries:`, `bundler:`, `includes:`, a `.bundle/config` or
`vendor/bundle/`, a plugin the port does not bundle, and a `target_ruby:`
outside `3.3` / `3.4` / `4.0` (with or without a patch level) and `latest`.
The reference rejects a malformed `target_ruby` before the run (exit 64) and
one its Prism cannot parse with a lone `configuration-error` row (exit 1;
the message embeds Prism's own error text, so the port does not reproduce
it).

### Order: project-first classes only

`env.class_decls` puts bundled classes first in the loader's order, which the
port's embed order does not reproduce (292 of 1,361 bundled-class reopens
were displaced). A directive on a class first declared by bundled RBS, a
plugin (the reference defers plugin `sig/` after the project), an rbs
collection, a possibly-quarantined file, or a load-set-listed name is
silent. Project-first classes keep `(sorted file, offset)` order: they
follow every bundled class upstream, and synthesized namespaces and stubs
are appended after them.

### Project files: `Dir.glob` and the config's directory

The project walk is `Dir.glob("<dir>/**/*.rbs")` exactly: no dot-files or
dot-directories, no descent into a symlinked directory, a symlinked or
dangling `*.rbs` entry matched by name (a read failure skips it, as
upstream), `*.RBS` only on a case-insensitive volume (Ruby folds case there).
Files are expanded, deduped and ingested sorted, rbs-collection dirs
included. A relative `signature_paths:` entry resolves against the directory
of the config file actually read (`Configuration.resolve_paths_in`), so
`--config conf/custom.yml` reads `conf/sig`. This moves every rule's
project-sig loading, as intended.

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

- A project that configures `signature_paths:` and writes the directive gets
  the reference's rows byte for byte (message, line:column, order, severity,
  `source_family: builtin`) wherever the allow-list proves the build, and
  nothing elsewhere.
- `UPSTREAM.md` step 3 gains `ruby harness/conformance_load_set.rb --check`
  (and `--check --plugin activesupport-core-ext`) beside
  `unbuildable_classes.rb`.
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
- After the fixes, **no probe had a port-only row** (the second review round
  below found seven families these 47 projects missed). The remaining
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

### Second round: the allow-list (2026-09-25, same pin and host)

The first round's probes (54 projects), the reviewer's (`pa`–`pj` plus the
all-classes and all-interfaces generators, 118) and this round's own (50:
the families below, `target_ruby`, the glob and `--config` edges, interface
diamonds, real-world shapes) were re-run against the release build, each in
a fresh cwd with `--no-cache`: **222 projects, 124 identical, 93 gap-only,
0 port-only rows, 0 order differences on common rows.** The other 5 differ
in exit code with no port row: the reference crashes (`prepend _Z`, 1) or
rejects `target_ruby` (`"3.2"` twice, `"x"`), and in the unbundled
`rigor-rails` stand-down it reports 2 rows and exits 1. Every reproducer of
families 1–7 is now silent or identical, and so are four new families:

| new family (found by this audit) | reference | port before |
|---|---|---|
| nested include order | `b, c, i` | `c, b, i` (post-order) |
| a written but undeclared `_Missing` in a method type | stubbed empty interface: no row | "not loaded" |
| `interface Nope::_I` under an undeclared namespace | "not loaded" | a missing-member row |
| a project file redeclaring a reference-only constant (`Prism::VERSION: String`) or kind-clashing with a reference-only module (`class RDoc`) | the file is quarantined: no row | its rows |

Accepted gaps (reference-only rows), each a deliberate refusal: a class
first declared by bundled RBS (all 1,361 reopened: 15,134 rows); a method
type naming an undeclared class (the reference stubs it); an alias in an
included module targeting the includer's superclass; an instance variable
declared twice where a third declaration defuses the check; bounds, defaults
and variance on project parameters; an interface redeclared across files;
`use` directives; `target_ruby` 3.5 / 4.1; tier B. `fp_audit.py --gaps
--sweep`: 0 FP / 9,337 files / 3,829 gaps, identical to master. Index load
grows about 2.5 ms (28.5 → 31 ms) for the model. `run.rb` live and
`run_snapshot.rb`: 0 unregistered; fixture 112 keeps its 8 rows MATCHED and
byte-identical (`snapshot.rb --check`: up to date). Details:
[`docs/notes/20260925-conforms-to-audit.md`](../notes/20260925-conforms-to-audit.md).
