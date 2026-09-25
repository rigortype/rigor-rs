# Read `%a{rigor:v1:conforms-to _Interface}`: presence tier only, silent wherever the reference may not load, build or resolve

Status: accepted 2026-09-25 (issue [#129](https://github.com/rigortype/rigor-rs/issues/129); pin `e59b7b89`); amended the same day by the second review round (the allow-list) and the third (the environment-parity gate)

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
file carrying a directive (`use`, `resolve-type-names`) stands the whole
scan down (§ "Environment-parity gate"). The
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
Round 3 generalises these into the environment-parity gate below.
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

### Environment-parity gate (PR #150, third review round)

The third review found 58 port-only projects in 14 families, and none came
from the build model: 440 random multi-file projects and 80 member shapes
gave gaps only. Every one came from the reference running a different
ENVIRONMENT or CONFIG than the port assumed. That set is open-ended too, so
the scan now also requires that the environment be provably the one the
reference builds. Each clause below is oracle-justified. "Stands down"
means no conformance row at all for the run.

| clause | why (oracle, `e59b7b89`) |
|---|---|
| the reference's run has ≥1 Ruby file: an explicit `*.rb` file argument, or a directory root (no glob metacharacter) with a `**/*.rb` file that `File.fnmatch?` could not exclude (over-approximated; built-in `vendor/bundle`, `.bundle`, `node_modules` excludes included) | with no file the reference builds no environment and emits no row (an empty `lib/`, a missing or `.rbs` argument, an empty `paths:`) |
| the config text fits a strict YAML subset and every key passes a coercion read in `configuration.rb` (below) | the reference rejects many values the port accepts, exits 64 or 1, and emits no row |
| every `signature_paths:` entry: no leading `~`, no glob metacharacter (`*?[]{}\`) in the absolute path `Dir.glob` receives, and a `..` only where the lexical fold names the directory the OS reaches | `File.expand_path` expands `~`; `Dir.glob` interprets metacharacters, including in the project root (`proj[x]`); `lnk/../sig` folds lexically upstream |
| no `rbs_collection.lock.yaml` and no discovered collection dir | the reference skips collection gems named in `DEFAULT_LIBRARIES` or vendored (`json`, `redis`, `prism`, `rbs`, …); the skip list is not modelled |
| a `Gemfile.lock` has only `GEM` / `PLATFORMS` / `DEPENDENCIES` / `RUBY VERSION` / `BUNDLED WITH` / `CHECKSUMS` sections | Bundler reads a `GIT` / `PATH` gem as locked and loads its overlay; the port reads `GEM` only |
| every project `.rbs` is readable, parses with the port's parser, and holds no NUL byte, no directive and no `resolve-type-names` comment (index side) | `ruby-rbs` 0.3.0 rejects non-ASCII identifiers rbs 4.2 accepts, and the file used to be dropped silently; a NUL crashes the reference; `use Foo::_Bar as _Baz` stubs `Foo::_Bar` in EVERY file; the magic comment is read Ruby-side only |
| (kept) `signature_paths:` configured and non-empty, `target_ruby` accepted, no `.bundle/config` or `vendor/bundle/`, only bundled plugins, no `prepend` of an interface | rounds 1 and 2 |

**The config subset.** `conformance_gate.rs` parses the file itself: top-level
`key: scalar`, `key: []` / `{}`, or ONE level of block sequence or mapping,
with comments, CRLF and a leading `---`. Anchors, aliases, tags, flow content,
block scalars, escapes, tabs, nesting and duplicate keys all stand the scan
down. A plain scalar counts as a string only when Psych (YAML 1.1) and
`serde_yaml` both read it as its text: it has name and path characters only,
no leading digit or sign, and is none of `yes/no/true/false/on/off/null`
(any case), `.inf` or `.nan`. So a bare `off`, `yes` or `1_0` directory is
refused, and so are a date (`DisallowedClass`) and a `:symbol`. Accepted keys:
- `signature_paths`, `paths`, `exclude`, `disable`: lists of strings (`exclude`
  and `disable` may be null);
- `plugins`: `rigor-<id>` gem names of bundled plugins only. A bare id loads
  nothing upstream; the port normalises it;
- `target_ruby`: quoted, or plain `3.3` / `3.4` / `4.0` / `latest` /
  `x.y.z`. `3.3e0` is a String to Psych and a float to `serde_yaml`;
- `severity_profile`: `lenient` / `balanced` / `strict`;
- `severity_overrides`: a mapping whose values are the strings `error`,
  `warning`, `info` or `off` (so `off` must be quoted);
- `baseline`: a string or `false`;
- `bleeding_edge`: `true`, `false` or a list;
- `rigor_rs`: a mapping of strings;
- keys the reference does not own: inert there (only warned about), so they
  are accepted when their values are strings, booleans or plain integers.

Every other key the reference owns stands the scan down: `libraries`,
`includes`, `bundler`, `rbs_collection`, `cache`, `parallel`,
`dependencies`, `effects`, `plugins_isolation`, `plugins_io`, `pre_eval`,
`fold_platform_specific_paths`, `parameter_inference`. Their validation or
their effect on the environment is not modelled.

**Fixed exactly rather than gated:** (6) a bundled plugin's `sig/` is deferred
upstream and dropped when a class's arity differs from that class's FIRST
declaration (bundled, else the first project one); the port compared against
its own first declaration, the plugin's. (14) the annotation's column is
counted in characters (Unicode scalar values), as RBS does. The oracle agrees
with 2-, 3- and 4-byte characters and with a combining mark before the
annotation.

**Measured cost.** Of rv3's 320 projects, 23 that the reference and the
port used to agree on became gaps:
- `resolve-type-names`, `# resolve-type-names: true` included (6);
- rbs collections, `rbs_collection:` included (3);
- an unreadable or unparseable project file whose rows came from another
  file: invalid UTF-8, a BOM, `-> instance` in an interface (4);
- config values: a null or numeric `signature_paths:` element,
  `effects:`, `target_ruby: 3.40` or `!!str 3.4`, a plugin listed twice (6);
- a glob metacharacter in a directory name (2);
- a `PATH` lockfile (1);
- the `Gemfile.lock` overlay treated as a deferred plugin (1).

Ordinary projects keep their rows: `probes7.rb`'s `w_realistic` (3 rows) and
fixture 112 (8 rows) are identical. The sweep is configless and unchanged.

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

### Third round: the environment-parity gate (2026-09-25, same pin and host)

The third review's 58 port-only projects (14 families, all environment or
config) are fixed: 57 now emit no conformance row, and the multibyte-column
one is identical. Re-run on the release build, in fresh cwds with
`--no-cache`:
- rv3 `b1`–`b15` + `fuzz` seed 1: 320 projects;
- `fuzz` seeds 2–11: 400 more;
- the earlier sets (first-round `probes1`–`8`, `/tmp/rv150`, `al/q1`–`q7`):
  239.

Across the 959 there are **0 port-only rows and 0 order differences**. The
cost is the 23 agreements listed under "Measured cost". Gates:
`cargo test --workspace` 1,344 passed; clippy 1.88 clean in both modes;
`snapshot.rb --check` up to date; `run_snapshot.rb` and `run.rb` 0
unregistered (fixture 112 8/8); `fp_audit.py --gaps --sweep` 0 FP / 9,337
files / 3,829 gaps; `conformance_load_set.rb --check` OK. The divergences
these families expose for OTHER rules are listed in the audit note as
follow-ups.
