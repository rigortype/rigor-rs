use super::*;

/// Probes v1/v3: `URI::HTTP#host` and `URI::Generic#host`. `host` is an
/// `attr_reader` on `URI::Generic` (`stdlib/uri/0/generic.rbs:245`), so
/// this row needs BOTH halves: attribute ingestion, and the written
/// superclass walk `URI::HTTP < Generic` that reaches `URI::Generic`
/// (its short-key leaf `Generic` is the wrong surface).
#[test]
fn uri_attr_reader_host_is_present_on_the_subclass() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("URI::HTTP") {
        return;
    }
    assert!(idx.qualified_class_has_method("URI::Generic", "host"));
    assert!(idx.qualified_class_has_method("URI::HTTP", "host"));
    // Writers too (`attr_accessor` halves are separate names).
    assert!(idx.qualified_class_has_method("URI::Generic", "scheme"));
    // The control: a genuine typo is still witnessed as absent.
    assert!(!idx.qualified_class_has_method("URI::HTTP", "frobnicate_zzz"));
}

/// Probes p7b/v2: `Digest::SHA256#hexdigest` / `#digest`, inherited over
/// `Digest::Base → Digest::Class → include ::Digest::Instance`. BOTH links
/// are ambiguous leaves (`Base` ∈ {`Random::Base`, `Digest::Base`},
/// `Class` ∈ {`::Class`, `Digest::Class`}), which is why the short-key walk
/// lost them; the written chain resolves them exactly.
#[test]
fn digest_inherited_methods_resolve_through_the_written_chain() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("Digest::SHA256") {
        return;
    }
    assert!(idx.qualified_class_has_method("Digest::SHA256", "hexdigest"));
    assert!(idx.qualified_class_has_method("Digest::SHA256", "digest"));
    // q4b: own method on the declaring class.
    assert!(idx.qualified_class_has_method("Digest::Class", "digest"));
    // q5b: a MODULE's own method.
    assert!(idx.qualified_class_has_method("Digest::Instance", "hexdigest"));
}

/// The must-STILL-FIRE side. q4 pins that a qualified name gets NO
/// leaf-fallback: `Digest::Class` must not see `::Class#superclass`. q4c
/// pins that an ambiguous leaf (`Base`) is no obstacle, and q5 that a
/// qualified MODULE is a witnessable target.
#[test]
fn absence_is_still_witnessed_without_leaf_fallback() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("Digest::Class") {
        return;
    }
    // q4: `superclass` is a `::Class` instance method, NOT on Digest::Class.
    assert!(!idx.qualified_class_has_method("Digest::Class", "superclass"));
    // q4c: `Random::Base` shares its leaf with `Digest::Base`.
    assert!(!idx.qualified_class_has_method("Random::Base", "frobnicate_zzz"));
    // q5: a module target witnesses absence.
    assert!(!idx.qualified_class_has_method("Digest::Instance", "frobnicate_zzz"));
    // Object-level methods stay present (p7c).
    assert!(idx.qualified_class_has_method("Digest::Instance", "frozen?"));
}

/// Attribute ingestion on the SHORT-key surface too (`class_has_method` is
/// the same hole), and on an overlay gem class whose whole surface is
/// attributes (`Gem::Specification`, `rubygems_extras.rbs:114`).
#[test]
fn attribute_members_are_present_on_both_surfaces() {
    let idx = CoreData::load();
    // `Bundler::Source::Git` (`bundler.rbs:182`) declares ONE `def` and
    // seven `attr_accessor`s — before S1 every one of those seven read as
    // proven absent. (`Gem::Specification`, the other all-attribute overlay
    // class, is in `UNBUILDABLE_DEFINITIONS` and is silent for that reason
    // instead, so it cannot serve as the row here.)
    if idx.knows_qualified_class("Bundler::Source::Git") {
        assert!(idx.qualified_class_has_method("Bundler::Source::Git", "uri"));
        assert!(idx.qualified_class_has_method("Bundler::Source::Git", "uri="));
        assert!(!idx.qualified_class_has_method("Bundler::Source::Git", "frobnicate_zzz"));
    }
    if idx.knows_class("Generic") {
        assert!(idx.class_has_method("Generic", "host"));
    }
}
