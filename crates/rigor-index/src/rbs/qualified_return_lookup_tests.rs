use super::*;

/// ADR-0042 Slice 5, the measurement's probe ladder: the return-lookup
/// family resolves NAMESPACED receivers through the qualified registry —
/// directly on the declaring class, one hop, and multi-hop down the
/// namespace-resolved superclass chain (`Digest::SHA256 < Digest::Base <
/// Digest::Class`, whose written references resolve exactly).
#[test]
fn qualified_singleton_return_probe_ladder() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("Digest::Class") {
        return; // stub fallback (no vendored rbs) — nothing to assert.
    }
    // Rung 1: directly on the DECLARING class.
    assert_eq!(idx.singleton_method_return("Digest::Class", "hexdigest"), Some("String"));
    // Rung 2: one hop (`Digest::SHA2 < Digest::Class`).
    assert_eq!(idx.singleton_method_return("Digest::SHA2", "hexdigest"), Some("String"));
    // Rung 3: multi-hop (`Digest::SHA256 < Digest::Base < Digest::Class`).
    assert_eq!(idx.singleton_method_return("Digest::SHA256", "hexdigest"), Some("String"));
    assert_eq!(idx.singleton_method_return("Digest::SHA256", "digest"), Some("String"));
    // `-> instance` late-binds to the QUALIFIED queried class.
    assert_eq!(
        idx.singleton_method_return("Digest::SHA256", "file"),
        Some("Digest::SHA256")
    );
    // A typo stays unresolved.
    assert_eq!(idx.singleton_method_return("Digest::SHA256", "hexdigset"), None);
    // An unknown qualified name stays unresolved.
    assert_eq!(idx.singleton_method_return("No::Such", "hexdigest"), None);
}

/// Rung 4: the INSTANCE side (`Digest::SHA256.new.hexdigest`) resolves
/// through the include chain (`Digest::Class include ::Digest::Instance` —
/// an ABSOLUTE written reference), and `-> self` resolves to the QUALIFIED
/// receiver.
#[test]
fn qualified_instance_return_through_absolute_include() {
    let idx = CoreData::load();
    if !idx.knows_qualified_class("Digest::Class") {
        return;
    }
    assert_eq!(idx.method_return("Digest::SHA256", "hexdigest"), Some("String"));
    assert_eq!(idx.method_return("Digest::SHA256", "digest_length"), Some("Integer"));
    // `Digest::Instance#reset: () -> self` ⇒ the qualified receiver.
    assert_eq!(idx.method_return("Digest::SHA256", "reset"), Some("Digest::SHA256"));
    // The nil-aware variant rides the same resolution.
    assert_eq!(
        idx.method_return_nilable("Digest::SHA256", "hexdigest"),
        Some(("String", false))
    );
    // A typo stays unresolved.
    assert_eq!(idx.method_return("Digest::SHA256", "hexdigset"), None);
}

/// A namespaced PROJECT-SIG class replica: instance + singleton returns
/// resolve through the qualified path (the measured silent rung — the
/// method was FOUND, its `-> String` return was LOST).
#[test]
fn qualified_project_sig_returns() {
    let dir = std::env::temp_dir().join("rigor_qual_return_projsig_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("f.rbs"),
        "module Foo\n  class Klass\n    def self.make: () -> String\n    def imake: () -> String\n    def iself: () -> self\n  end\nend\n",
    )
    .unwrap();
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    assert_eq!(idx.method_return("Foo::Klass", "imake"), Some("String"));
    assert_eq!(idx.singleton_method_return("Foo::Klass", "make"), Some("String"));
    assert_eq!(idx.method_return("Foo::Klass", "iself"), Some("Foo::Klass"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The slice's entire FP risk: an unqualified superclass leaf that RBS
/// resolution CANNOT pin down must DECLINE, never guess. `Base` written
/// bare inside `module FooAmb` resolves to neither `Random::Base` nor
/// `Digest::Base` (neither is on the lexical walk: `FooAmb::Base`, then
/// `::Base` — both absent), so inherited lookups yield None while the
/// leaf's OWN definitions (the resolved chain prefix) still answer.
#[test]
fn ambiguous_superclass_leaf_declines() {
    let dir = std::env::temp_dir().join("rigor_qual_return_amb_super_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("a.rbs"),
        "module FooAmb\n  class Sub < Base\n    def own_i: () -> Integer\n    def self.own_s: () -> Integer\n  end\nend\n",
    )
    .unwrap();
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    if !idx.knows_qualified_class("Digest::Base") {
        return; // stub fallback.
    }
    // The vendored set really does hold BOTH same-leaf candidates the
    // measurement named — the decline below is exercised against them.
    assert!(idx.knows_qualified_class("Random::Base"));
    // Inherited singleton/instance lookups DECLINE (the superclass link is
    // unresolvable — it must NOT resolve to Random::Base or Digest::Base,
    // whose `hexdigest` would otherwise leak in).
    assert_eq!(idx.singleton_method_return("FooAmb::Sub", "hexdigest"), None);
    assert_eq!(idx.method_return("FooAmb::Sub", "hexdigest"), None);
    // The leaf's own methods precede the break ⇒ still resolve.
    assert_eq!(idx.method_return("FooAmb::Sub", "own_i"), Some("Integer"));
    assert_eq!(idx.singleton_method_return("FooAmb::Sub", "own_s"), Some("Integer"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Member-level twin of the ambiguity decline: a flat return LEAF whose
/// namespace the tables discarded resolves only when the lexical walk has
/// exactly ONE candidate. `-> Class` written inside `module Digest` sees
/// BOTH `Digest::Class` and top-level `Class` ⇒ DECLINE; `-> Base` sees
/// only `Digest::Base` ⇒ resolves.
#[test]
fn ambiguous_member_return_leaf_declines() {
    let dir = std::env::temp_dir().join("rigor_qual_return_amb_member_test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("m.rbs"),
        "module Digest\n  class MakerZzz\n    def makec: () -> Class\n    def makeb: () -> Base\n  end\nend\n",
    )
    .unwrap();
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    if !idx.knows_qualified_class("Digest::Base") {
        return; // stub fallback.
    }
    assert_eq!(idx.method_return("Digest::MakerZzz", "makec"), None);
    assert_eq!(idx.method_return("Digest::MakerZzz", "makeb"), Some("Digest::Base"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Top-level-name control: the routing branch never fires for a name the
/// short map holds, so the existing short-key resolution is untouched.
#[test]
fn toplevel_return_lookup_control_unchanged() {
    let idx = CoreData::load();
    if !idx.knows_class("String") {
        return;
    }
    assert_eq!(idx.method_return("String", "upcase"), Some("String"));
    assert_eq!(idx.singleton_method_return("Time", "now"), Some("Time"));
    assert_eq!(idx.method_return_with_block("Array", "map"), Some("Array"));
    assert!(!idx.method_return_is_void("String", "upcase"));
}
