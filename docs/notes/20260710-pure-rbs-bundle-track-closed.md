# Pure-RBS bundle track — CLOSED by measurement (2026-07-10)

Follow-up to [the plugin-engine design slice](20260710-plugin-engine-design.md),
which recommended the productization-relevant plugin path was **pure-RBS bundle
expansion** ("vendor additional core-ext-style RBS bundles — more ActiveSupport
surface, other gems' core-ext … bounded (vendor RBS + register)"). This note
records the measurement that track produced: **there are no work items in it.**

## What was measured

Enumerated all 31 reference plugins (`reference/rigor/plugins/rigor-*`) and
classified each as pure-RBS (manifest declares ONLY `signature_paths`, empty
class body — the shipped, FP-safe, config-gated mechanism rigor-rs already has)
vs code-contributing (macros / FactStore producers / hooks — the unbuilt code
engine).

**Result: `activesupport-core-ext` is the ONLY pure-RBS plugin in the reference.**
Every other plugin contributes code:

- **Macro DSLs (ADR-16):** `devise` (`trait_registries`), `sinatra`
  (`block_as_methods`), `dry-struct` (`heredoc_templates`), `mangrove`
  (`nested_class_templates`).
- **FactStore producers (ADR-13):** `dry-schema`, `dry-validation`, `graphql`
  (`produces:`/`consumes:` + a `prepare` scanner), `activerecord`
  (`producer :model_index` + `config_schema` + `open_receivers`).
- The remaining ~20 (actionpack, activejob, rspec, pundit, sidekiq, sorbet, …)
  ship `lib/rigor/plugin/*.rb` code and NO `signature_paths` at all; most of
  their `.rbs` lives under `demo/sig/` (demo fixtures, not the shipped bundle).

Cross-checks:
- The one code-hook regex match against the AS plugin body was inside a comment
  (`# … no dynamic_return`) — it is genuinely pure.
- Where a code plugin ALSO ships real `sig/` (activerecord's
  `active_record/relation.rbs`, dry-validation's `dry_validation.rbs`), the RBS
  declares a NEW namespaced class, not a core-class reopening, and is inert
  without its code hook: the design note already established a pure-RBS AR bundle
  "declares the class but can't make any receiver *be* it" (open_receivers has no
  consumer without `dynamic_return`).

## The AS bundle is already byte-current

`shasum` of the vendored copy
(`crates/rigor-index/vendor/plugins/activesupport-core-ext/active_support/core_ext.rbs`)
== the current reference source
(`reference/rigor/plugins/rigor-activesupport-core-ext/sig/active_support/core_ext.rbs`):
both `d31d19b02e09914f5cfdd8a5e4820c440bd5d5ea`. So the "more ActiveSupport
surface" refresh path is a no-op too — the vendored bundle carries the full
current selector set (the ~40 the reference ships). Expanding it FURTHER means
adding selectors the reference itself doesn't have — that is upstream work, not a
faithful port (and a bounded selector list is not "genuinely unreasonable", so
the port-discipline exception doesn't apply).

## Conclusion

The pure-RBS bundle mechanism is at its natural completion: the sole pure-RBS
plugin is vendored and byte-current, and no second one exists to add. Combined
with:
- the **code engine** deferred (interdependent, thin, no paying thin-slice —
  [design note](20260710-plugin-engine-design.md)),
- the **Gemfile.lock auto-overlay** already matching the reference's
  `GEM_OVERLAY_PLUGIN_IDS = { "activesupport" => "activesupport-core-ext" }`
  exactly (ADR-72),

⇒ **the entire plugin track is now assessed done-or-deferred.** There is no
cheap, FP-safe, faithful-port plugin work remaining. This is the fourth
"big track, thin/absent value" finding of the session (after remaining CLI
commands, possible-nil/ivar, and the code engine), all sharing the root cause the
design note names: rigor-rs's leniency + pure-RBS design already captures the
FP-safe value.

**Next real frontier is one of the substantial, ADR-backed tracks** (substrate
for sig-gen/trace/coverage; the code engine on a measured Rails gap; or §12 LSP
two-tier infra) — none of them a cheap slice. See CURRENT_WORK "NEXT SESSION".
