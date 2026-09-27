use super::*;

/// ADR-0042 Slice 4: a NESTED project-sig class is tracked by its QUALIFIED
/// name, so `Outer::Inner.new.spni` witnesses through the qualified path.
#[test]
fn qualified_nested_project_sig_provenance_and_witness() {
    let dir = std::env::temp_dir().join("rigor_qual_projsig_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("n.rbs"),
        "module Outer\n  class Inner\n    def spin: () -> Integer\n  end\nend\n",
    )
    .unwrap();
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    // Introduced-by-sig, tracked qualified.
    assert!(idx.is_qualified_project_sig_class("Outer::Inner"));
    assert!(idx.knows_qualified_class("Outer::Inner"));
    // Instance witness over the isolated qualified surface: valid present,
    // typo absent.
    assert!(idx.qualified_class_has_method("Outer::Inner", "spin"));
    assert!(!idx.qualified_class_has_method("Outer::Inner", "spni"));
    let _ = std::fs::remove_dir_all(&dir);
}
