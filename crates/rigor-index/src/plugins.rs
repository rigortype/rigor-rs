//! Config-gated, bundled RBS plugins (ADR-25, first plugin slice).
//!
//! A bundled plugin is a PURE-RBS payload: it ships NO analyzer code, only a
//! vendored RBS bundle (the reference's `signature_paths:`) that **reopens core
//! classes** to add extension selectors. Activating a plugin means ingesting its
//! RBS on top of the embedded core via the SAME `ruby-rbs` parser + reopen-union
//! merge the core RBS uses (see [`crate::rbs::CoreData::load_with_plugins`]), so
//! the index is byte-identical to feeding the reference's bundled RBS through the
//! core path — the zero-false-positive keystone.
//!
//! **Gating is mandatory.** A plugin is applied ONLY when its id appears in
//! `.rigor.yml`'s `plugins:` list (resolved through [`bundled_plugin`]). The
//! default (no-config) load path never references this module, so default
//! behaviour stays byte-unchanged. Unknown / unbundled plugin ids resolve to
//! `None` and are silently ignored (never an error), matching the reference,
//! which simply can't load a gem it doesn't have.

/// A bundled, config-gated RBS plugin. `rbs` is one or more
/// `(relative-path, file-contents)` entries — the same `(name, contents)` shape
/// [`crate::rbs::ingest_rbs_source`] accepts — embedded at build time via
/// `include_str!` (no runtime filesystem dependency, mirroring the embedded core
/// RBS precedent in `build.rs`).
pub struct BundledPlugin {
    /// The plugin's manifest id (e.g. `"activesupport-core-ext"`).
    pub id: &'static str,
    /// The plugin's vendored RBS payload, `(name, contents)` per file.
    pub rbs: &'static [(&'static str, &'static str)],
}

/// `rigor-activesupport-core-ext` — the highest-leverage Rails plugin. A
/// PURE-RBS bundle reopening Object / String / Integer / Float / Time / Date /
/// DateTime / Array / Hash / Enumerable / NilClass / TrueClass / FalseClass with
/// the most-frequently-flagged ActiveSupport core-extension selectors. The RBS is
/// vendored verbatim under `vendor/plugins/` (see that tree's `PROVENANCE.md`)
/// and embedded byte-for-byte here.
pub const ACTIVESUPPORT_CORE_EXT: BundledPlugin = BundledPlugin {
    id: "activesupport-core-ext",
    rbs: &[(
        "active_support/core_ext.rbs",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/vendor/plugins/activesupport-core-ext/active_support/core_ext.rbs"
        )),
    )],
};

/// All bundled plugins, for iteration in tests / future dispatch.
const ALL: &[&BundledPlugin] = &[&ACTIVESUPPORT_CORE_EXT];

/// Resolve a plugin id from `.rigor.yml` to its bundled payload, or `None` for an
/// unknown / unbundled id (NEVER an error — the reference likewise can't load a
/// gem it doesn't have, so an unknown id is simply inert).
///
/// Accepts BOTH spellings the reference recognises: the gem name
/// (`"rigor-activesupport-core-ext"`) AND the manifest id
/// (`"activesupport-core-ext"`). The gem name is the `manifest.id` prefixed with
/// `rigor-`, so we normalise by stripping a leading `rigor-` before matching.
pub fn bundled_plugin(id: &str) -> Option<&'static BundledPlugin> {
    let normalized = normalize_id(id);
    ALL.iter()
        .copied()
        .find(|p| p.id == normalized)
}

/// Normalise a plugin id to its manifest-id form: trim surrounding whitespace and
/// strip a leading `rigor-` gem-name prefix. So both
/// `"rigor-activesupport-core-ext"` and `"activesupport-core-ext"` map to
/// `"activesupport-core-ext"`. The single normalization seam (the design's
/// gem-name ↔ manifest-id reconciliation lives HERE, not in the config layer).
fn normalize_id(id: &str) -> &str {
    let trimmed = id.trim();
    trimmed.strip_prefix("rigor-").unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_id_resolves() {
        let p = bundled_plugin("activesupport-core-ext").expect("bundled");
        assert_eq!(p.id, "activesupport-core-ext");
        assert_eq!(p.rbs.len(), 1);
        assert_eq!(p.rbs[0].0, "active_support/core_ext.rbs");
        assert!(!p.rbs[0].1.is_empty(), "embedded RBS payload must be present");
    }

    #[test]
    fn gem_name_alias_resolves_identically() {
        // The gem name (`rigor-` prefixed) resolves to the SAME bundled plugin.
        let by_gem = bundled_plugin("rigor-activesupport-core-ext").expect("bundled");
        let by_id = bundled_plugin("activesupport-core-ext").expect("bundled");
        assert!(std::ptr::eq(by_gem, by_id));
    }

    #[test]
    fn whitespace_tolerant() {
        assert!(bundled_plugin("  activesupport-core-ext  ").is_some());
    }

    #[test]
    fn unknown_id_is_none_never_errors() {
        assert!(bundled_plugin("not-a-real-plugin").is_none());
        assert!(bundled_plugin("").is_none());
        assert!(bundled_plugin("rigor-").is_none());
    }

    #[test]
    fn embedded_payload_carries_core_ext_selectors() {
        // Sanity: the vendored bytes are the ActiveSupport bundle (reopens String
        // with `squish`, Integer with `minutes`, Time with `self.current`).
        let p = bundled_plugin("activesupport-core-ext").unwrap();
        let rbs = p.rbs[0].1;
        assert!(rbs.contains("def squish"));
        assert!(rbs.contains("def minutes"));
        assert!(rbs.contains("def self.current"));
    }
}
