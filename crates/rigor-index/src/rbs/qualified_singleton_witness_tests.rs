use super::*;

/// ADR-0042 Slice 2: `class_has_singleton_method` transparently resolves a
/// QUALIFIED namespaced receiver via the qualified registry, with the full
/// base-object surface, so a real method is present and a typo is ABSENT —
/// and ERB::Util / CGI::Util stay method-disjoint (no short-key merge).
#[test]
fn qualified_singleton_witness_split_and_ancestry() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("ERB::Util") {
        return; // stub fallback (no vendored rbs) — nothing to assert.
    }
    // Real method present (ERB::Util declares `self?.html_escape`).
    assert!(idx.class_has_singleton_method("ERB::Util", "html_escape"));
    // Genuine typo ABSENT (own surface complete + base surface known).
    assert!(!idx.class_has_singleton_method("ERB::Util", "no_such_method"));
    // MERGE-collision split: CGI::Util-only `pretty` is absent on ERB::Util
    // and vice-versa, despite the shared short key "Util".
    assert!(!idx.class_has_singleton_method("ERB::Util", "pretty"));
    assert!(!idx.class_has_singleton_method("CGI::Util", "html_escape"));
    // A base-object method (`name`, from Module) is present on any class obj.
    assert!(idx.class_has_singleton_method("ERB::Util", "name"));
    // A singleton ALIAS resolves (`alias self.h self.html_escape`) — the
    // measured rails FP. Present, not witnessed absent.
    assert!(idx.class_has_singleton_method("ERB::Util", "h"));
    // An unknown qualified name stays silent (assume-present).
    assert!(idx.class_has_singleton_method("No::Such", "whatever"));
    // Measure-first scope: a qualified CLASS (not module) stays SILENT even
    // for a genuine typo — its inherited class-method chain is not walked in
    // this slice (the measured dependabot `Gem::Specification` FP). Use a
    // qualified class known to exist; `Gem::Specification` is heavily
    // reopened in the vendored rbs.
    if idx.knows_qualified_class("Gem::Specification") {
        assert!(idx.class_has_singleton_method("Gem::Specification", "no_such_zzz"));
    }
}
