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
    // rigor-rs#908: a qualified CLASS witnesses absence over its inherited
    // class-method chain — `Gem::Specification` (heavily reopened in the
    // vendored rbs) answers `load` from `Gem::BasicSpecification`'s singleton
    // surface while `no_such_zzz` is absent on a complete chain. The old
    // measure-first bail stayed silent on classes because the chain walk
    // did not exist (the dependabot `Gem::Specification` FPs were inherited
    // methods judged absent — exactly what the walk now resolves).
    if idx.knows_qualified_class("Gem::Specification") {
        assert!(idx.class_has_singleton_method("Gem::Specification", "load"));
        assert!(!idx.class_has_singleton_method("Gem::Specification", "no_such_zzz"));
    }
}

/// rigor-rs#908: a deeply-namespaced class-decl singleton (`Resolv::DNS::
/// Resource::IN::A`, five segments) walks the qualified superclass chain
/// (`IN::A < Resource < Query < Object < BasicObject`) — `bogus` is absent
/// (fires `singleton(Resolv::DNS::Resource::IN::A)`) while `name`/`new`
/// from the base-object surface stay present. `Digest::SHA256 < Digest::
/// Base < Digest::Class` carries the inherited-singleton control.
#[test]
fn qualified_class_singleton_walks_superclass_chain() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("Resolv::DNS::Resource::IN::A") {
        return; // stub fallback — nothing to assert.
    }
    assert!(!idx.class_has_singleton_method("Resolv::DNS::Resource::IN::A", "bogus"));
    assert!(idx.class_has_singleton_method("Resolv::DNS::Resource::IN::A", "name"));
    assert!(idx.class_has_singleton_method("Resolv::DNS::Resource::IN::A", "new"));
    assert!(!idx.class_has_singleton_method("Resolv::DNS", "bogus"));
    // Inherited singleton surface: `Digest::Class` declares the `digest` /
    // `file` class methods `Digest::SHA256` answers (the measured-FP family).
    if idx.knows_qualified_class("Digest::SHA256") {
        assert!(idx.class_has_singleton_method("Digest::SHA256", "digest"));
        assert!(idx.class_has_singleton_method("Digest::SHA256", "file"));
        assert!(!idx.class_has_singleton_method("Digest::SHA256", "bogus"));
    }
}
