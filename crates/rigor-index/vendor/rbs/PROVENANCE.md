# Vendored RBS signatures — provenance

This tree is the **exact** RBS signature set that `rigor-index` loads, vendored
into the repo so the analyzer is standalone (no runtime dependency on a local
`rbs` gem). It is embedded at build time by `crates/rigor-index/build.rs` and
ingested by `CoreData::load()` (`src/rbs.rs`) when `RIGOR_RBS_CORE_DIR` is unset.

- **Source gem:** `rbs-4.1.1`
- **Source path:** `/Users/megurine/.local/share/mise/installs/ruby/4.0.5/lib/ruby/gems/4.0.0/gems/rbs-4.1.1`
- **Vendored:** 2026-08-09 (was `rbs-4.1.0`, 2026-07-31; before that `rbs-4.0.3`,
  2026-06-26 — bumped with the reference pin to `v0.3.2`, which follows rbs
  4.1.1; the two pins must match). **The 4.1.0 → 4.1.1 bump is a no-op for this
  tree**: `vendor_rbs.py --check` against the 4.1.1 gem reports an exact match on
  all 174 `.rbs`, so `core/` and `stdlib/` are byte-identical across the two
  releases and no signature-resolution behaviour moves with the version string.
- **What the set is:** the WHOLE `core/` directory ⊕ the `DEFAULT_LIBRARIES`
  stdlib set (`src/rbs.rs`) transitively closed over each lib's
  `manifest.yaml` `dependencies:` — i.e. byte-for-byte the set the old runtime
  path ingested. **Not** the entire `stdlib/` tree; only the loaded closure.
- **This is NOT byte-for-byte what the REFERENCE loads**, and the difference is
  load-bearing. `RBS::EnvironmentLoader#add(library:)` prefers an installed
  GEM's own `sig/` over `rbs`'s `stdlib/<lib>/` copy, so on a current Ruby the
  reference reads `bigdecimal-*/sig`, `base64-*/sig`, `mutex_m-*/sig`,
  `prism-*/sig` and `rbs-*/sig` where this tree carries the `rbs` stdlib copy
  (or nothing). Through the `v0.3.1` pin that asymmetry cost real signatures:
  `bigdecimal` made the reference load `BigMath` TWICE (gem + `stdlib/
  bigdecimal-math`), which collided and left the reference unable to build the
  definition at all, and the `rbs` gem's `sig/shims/{bundler,rubygems}.rbs`
  collided the same way with `data/vendored_gem_sigs/`.
  `UNBUILDABLE_DEFINITIONS` in `src/rbs.rs` mirrored all twelve casualties.
  **Upstream `v0.3.2` fixed every one** (#299/#300/#301), so that table is now
  EMPTY — but regenerate it with `harness/unbuildable_classes.rb --check`
  whenever this tree, the reference pin, or the host Ruby moves, **in the
  environment the gates run in**: the `BigMath` half depended on the host having
  the `bigdecimal` gem installed at all (measured on `v0.3.1`: without it the
  reference built `BigMath` and the set dropped from 12 entries to 11).
  [note](../../../../docs/notes/20260731-bigmath-ingestion-asymmetry.md),
  [note](../../../../docs/notes/20260809-repin-v032.md)

## Contents

- `core/` — all 89 `.rbs` from `…/rbs-4.1.1/core` (nested under `io/`,
  `enumerator/`, `object_space/`, `rbs/`, `rbs/unnamed/`, `rubygems/`). 4.1.0
  added three: `file_constants.rbs` and `file_stat.rbs` (`File` split out) and
  `rbs/ops.rbs`; 4.1.1 changed nothing here (byte-identical to 4.1.0).
- `stdlib/<lib>/0/…` — 49 libs (the resolved transitive closure), 85 `.rbs`
  total, each with its `manifest.yaml` preserved for auditability:
  `abbrev base64 benchmark bigdecimal bigdecimal-math cgi cgi-escape csv date
  dbm delegate did_you_mean digest erb etc fileutils find forwardable
  io-console ipaddr json logger monitor mutex_m objspace observable open-uri
  open3 optparse pathname pp prettyprint pstore psych random-formatter resolv
  securerandom shellwords singleton socket stringio strscan tempfile time
  timeout tmpdir tsort uri yaml`.
  - `DEFAULT_LIBRARIES` lists 51 names; `prism` and `rbs` ship RBS with their
    own gems (not in this stdlib tree) and are skipped silently, exactly as the
    runtime loader does. `dbm`, `psych`, `socket` are pulled in transitively via
    manifest dependencies (e.g. `yaml` ⇒ `psych`, `csv` ⇒ no new, `pstore` ⇒
    `digest`/`pstore` deps, `resolv` ⇒ `socket`).

- `overlay/` — **not from the rbs gem**. The reference's own supplementary
  signatures, copied from `reference/rigor/data/`:
  - `overlay/core_overlay/` ⇐ `data/core_overlay/` (6 files) — reopens core
    classes to add methods upstream RBS omits but every concrete value answers
    (`Numeric#to_f`, `Pathname`, `CSV`, `Psych`, `StringScanner`, `Resolv`).
  - `overlay/vendored_gem_sigs/<gem>/` ⇐ `data/vendored_gem_sigs/<gem>/`
    (12 gems) — signatures for gems whose own RBS is missing or incomplete
    (`ast bcrypt bundler cgi did_you_mean idn-ruby mysql2 nokogiri pg racc redis
    rubygems`, plus the `*_extras.rbs` supplements). `racc` arrived with the
    `v0.3.2` pin and is not optional: `nokogiri.rbs` declares
    `Nokogiri::CSS::Parser < Racc::Parser`, and a declared superclass with no
    declaration anywhere took that whole definition down (upstream #299).
  - `overlay/rbs_shims/` ⇐ the **rbs GEM's** `sig/shims/` (`bundler.rbs`,
    `rubygems.rbs`, `enumerable.rbs`) — the one place this tree deliberately
    reaches into the `rbs` gem's own `sig/`, and the `v0.3.2` pin is what forced
    it. Upstream #300/#301 rewrote `data/vendored_gem_sigs/{bundler,rubygems}/`
    to STOP re-declaring anything these shims already declare (a duplicate
    raised `DuplicatedMethodDefinitionError` and collapsed the whole class).
    The reference still reads the shims — `rbs` is in `DEFAULT_LIBRARIES`, so its
    whole `sig/` tree loads on every run — so after that rewrite the shims are
    the ONLY declaration site for e.g. `Bundler.definition`,
    `Bundler.default_lockfile`, `Bundler::Definition#lockfile`,
    `Gem::Specification#name`. Without this copy rigor-rs's surface is strictly
    weaker than the oracle's and it witnesses those methods absent: measured
    2026-08-09, `Bundler.definition` and `Bundler.default_lockfile` were two
    live false positives that the 9204-file sweep does not reach (no corpus file
    calls them). Only `sig/shims/` is copied, never the rest of the gem's
    `sig/` — the whole tree would drag `RBS::*` into `knows_class`, the failure
    mode recorded for the `prism` supplement below.

  The reference loads BOTH in every run (`rbs_loader.rb`:
  `vendored_gem_sig_paths` then `core_overlay_sig_paths`, after the upstream
  set), so anything they declare is part of the ORACLE's surface. Since `v0.3.2`
  a few of those files are GATED on their library actually resolving
  (`LIBRARY_SUPPLEMENT_CORE_OVERLAYS` = `resolv.rbs`⇒`resolv`,
  `string_scanner.rbs`⇒`strscan`; `LIBRARY_SUPPLEMENT_VENDORED_DIRS` =
  `cgi`⇒`cgi`, `prism`⇒`prism`) because each carries an `| ...` overload
  continuation or a mixin that explodes with no base declaration. Every gating
  library except `prism` is in `DEFAULT_LIBRARIES` and in this tree, so the
  gates are all satisfied here and the loaded set is unchanged. Not vendoring
  them made rigor-rs's surface strictly weaker than the oracle's and produced
  false positives on methods the oracle resolves — measured on rigor-survey:
  `::DidYouMean.formatter`, which `data/vendored_gem_sigs/did_you_mean/` adds
  and upstream RBS does not declare. `ingest_embedded` loads `overlay/` LAST,
  mirroring the reference's order so an upstream declaration always wins.

  **`nokogiri/nokogiri.rbs` opens with a `class Object` REOPEN** (`def
  Nokogiri:`) — the only one in the whole `data/` tree, and the load-bearing
  reason the copy is not optional: it puts the method on EVERY receiver, so
  deleting it costs three `call.undefined-method` false positives (`"abc"`, `1`,
  `Object.new`) plus a `call.unresolved-toplevel` on the bare `Nokogiri(…)`
  form. Fixture 80 guards it; the whole `Object#`-level conversion-function
  family is swept in
  [this note](../../../../docs/notes/20260801-nokogiri-ingestion-asymmetry-closed.md).

  **`prism` is deliberately EXCLUDED** from the copy. Its file is a *supplement*
  (`prism_supplement.rbs`) to the prism gem's own `sig/`, which the reference
  loads via `DEFAULT_LIBRARIES` but this tree does not vendor (prism ships RBS
  with its own gem — see the note above). Loading the supplement alone declares
  `module Prism` without the gem's `Prism.parse`, which turns a class rigor-rs
  used to be silent about into a witnessed-absent one: 8 fresh
  `call.undefined-method` false positives on `Prism.parse` across
  rigor-survey `dependabot-core` and `rdoc-7.2.0` when it was included. A
  supplement is only safe here when the set it supplements is also vendored.

## Regenerate

`harness/vendor_rbs.py` IS the recipe — the prose version below used to be
executed by hand, which is how a refresh silently drifts:

```sh
python3 harness/vendor_rbs.py <rbs-gem-root>            # rewrite this tree
python3 harness/vendor_rbs.py <rbs-gem-root> --check    # verify, write nothing
```

It reads `DEFAULT_LIBRARIES` out of `src/rbs.rs` (one source of truth), copies
the whole `core/`, closes the library set transitively over each
`stdlib/<lib>/0/manifest.yaml`'s `dependencies:`, and carries `overlay/` +
this file across untouched. `--check` regenerates into a temp dir and diffs
against the committed tree — run it against the source gem named above and it
must report an exact match. Update this file's source version/path/date by hand
after a real bump.

Because the script does NOT touch `overlay/`, a REFERENCE-pin bump needs that
half re-synced by hand, and it moves independently of the rbs version — the
`v0.3.1 → v0.3.2` bump left `core/` + `stdlib/` byte-identical while rewriting
five overlay files and adding two directories:

```sh
rsync -a --delete --exclude README.md \
  reference/rigor/data/core_overlay/ \
  crates/rigor-index/vendor/rbs/overlay/core_overlay/
rsync -a --delete --exclude README.md --exclude prism/ \
  reference/rigor/data/vendored_gem_sigs/ \
  crates/rigor-index/vendor/rbs/overlay/vendored_gem_sigs/
cp "$(gem env gemdir)"/gems/rbs-<version>/sig/shims/*.rbs \
  crates/rigor-index/vendor/rbs/overlay/rbs_shims/
```

Then `diff -r` both `data/` halves against the copies: an "Only in reference"
line is a new file to consider, and the `prism` exclusion is the one deliberate
omission.

The closure is computed exactly as `CoreData::load()` does. Note that the
manifest set is NOT stable across rbs versions (4.1.0 gave `tempfile` its first
`manifest.yaml`), so a refresh must recompute the closure rather than copy the
previous file list.

At runtime the *resolved file list* is embedded directly (see `build.rs` →
`$OUT_DIR/embedded_rbs.rs`), so the `manifest.yaml` files here are for
audit/reproducibility only — they are not parsed in the embedded path.
