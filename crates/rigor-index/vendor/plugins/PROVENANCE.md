# Vendored plugin RBS signatures — provenance

This tree holds the **exact** RBS signature bundles shipped by Rigor's
config-gated plugins, vendored into the repo so the analyzer is standalone (no
runtime dependency on a local plugin gem checkout). Each plugin's RBS is
embedded by direct `include_str!` in `src/plugins.rs` (no `build.rs` table —
this file once claimed an `EMBEDDED_PLUGIN_RBS` table that never existed)
and ingested by `CoreData::load_with_plugins()` (`src/rbs.rs`) ONLY when
the plugin id is named in `.rigor.yml`'s `plugins:` list (ADR-25). The default
(no-config) load path never touches these bytes, so default behaviour is
byte-unchanged.

The files here are **verbatim** copies — never hand-edit them; the byte-identity
with the reference's bundled RBS, fed through the SAME `ruby-rbs` parser as the
embedded core RBS, is the zero-false-positive keystone of plugin parity.

## Contents

### `activesupport-core-ext/`

- **Source plugin:** `rigor-activesupport-core-ext` (manifest id
  `activesupport-core-ext`).
- **Source path:** `reference/rigor/plugins/rigor-activesupport-core-ext/sig/active_support/core_ext.rbs`
  — **the PINNED submodule**, not a local checkout.
- **Vendored:** 2026-08-25 at the `v0.3.4` pin (`b10bd5df`), `shasum`
  `996ccf62856134ee4fb20efaee32c88a9a3fc143`. Was 2026-06-26, from
  `/Users/megurine/repo/ruby/rigor/plugins/…` — a local WORKING checkout, which
  is the hazard `UPSTREAM.md` records as hazard 3 applied to a different file.
  The copy then sat unmoved for two months while upstream's grew, and the drift
  was **10 live false positives** (`titlecase`, `dasherize`, `upcase_first`,
  `remove`, `remove!`, `in?`, `Time#advance`, `Time#all_day`, `Date#advance`,
  `Date#all_day`) that **neither sweep tool can see** — `fp_audit.py` runs both
  sides from a clean cwd, so no `.rigor.yml` and no plugin. Harness fixture 98
  is what grades this file now; step 3 of the `UPSTREAM.md` ritual re-syncs it.
  [note](../../../../docs/notes/20260825-upstream-survey-v034-master.md)
- **What it is:** a PURE-RBS plugin (ships NO analyzer code — its whole
  contribution is the manifest's `signature_paths: ["sig"]`). The bundled
  `core_ext.rbs` reopens core classes (Object / String / Integer / Float / Time /
  Date / DateTime / Array / Hash / Enumerable / NilClass / TrueClass /
  FalseClass) to add the ~40 most-frequently-flagged ActiveSupport core-extension
  selectors (`blank?` / `present?` / `presence` / `squish` / `underscore` /
  `camelize` / `pluralize` / `minutes` / `hours` / `days` / `current` /
  `symbolize_keys` / `pluck` / `second` / …). It is mapped under
  `active_support/core_ext.rbs` mirroring the gem's `sig/` layout.

## Regenerate

**Re-sync at every pin bump** (`UPSTREAM.md` step 3): copy the PINNED
submodule's plugin `sig/` BYTE-FOR-BYTE, preserving the relative layout under
the gem's `sig/`, then update this file's source path/date/`shasum`. Never
hand-edit the RBS.

```sh
cp reference/rigor/plugins/rigor-activesupport-core-ext/sig/active_support/core_ext.rbs \
   crates/rigor-index/vendor/plugins/activesupport-core-ext/active_support/core_ext.rbs
shasum reference/rigor/plugins/rigor-activesupport-core-ext/sig/active_support/core_ext.rbs \
       crates/rigor-index/vendor/plugins/activesupport-core-ext/active_support/core_ext.rbs
```

Then `ruby harness/run.rb` — fixture 98 is the gate, and it is the ONLY
instrument that sees this file: both sweep tools run from a clean cwd, so no
`.rigor.yml` is read and no plugin is ever loaded.

> **Upstream keeps a second copy of this surface, and it is NOT the one to
> vendor.** `data/gem_overlay/activesupport/core_ext.rbs` (ADR-72) is the
> auto-applied twin, loaded when `activesupport` is locked in the project's
> Gemfile.lock but ships no RBS; it carries the same selectors deliberately and
> stands down when the plugin id is loaded. rigor-rs implements the PLUGIN
> mechanism (ADR-25, `.rigor.yml`-gated), so the plugin's `sig/` is the
> authoring home and the right source. The two upstream files are not identical
> — at `v0.3.4` the plugin's is 867 lines to the overlay's 477 — so copying the
> wrong one silently vendors a weaker surface.
