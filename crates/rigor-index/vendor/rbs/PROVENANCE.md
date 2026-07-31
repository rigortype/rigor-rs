# Vendored RBS signatures — provenance

This tree is the **exact** RBS signature set that `rigor-index` loads, vendored
into the repo so the analyzer is standalone (no runtime dependency on a local
`rbs` gem). It is embedded at build time by `crates/rigor-index/build.rs` and
ingested by `CoreData::load()` (`src/rbs.rs`) when `RIGOR_RBS_CORE_DIR` is unset.

- **Source gem:** `rbs-4.1.0`
- **Source path:** `/Users/megurine/.local/share/mise/installs/ruby/4.0.5/lib/ruby/gems/4.0.0/gems/rbs-4.1.0`
- **Vendored:** 2026-07-31 (was `rbs-4.0.3`, 2026-06-26 — bumped with the
  reference pin to `v0.3.1`, which follows rbs 4.1.0; the two pins must match)
- **What the set is:** the WHOLE `core/` directory ⊕ the `DEFAULT_LIBRARIES`
  stdlib set (`src/rbs.rs`) transitively closed over each lib's
  `manifest.yaml` `dependencies:` — i.e. byte-for-byte the set the old runtime
  path ingested. **Not** the entire `stdlib/` tree; only the loaded closure.

## Contents

- `core/` — all 89 `.rbs` from `…/rbs-4.1.0/core` (nested under `io/`,
  `enumerator/`, `object_space/`, `rbs/`, `rbs/unnamed/`, `rubygems/`). 4.1.0
  added three: `file_constants.rbs` and `file_stat.rbs` (`File` split out) and
  `rbs/ops.rbs`.
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
  - `overlay/core_overlay/` ⇐ `data/core_overlay/` (5 files) — reopens core
    classes to add methods upstream RBS omits but every concrete value answers
    (`Numeric#to_f`, `Pathname`, `CSV`, `Psych`, `StringScanner`).
  - `overlay/vendored_gem_sigs/<gem>/` ⇐ `data/vendored_gem_sigs/<gem>/`
    (12 gems) — signatures for gems whose own RBS is missing or incomplete
    (`ast bcrypt bundler cgi did_you_mean idn-ruby mysql2 nokogiri pg redis
    rubygems`, plus the `*_extras.rbs` supplements).

  The reference loads BOTH unconditionally in every run (`rbs_loader.rb`:
  `vendored_gem_sig_paths` then `core_overlay_sig_paths`, after the upstream
  set), so anything they declare is part of the ORACLE's surface. Not vendoring
  them made rigor-rs's surface strictly weaker than the oracle's and produced
  false positives on methods the oracle resolves — measured on rigor-survey:
  `::DidYouMean.formatter`, which `data/vendored_gem_sigs/did_you_mean/` adds
  and upstream RBS does not declare. `ingest_embedded` loads `overlay/` LAST,
  mirroring the reference's order so an upstream declaration always wins.

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

The closure is computed exactly as `CoreData::load()` does. Note that the
manifest set is NOT stable across rbs versions (4.1.0 gave `tempfile` its first
`manifest.yaml`), so a refresh must recompute the closure rather than copy the
previous file list.

At runtime the *resolved file list* is embedded directly (see `build.rs` →
`$OUT_DIR/embedded_rbs.rs`), so the `manifest.yaml` files here are for
audit/reproducibility only — they are not parsed in the embedded path.
