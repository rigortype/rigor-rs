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
number, `115_` (siblings #148, #149 and #154 took 112–114 first). It gained `d_surface.rbs` (the module row, plus the
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
| `snapshot.rb --check` / `run_snapshot.rb` / `run.rb` live | up to date / 0 unregistered / 0 unregistered; fixture 115 8/8 MATCHED, byte-identical |
| `fp_audit.py --gaps --sweep` | 0 FP / 9,337 files / 3,829 gaps, identical to master |
| `conformance_load_set.rb --check` (+ `--plugin activesupport-core-ext`) | OK, 897 names (plugin adds none) |

## Third round: the environment, not the build

A second independent review (probe sets `rv3/b1`–`b15` plus `fuzz.rb`)
found 58 port-only projects in 14 families. None came from the build model:
440 random multi-file projects and 80 member shapes gave gaps only. Each
came from the reference running a different environment or configuration:

| # | family | reference |
|---|---|---|
| 1 | `use` directives | `use Foo::_Bar as _Baz` stubs `Foo::_Bar` (in every file); `use Nope::*` crashes |
| 2 | `# resolve-type-names: false` | read Ruby-side; the port's parser never sees it |
| 3 | non-ASCII identifiers | rbs 4.2 parses them; `ruby-rbs` 0.3.0 rejects the file, which the port dropped silently |
| 4 | a NUL byte in a project `.rbs` | the run crashes |
| 5 | rbs collections | gems named in `DEFAULT_LIBRARIES` or vendored are skipped |
| 6 | deferred plugin arity | compared against the class's first bundled/project declaration |
| 7 | `plugins: [activesupport-core-ext]` | a load error, no plugin |
| 8 | activesupport locked from `GIT` | the overlay loads (Bundler's parser) |
| 9 | no Ruby file analysed | no environment, no rows |
| 10 | glob metacharacters in the signature path (root included) | `Dir.glob` interprets them |
| 11 | `~` in `signature_paths:` | expanded to `$HOME` |
| 12 | `lnk/../sig` | folded lexically |
| 13 | 22 configs | rejected (exit 64 / 1) or read otherwise (YAML 1.1 `off` / `yes` / `1_0`, dates, aliases, symbols, bad profile / override / workers / isolation / cache / dependencies, `3.3e0`) |
| 14 | a column after multibyte text | counted in characters |

**The fix** (ADR-0044 § "Environment-parity gate") adds a gate. The scan
stands down unless the environment is provably the reference's:
- the reference has ≥1 Ruby file;
- the config fits a strict YAML subset whose every key's coercion was read
  in `configuration.rb`;
- signature paths have no `~`, no glob metacharacter, and a `..` only where
  the lexical fold matches the physical path;
- no rbs collection;
- a `Gemfile.lock` with `GEM`-only gem sources;
- every project `.rbs` is readable and parseable, with no NUL, directive or
  `resolve-type-names`.

Two families are fixed exactly: the plugin arity comparison (6) and the
character column (14; oracle: 2-, 3- and 4-byte characters and a combining
mark all count as one scalar each).

### Tallies (release build, one fresh cwd per project, `--no-cache`)

| probe set | projects | identical | same rows, other diffs | gap-only | exit code only | port-only rows |
|---|---|---|---|---|---|---|
| rv3 `b1`–`b15` + `fuzz` (seed 1) | 320 | 180 | 63 | 77 | — | 0 |
| rv3 `fuzz` seeds 2–11 | 400 | 173 | 159 | 68 | — | 0 |
| first round `probes1`–`8`, reviewer `/tmp/rv150`, `al/q1`–`q7` | 239 | 129 | — | 104 | 6 | 0 |

- All 58 rv3 port-only projects are fixed: 57 now emit no conformance row,
  and the multibyte-column one is identical.
- The rv3 runner counts "same rows, other diffs" separately: rows identical,
  exit code or other rules different. The first-round runner folds that
  into "gap-only" and reports exit codes apart. Its six exit-code
  differences have no port row: the reference's `prepend _Z` crash, three
  `target_ruby` rejections, a bare-`off` override (exit 64), and the
  unbundled `rigor-rails` stand-down.
- No common row changed order.
- `probes7.rb`'s `w_realistic` (3 rows) and fixture 115 (8 rows) are
  identical.
- Measured cost: 23 rv3 projects the two tools used to agree on became gaps
  (listed in the ADR).

| gate | result |
|---|---|
| `cargo test --workspace` | 1,344 passed, 0 failed, 1 ignored |
| clippy 1.88, fresh target dir, lib and `--all-targets` | clean |
| `snapshot.rb --check` / `run_snapshot.rb` / `run.rb` live | up to date / 0 unregistered / 0 unregistered, coverage 565/613; fixture 115 8/8 MATCHED |
| `fp_audit.py --gaps --sweep` | 0 FP / 9,337 files / 3,829 gaps |
| `conformance_load_set.rb --check` | OK, 897 names |

**Divergences for other rules (not fixed here, follow-up candidates):**
every family above except 9 and 14 also changes what the port's other
rules see from project RBS or config. That covers `use` /
`resolve-type-names` unmodelled, non-ASCII files dropped, NUL, the
collection skip list, deferred plugin arity, bare plugin ids, `GIT` / `PATH`
lockfile sources, glob / `~` / `..` path resolution, and the 22 rejected
or differently-read configs. It adds the round-2 items (`paths:`,
`pre_eval`, `plugins_io.allowed_paths` and `includes` resolved against
cwd; a scalar `signature_paths: sig`; an unsupported `target_ruby` emitting
only its error), `.rigor.dist.yml` discovery, and the Gemfile.lock overlay
loaded as a project signature path upstream but as a plugin here. `exclude:`
matching also differs: the reference uses `File.fnmatch?` with no flags and
does not appear to apply `exclude:` to explicit file arguments; the port
uses `glob::Pattern` on every file. That last point was read, not probed.

## Fourth round: the CLI, the baseline and the host

A third independent review (`rv4/c0`–`c16`, 217 projects) found no
false positive from the build model. It found 10 families that reached the
scan through the environment, the config text or the CLI:

| family | what the reference does | port before |
|---|---|---|
| A. NEL / LS / PS in `.rigor.yml` | libyaml breaks lines on them, even inside a comment: `# note<LS>cache: 5` is a `cache: 5` it rejects | the `\n`-only subset reader saw one comment |
| B. `$HOME/.bundle/config` | `BundleSigDiscovery.auto_detect` step 3 loads the global `BUNDLE_PATH`'s gem `sig/` | not read |
| C. `--config <D>/lnk/../conf.yml`, `--config '~/x.yml'` | `File.expand_path`: `..` folded lexically, `~` expanded | the OS path |
| D. a baseline | regroups output by (file, rule) bin; message-mode baselines use folded scalars; `../` paths for a sig dir outside the cwd | order, parse and path differences |
| E. unparsed flags | unknown flags and `-file.rb` exit 64; abbreviations (`--basel=`); `--config=PATH`, `--baseline=PATH`, `--`, `--verify-incremental`, a trailing `--workers` | taken as paths |
| G. case-insensitive volume | `Dir.glob` reports the on-disk case of literal segments | the spelled case |
| (concern) non-UTF-8 entry names | seen by Ruby's glob | skipped |
| (concern) row offsets | against the parsed text | a re-read of the file |
| I. rbs-inline in the reference's Ruby | folds inline `#:` signatures into the class | not modelled (accepted) |
| J. newer default-library gems on `GEM_PATH` | their `sig/` replaces rbs's stdlib copy | not modelled (accepted) |

**Fix.** Each family A–G became a gate clause (ADR-0044 § "Environment-parity
gate", round-4 rows):
- A: the config must contain only characters both YAML readers treat as
  text;
- B: no bundle source, the global config included;
- C: the config path passes the `~` / lexical-`..` / case checks;
- D: no baseline in effect;
- E: every dash argument is on an allow-list of spellings verified against
  `check_command.rb`'s `OptionParser`;
- G: every existing path component (signature paths, config path, absolute
  prefix) is spelled as on disk.

In the index, a non-UTF-8 name under a sig dir stands the scan down, and
rows are positioned against the text the index parsed. I and J are
recorded as accepted divergences in the ADR, with their reproducers.

### Tallies (release build, one fresh cwd per project, `--no-cache`)

| probe set | projects | identical | same rows, other diffs | gap-only | exit code only | port-only |
|---|---|---|---|---|---|---|
| rv4 `c0`–`c16` | 217 | 120 | 35 | 59 | — | 3 (the accepted I/J reproducers only) |
| rv3 `b1`–`b15` + `fuzz` seed 1 | 320 | 179 | 63 | 78 | — | 0 |
| rv3 `fuzz` seeds 2–11 | 400 | 173 | 159 | 68 | — | 0 |
| first-round `probes1`–`8`, `/tmp/rv150`, `al/q1`–`q7` | 239 | 128 | — | 105 | 6 | 0 |

- Every rv4 port-only or order-differing project that is not an I/J
  reproducer is now silent or identical: 45 of 48, the other 3 being those
  reproducers. That is 26 same-rows, 5 identical and 14 gaps. No common row
  changed order anywhere.
- `probes7.rb`'s `w_realistic` (3 rows) and fixture 115 (8 rows) are
  identical.
- The six exit-code differences carry no port row and are the ones round 3
  listed.
- Cost this round: 32 former agreements became gaps.
  - rv4 (30):
    - the three `effects:` keys behind an LS / NEL / PS comment;
    - ten baseline probes (reference-generated rule baseline, Ruby-regex
      message rows);
    - 15 CLI-flag probes (reference-only flags, `=` forms);
    - `--no-basel`;
    - `g_nfc_quoted` (an NFC spelling of an NFD directory).
  - Elsewhere (2): `b3_bad_baseline` and `f_baseline` (baselines).

| gate | result |
|---|---|
| `cargo test --workspace` | 1,349 passed, 0 failed, 1 ignored |
| clippy 1.88, fresh target dir, lib and `--all-targets` | clean |
| `snapshot.rb --check` / `run_snapshot.rb` / `run.rb` live | up to date / 0 unregistered / 0 unregistered, coverage 565/613; fixture 115 8/8 MATCHED |
| `fp_audit.py --gaps --sweep` | 0 FP / 9,337 files / 3,829 gaps |
| `conformance_load_set.rb --check` | OK, 897 names |

### Consolidated follow-ups: divergences that also affect OTHER rules (rounds 2–4)

None is fixed here. The `conforms-to` scan stands down on each; every other
rule still runs through them.

1. **CLI parsing.** The port takes every unrecognised argument as a path:
   unknown flags and `-file.rb` (exit 64 there), `OptionParser`
   abbreviations (`--basel=` = `--baseline=`, `--no-basel`), the `=` forms
   `--config=PATH` / `--baseline=PATH` / `--format=json`, `--`, a trailing
   `--workers`, the reference-only flags (`--workers`, `--incremental`,
   `--verify-incremental`, `--explain`, `--no-cache`, `--fail-on`,
   `--treat-all-as-inline-rbs`, `--tmp-file` / `--instead-of`, …) and the
   port-only `--ruby` / `--no-ruby` (unknown there).
2. **Config YAML dialect.** YAML 1.1 (Psych) against 1.2 (`serde_yaml`):
   bare `on` / `off` / `yes` / `no`, `1_0`, `3.3e0`, dates (Psych
   `DisallowedClass`), symbols, aliases, NEL / LS / PS line breaks,
   duplicate keys.
3. **Config validation.** The reference rejects values the port accepts
   (exit 64 / 1, no rows): an unknown `severity_profile` or override value,
   `parallel.workers < 0`, bogus `plugins_isolation`, a non-mapping `cache`
   or `dependencies`, an unsupported or malformed `target_ruby` (a lone
   `configuration-error` there). The port reads some values it does not
   reject differently: a scalar `signature_paths: sig`, a bare plugin id.
4. **Config paths.** `paths:`, `pre_eval`, `plugins_io.allowed_paths` and
   `includes` resolve against the config file's directory upstream and the
   cwd here. `~` is expanded upstream. `..` folds lexically upstream, in
   signature paths AND in the `--config` path. `.rigor.dist.yml` is not
   discovered.
5. **Glob and filesystem semantics.** Glob metacharacters in a signature
   path (project root included) are interpreted upstream. On a
   case-insensitive volume the reference renders on-disk case, and NFC/NFD
   spellings of a name differ (`g_nfc_quoted`). The port skipped non-UTF-8
   names. `exclude:` is `File.fnmatch?` with no flags upstream, and was not
   read as applying to explicit file arguments there (read, not probed);
   the port uses `glob::Pattern` on every file.
6. **Project RBS the port does not read as the reference does.** `use`
   directives (stubs included), `resolve-type-names`, non-ASCII identifiers
   (`ruby-rbs` 0.3.0 rejects them and the file drops for every rule), NUL
   bytes (a crash there).
7. **Load set and bundler.** The rbs collection skip list
   (`DEFAULT_LIBRARIES` + vendored gem names) is not applied. GIT / PATH
   lockfile sources don't select overlays. The Gemfile.lock overlay is a
   signature path upstream and a plugin here. The deferred plugin arity
   stand-down is missing in the main index. `$HOME/.bundle/config`
   `BUNDLE_PATH` is not read.
8. **Baseline.** Output order under a non-empty baseline (the reference
   regroups by (file, rule) bin); reference-generated message-mode baselines
   (folded double-quoted scalars) don't parse; `../` relative paths for a
   file outside the cwd; Ruby-only regex syntax in message rows.
9. **Host dependence** (accepted in the ADR): rbs-inline in the reference's
   Ruby, and default-library gem versions on `GEM_PATH`.

## Fifth round: subcommands and the process environment

A fourth review of the round-4 delta (`rv5/d1`–`d6`, `repro1`–`2`,
`tri*`) found that the round-4 clauses held, and found four small families:

| family | reference | port before |
|---|---|---|
| `POSIXLY_CORRECT` present (even empty) | `OptionParser` stops at the first non-option: `app.rb --config x.yml` makes `--config` a path | read `--config` |
| `diff`, `triage`, `baseline *` | their own parsers upstream | carried conformance rows past the CLI half of the gate (only `cmd_check` applied it) |
| `RIGOR_RACTOR_WORKERS=abc` / `2x` / `1.5` | the run crashes (`Integer()`, exit 1) | rows |
| a non-UTF-8 component above a sig dir (Linux) | reads the file | a lossy key dropped it silently |

**Fix.**
- The scan runs for `check` only. There is one gate function with no bypass:
  `conformance_scan_active` is reached only through `analyze_files` with
  verb `check`.
- `POSIXLY_CORRECT` present, or a `RIGOR_RACTOR_WORKERS` that is not plain
  decimal digits, stands the scan down.
- A project signature path that is not valid UTF-8 stands the scan down.
- The config reader now trims ASCII spaces only. `parse_subset` had
  trimmed Unicode whitespace, which libyaml keeps inside a plain scalar.
- The path-case check treats only "not found" as missing; any other error
  stands the scan down.
- The environments where the reference cannot start are recorded in the
  ADR as accepted divergences:
  - `RUBYOPT=--disable-gems`;
  - an empty `GEM_PATH`;
  - `-rbundler/setup`;
  - `-Eascii-8bit:ascii-8bit`;
  - `RUBY_BOX=1`.

### Tallies (release build, one fresh cwd per project, `--no-cache`)

| probe set | projects | identical | same rows, other diffs | gap-only | exit code only | port-only |
|---|---|---|---|---|---|---|
| rv5 `d1`–`d6` + `repro1`–`2` | 141 | 93 | 13 | 30 | — | 5 (the accepted can't-start environments only) |
| rv5 `tri` / `tri2` / `tri3` (`diff` / `triage`) | 18 | — | — | — | — | 0 (no port conformance row in any) |
| rv4 `c0`–`c16` | 217 | 120 | 35 | 59 | — | 3 (the accepted I/J reproducers only) |
| first-round `probes1`–`8`, `/tmp/rv150`, `al/q1`–`q7` | 239 | 128 | — | 105 | 6 | 0 |

- Every rv5 port-only project that the ADR does not list as accepted is
  now silent: the three `POSIXLY_CORRECT` ones (`a1`, `a3`, `a5`) and the
  three `RIGOR_RACTOR_WORKERS` ones.
- No common row changed order.
- `probes7.rb`'s `w_realistic` (3 rows) and fixture 115 (8 rows) are
  identical.

| gate | result |
|---|---|
| `cargo test --workspace` | 1,351 passed, 0 failed, 1 ignored |
| clippy 1.88, fresh target dir, lib and `--all-targets` | clean |
| `snapshot.rb --check` / `run_snapshot.rb` / `run.rb` live | up to date / 0 unregistered / 0 unregistered, coverage 565/613; fixture 115 8/8 MATCHED |
| `fp_audit.py --gaps --sweep` | 0 FP / 9,337 files / 3,829 gaps |
| `conformance_load_set.rb --check` | OK, 897 names |

**Follow-ups added to the consolidated list:**

10. **Non-JSON output formats.** `text`, `github`, `sarif` and the other
    non-JSON formats render every rule's rows differently from the
    reference (`rv5/fmt.rb`). This is not conformance-specific.
11. **Process environment for other rules.** `POSIXLY_CORRECT` argument
    parsing, `RIGOR_RACTOR_WORKERS` validation, and the non-`check`
    subcommands' parsers (`diff`, `triage`, `baseline`) share the round-4
    CLI divergences.
