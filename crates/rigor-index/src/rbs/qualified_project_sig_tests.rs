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

/// upstream #2438 — `memberless_project_class?`: a project-sig decl with no
/// surface member (method, attribute, alias or mixin) is member-less; one
/// with a member is not; and a bundled name reopened member-less in `sig/`
/// is NOT project-authored, so it is not member-less either.
#[test]
fn memberless_project_class_tracks_surface_and_authorship() {
    let dir = std::env::temp_dir().join("rigor_memberless_sig_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("m.rbs"),
        "class Empty::Deep\nend\n\
         class My::Sub < Resolv::DNS::Resource::IN::A\nend\n\
         module Mixin::Thing\nend\n\
         class Full::Klass\n  def inst: () -> Integer\nend\n\
         class SelfMeth::Klass\n  def self.q: () -> Integer\nend\n\
         class String\nend\n",
    )
    .unwrap();
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    // Member-less project decls: classes AND modules, nested and nested
    // with a written superclass.
    assert!(idx.memberless_project_class("Empty::Deep"));
    assert!(idx.memberless_project_class("My::Sub"));
    assert!(idx.memberless_project_class("Mixin::Thing"));
    // Membered decls are never member-less.
    assert!(!idx.memberless_project_class("Full::Klass"));
    assert!(!idx.memberless_project_class("SelfMeth::Klass"));
    // A bundled class reopened member-less in sig/ is not project-authored.
    assert!(!idx.memberless_project_class("String"));
    // Unknown / bundled-only names are not member-less either.
    assert!(!idx.memberless_project_class("No::Such"));
    assert!(!idx.memberless_project_class("Resolv::DNS"));
    // `memberless_project_superclass`: the member-less decl's own written
    // superclass resolved to its qualified key; nil when the decl wrote
    // none or isn't member-less.
    if idx.knows_qualified_class("Resolv::DNS::Resource::IN::A") {
        assert_eq!(
            idx.memberless_project_superclass("My::Sub"),
            Some("Resolv::DNS::Resource::IN::A")
        );
    }
    assert_eq!(idx.memberless_project_superclass("Empty::Deep"), None);
    assert_eq!(idx.memberless_project_superclass("Full::Klass"), None);
    let _ = std::fs::remove_dir_all(&dir);
}
