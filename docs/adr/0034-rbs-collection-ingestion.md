# Ingest gem RBS via `rbs_collection` discovery (Ruby-free)

Status: accepted

[ADR-0007](0007-rbs-stdlib-shipping.md) defines the analysis-time type
environment as embedded stdlib RBS ⊕ project `sig/` ⊕ **gem RBS** ⊕ inline RBS.
[ADR-0033](0033-project-sig-ingestion.md) landed the project-`sig/` leg. This ADR
lands the tractable, Ruby-free half of the **gem RBS** leg: `rbs collection`
awareness. A project set up with `rbs collection install` (the standard flow for
pulling community RBS from [`ruby/gem_rbs_collection`](https://github.com/ruby/gem_rbs_collection))
records resolved `(gem, version, source)` triples in `rbs_collection.lock.yaml`
and carries the actual `.rbs` under `.gem_rbs_collection/<name>/<version>/`.

## The decision

- Port the reference's `Environment::RbsCollectionDiscovery` as a **pure Rust
  filesystem + YAML walk** — the reference module is itself explicitly "no Bundler
  API call, no network access", so the port is faithful and adds **no Ruby
  runtime** (native `serde_yaml`, already a dependency). It: resolves the lockfile
  (explicit `rbs_collection.lockfile:` or, when `auto_detect` is on, auto-detects
  `<project_root>/rbs_collection.lock.yaml`), reads `path:` (default
  `.gem_rbs_collection`) and `gems:`, and returns each existing
  `<collection_root>/<name>/<version>/` directory.
- **Default `auto_detect` is `true`**, matching the reference
  (`configuration.rb` `rbs_collection.auto_detect => true`): with no config, a
  project that ran `rbs collection install` gets its gem types for free. Inert
  when no lockfile is present, so the default (no-collection) path is unchanged.
- The discovered directories are fed into the **SAME ingestion step** as project
  `sig/` (ADR-0033 `CoreData::load_for_project`), so collection gem classes gain
  **project-sig provenance** and are WITNESSED for `X.new` instance-method typos.
  This matches the reference exactly: empirically it attributes a collection gem
  to the `signature_paths:` / "project sig" tier and fires `call.undefined-method`
  on `Mygem.new.typo` (unlike a bundled stdlib class such as `Pathname`, which
  stays lenient).
- **`stdlib`-source-typed lockfile entries are skipped**, mirroring the
  reference's `SKIPPED_SOURCE_TYPES` — rigor-rs already loads that surface via its
  bundled stdlib closure, and a second copy at a different `rbs` version would be
  a divergence source (rigor-rs's union merge cannot raise the reference's
  `RBS::DuplicatedDeclarationError`, so the skip is for parity fidelity, not
  crash-safety).

## Scope

**`rbs_collection` only.** The other half of the gem-RBS leg — loading a gem's own
bundled `sig/` from the **bundler install root** (`bundler_bundle_path` /
Gemfile.lock → installed-gem paths) — requires gem-path discovery that IS
bundler/environment-dependent and cannot be done Ruby-free reproducibly. It is
deferred to its own slice/decision. Deferring it is **parity-safe**: a gem whose
types the reference loads via bundler but rigor-rs does not is a *coverage gap*
(a missed diagnostic), never a false positive — and coverage gaps are expected,
not gate failures ([ADR-0002](0002-diagnostic-set-parity.md)). Inline RBS
(ADR-0007's fourth leg) remains separate.

## Why Ruby-free holds

The discovery reads text files (`.yaml`, `.rbs`) and walks directories — no
`ruby`/`rbs`-gem/bundler process. The reference's own module documents the same
constraint. The `.rbs` are parsed by the native `ruby-rbs` parser already in use,
through the ADR-0033 `ingest_rbs_dir` path. This is the [ADR-0001](0001-rust-reimplementation-strategy.md)
single-binary contract, unbroken.

## Robustness

Every failure mode degrades to an empty directory list (missing/malformed
lockfile, absent collection root, a listed gem dir that isn't on disk) — the
run proceeds with no gem RBS, never crashes ([ADR-0016](0016-never-crash-isolation.md)).
As with ADR-0033, a malformed collection `.rbs` drops only its own declarations
(per-file isolation); there is no global resolve pass to collapse.

## Landing gate

The differential harness with a fixture that ships an `rbs_collection.lock.yaml`
+ `.gem_rbs_collection/` (staged into each tool's cwd, the ADR-0033 fixture-env
pattern): the collection gem's `.new` typo must be witnessed by both tools, and
a valid call must stay silent — measured coverage delta + 0 unregistered FP.
Discovery internals (lockfile parse, `path:` resolution, `stdlib` skip, missing-
dir filter) are unit-tested in `rigor-cli`.

## Considered options

- **Shell to `rbs`/bundler to resolve the collection** — rejected: breaks
  Ruby-free (ADR-0001) for no gain; the resolved lockfile + vendored `.rbs` are
  already on disk and the reference itself reads them without Bundler.
- **Load collection dirs as a distinct non-witnessing tier** — rejected: the
  reference witnesses them (empirically confirmed), so a separate lenient tier
  would *lose* parity coverage. Collection RBS is project-opted-in (the user ran
  `rbs collection install`), hence authoritative — same tier as project `sig/`.
- **Do the bundler-installed-gem `sig/` leg now too** — rejected for this slice:
  it needs bundler/environment-specific gem-path discovery (Ruby-free tension);
  deferring it only leaves coverage gaps, never FPs.
- **Skip only by `source.type`, not by gem name** — accepted for the first slice;
  the reference additionally skips stdlib-extracted default-gem *names* (`cgi`,
  `logger`, …) shipped under a `git` source. rigor-rs's union merge can't raise
  `DuplicatedDeclarationError`, so the residual is a narrow version-skew method-set
  divergence; the name-skip refinement is deferred until a corpus divergence
  demands it.

## Revisiting

Extend when the bundler-installed-gem `sig/` leg lands (needs a gem-path story),
if a corpus divergence forces the stdlib-extracted default-gem name skip, or when
inline RBS lands.
