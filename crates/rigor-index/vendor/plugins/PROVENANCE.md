# Vendored plugin RBS signatures — provenance

This tree holds the **exact** RBS signature bundles shipped by Rigor's
config-gated plugins, vendored into the repo so the analyzer is standalone (no
runtime dependency on a local plugin gem checkout). Each plugin's RBS is
embedded at build time by `crates/rigor-index/build.rs` (the `EMBEDDED_PLUGIN_RBS`
table) and ingested by `CoreData::load_with_plugins()` (`src/rbs.rs`) ONLY when
the plugin id is named in `.rigor.yml`'s `plugins:` list (ADR-25). The default
(no-config) load path never touches these bytes, so default behaviour is
byte-unchanged.

The files here are **verbatim** copies — never hand-edit them; the byte-identity
with the reference's bundled RBS, fed through the SAME `ruby-rbs` parser as the
embedded core RBS, is the zero-false-positive keystone of plugin parity.

## Contents

### `activesupport-core-ext/`

- **Source plugin:** `rigor-activesupport-core-ext` (manifest id
  `activesupport-core-ext`, version `0.2.0`).
- **Source path:**
  `/Users/megurine/repo/ruby/rigor/plugins/rigor-activesupport-core-ext/sig/active_support/core_ext.rbs`
- **Vendored:** 2026-06-26
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

To refresh against a new plugin version, copy the plugin gem's
`sig/active_support/core_ext.rbs` BYTE-FOR-BYTE into
`activesupport-core-ext/active_support/core_ext.rbs` (preserve the relative
layout under the gem's `sig/`), then update this file's source version/path/date.
Never hand-edit the RBS — `shasum` it against the source to confirm identity.
