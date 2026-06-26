# Vendored RBS signatures — provenance

This tree is the **exact** RBS signature set that `rigor-index` loads, vendored
into the repo so the analyzer is standalone (no runtime dependency on a local
`rbs` gem). It is embedded at build time by `crates/rigor-index/build.rs` and
ingested by `CoreData::load()` (`src/rbs.rs`) when `RIGOR_RBS_CORE_DIR` is unset.

- **Source gem:** `rbs-4.0.3`
- **Source path:** `/Users/megurine/.local/share/mise/installs/ruby/4.0.5/lib/ruby/gems/4.0.0/gems/rbs-4.0.3`
- **Vendored:** 2026-06-26
- **What the set is:** the WHOLE `core/` directory ⊕ the `DEFAULT_LIBRARIES`
  stdlib set (`src/rbs.rs`) transitively closed over each lib's
  `manifest.yaml` `dependencies:` — i.e. byte-for-byte the set the old runtime
  path ingested. **Not** the entire `stdlib/` tree; only the loaded closure.

## Contents

- `core/` — all 86 `.rbs` from `…/rbs-4.0.3/core` (62 top-level + nested under
  `io/`, `enumerator/`, `object_space/`, `rbs/`, `rbs/unnamed/`, `rubygems/`).
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

## Regenerate

The closure is computed exactly as `CoreData::load()` does (whole `core/` +
`DEFAULT_LIBRARIES` transitive `manifest.yaml` closure). To refresh against a new
rbs gem version, point a script at the gem's `core/` + `stdlib/`, walk
`DEFAULT_LIBRARIES` (from `src/rbs.rs`) closing over each
`stdlib/<lib>/0/manifest.yaml`'s `dependencies:`, and copy each present
`core/` tree and `stdlib/<lib>/0/` dir here, preserving structure and the
`manifest.yaml` files. Then update this file's source version/path/date.

At runtime the *resolved file list* is embedded directly (see `build.rs` →
`$OUT_DIR/embedded_rbs.rs`), so the `manifest.yaml` files here are for
audit/reproducibility only — they are not parsed in the embedded path.
