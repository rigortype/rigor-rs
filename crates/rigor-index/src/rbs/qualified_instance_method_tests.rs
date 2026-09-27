use super::*;

/// ADR-0042 Slice 3: `qualified_class_has_method` resolves the leaf's own
/// instance surface + ancestry (Object/Kernel/BasicObject via the short
/// chain) + instance aliases; a genuine typo on a top-level class is
/// witnessed ABSENT and a real method (incl. an inherited one) present.
#[test]
fn qualified_instance_own_ancestry_and_alias() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("String") {
        return; // stub fallback.
    }
    // Own method present; inherited (Object#frozen?) present; typo absent.
    assert!(idx.qualified_class_has_method("String", "upcase"));
    assert!(idx.qualified_class_has_method("String", "frozen?"));
    assert!(!idx.qualified_class_has_method("String", "no_such_zzz"));
    // An instance alias resolves (`String#size` aliases `length`).
    assert!(idx.qualified_class_has_method("String", "size"));
    // Unknown qualified name ⇒ silent (assume-present).
    assert!(idx.qualified_class_has_method("No::Such", "whatever"));
}
