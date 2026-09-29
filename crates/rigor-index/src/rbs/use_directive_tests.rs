use super::*;

/// Issue #168: the `use` directives, `# resolve-type-names:` magic comment
/// and missing-referenced-type stubs a project `sig/` exercises, probed
/// through `receiver_method_return` / `is_synthesized_stub` — the surfaces
/// `call_dispatch` consults. Each test writes the .rbs fixtures its rows
/// need into its own temp dir (parallel `cargo test` shares none).
fn proj_dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rigor_use_dir_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, content) in files {
        std::fs::write(dir.join(file), content).unwrap();
    }
    dir
}

/// `use Foo::*` wildcard, `use X as Y` alias, and `use X` (same-name) all map
/// member-level references the way `UseMap::build_map` leaves them; an
/// interface alias lands on an interface (unqualified ⇒ the return lookup
/// declines to `Dynamic`, matching `Dynamic[top]`).
#[test]
fn use_directives_resolve_member_returns() {
    let dir = proj_dir(
        "alias_wildcard",
        &[
            (
                "foo.rbs",
                "module Foo\n  class Impl\n    def make: () -> Integer\n  end\n  interface _Bar\n    def bar: () -> String\n  end\nend\n",
            ),
            (
                "consumer.rbs",
                "use Foo::*\nuse Foo::Impl as AliasImpl\nuse Foo::_Bar as _Baz\n\nclass Consumer\n  def use_it: () -> Impl\n  def aliased: () -> AliasImpl\n  def aliased_i: () -> _Baz\nend\n",
            ),
        ],
    );
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    assert_eq!(
        idx.receiver_method_return("Consumer", "use_it"),
        Some(("Foo::Impl", false))
    );
    assert_eq!(
        idx.receiver_method_return("Consumer", "aliased"),
        Some(("Foo::Impl", false))
    );
    // `_Baz` ⇒ `Foo::_Bar` is an INTERFACE — never a `qualified` key — so the
    // return lookup declines (the reference reads an interface receiver as
    // `Dynamic[top]` anyway).
    assert_eq!(idx.receiver_method_return("Consumer", "aliased_i"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A wildcard import's local names shadow a root decl of the same spelling
/// (`use Ns::*` maps `Impl` ⇒ `Ns::Impl`, beating `::Impl`).
#[test]
fn use_wildcard_shadows_root_name() {
    let dir = proj_dir(
        "wildcard_shadow",
        &[
            ("roots.rbs", "class Impl\nend\n"),
            ("resolved.rbs", "module Ns\n  class Impl\n  end\nend\n"),
            (
                "use_ns.rbs",
                "use Ns::*\n\nclass WildConsumer\n  def w: () -> Impl\nend\n",
            ),
        ],
    );
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    assert_eq!(
        idx.receiver_method_return("WildConsumer", "w"),
        Some(("Ns::Impl", false))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RBS lexical resolution binds the nearest scope FIRST: `Impl` written
/// inside `module Ns` means `Ns::Impl`, even though a root `Impl` exists.
/// `# resolve-type-names: false` stands that resolution down — the written
/// name stays root-only. `resolve_leaf_unique`'s uniqueness requirement
/// would decline BOTH (a deterministic answer lost); the project path
/// resolves them.
#[test]
fn lexical_shadowing_and_resolve_type_names_directive() {
    let dir = proj_dir(
        "lexical",
        &[
            ("roots.rbs", "class Impl\nend\n"),
            (
                "resolved.rbs",
                "module Ns\n  class Impl\n  end\n  class Uses1\n    def m: () -> Impl\n  end\nend\n",
            ),
            (
                "noresolve.rbs",
                "# resolve-type-names: false\nmodule Ns\n  class Uses2\n    def m: () -> Impl\n  end\nend\n",
            ),
            (
                "noconsumers.rbs",
                "module Other\n  class Uses3\n    def m: () -> Impl\n  end\nend\n",
            ),
            (
                "rt_true.rbs",
                "# resolve-type-names: true\nmodule Ns\n  class Uses4\n    def m: () -> Impl\n  end\nend\n",
            ),
        ],
    );
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    // Innermost-first: `Ns::Impl` shadows `::Impl` for `Ns::Uses1#m`.
    assert_eq!(
        idx.receiver_method_return("Ns::Uses1", "m"),
        Some(("Ns::Impl", false))
    );
    // The magic comment leaves the name root-only: `::Impl`, never `Ns::Impl`.
    assert_eq!(
        idx.receiver_method_return("Ns::Uses2", "m"),
        Some(("Impl", false))
    );
    // A lexical miss falls through to root (`Other::Impl` absent).
    assert_eq!(
        idx.receiver_method_return("Other::Uses3", "m"),
        Some(("Impl", false))
    );
    // `true` resolves exactly like a file with no directive.
    assert_eq!(
        idx.receiver_method_return("Ns::Uses4", "m"),
        Some(("Ns::Impl", false))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `stub_missing_referenced_types`: a method type naming an undeclared
/// `MissingClass` synthesizes an empty class (a `Dynamic[top]` receiver in
/// the reference — `is_synthesized_stub` gates it), an undeclared `_Missing`
/// synthesizes an interface (which never joins the class/module stub set),
/// and a `use`-mapped missing name stubs at the RESOLVED spelling.
#[test]
fn missing_referenced_types_are_stubbed() {
    let dir = proj_dir(
        "stubs",
        &[
            ("foo.rbs", "module Foo\n  class Impl\n  end\nend\n"),
            (
                "consumer.rbs",
                "class Consumer\n  def missing_i: () -> _Missing\n  def missing_c: () -> MissingClass\n  def take: (_Missing2 x) -> Integer\nend\n",
            ),
            (
                "alias_miss.rbs",
                "use Foo::Nope\n\nclass AliasMiss\n  def n: () -> Nope\nend\n",
            ),
        ],
    );
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    // The class stub joins `synthesized_type_names`; the return still names
    // it (dispatch maps it to `Dynamic[top]` through `is_synthesized_stub`).
    assert_eq!(
        idx.receiver_method_return("Consumer", "missing_c"),
        Some(("MissingClass", false))
    );
    assert!(idx.is_synthesized_stub("MissingClass"));
    // The interface stub does NOT join the class/module set — but the name
    // was still recognized (a param-position reference stubbed too: the file
    // loads and `take` keeps its `Integer` return).
    assert!(!idx.is_synthesized_stub("_Missing"));
    assert_eq!(
        idx.receiver_method_return("Consumer", "missing_i"),
        None
    );
    assert_eq!(
        idx.receiver_method_return("Consumer", "take"),
        Some(("Integer", false))
    );
    // `use Foo::Nope` ⇒ `Nope` resolves to `Foo::Nope`, which is what gets
    // stubbed — not a root `Nope`.
    assert_eq!(
        idx.receiver_method_return("AliasMiss", "n"),
        Some(("Foo::Nope", false))
    );
    assert!(idx.is_synthesized_stub("Foo::Nope"));
    assert!(!idx.is_synthesized_stub("Nope"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The tuple twin resolves element names through the same project path
/// (`[Integer, Impl]` inside `module Foo` ⇒ `Foo::Impl`), and
/// `project_sig_chain_ok` reports an unbuildable superclass chain the way
/// the reference's `build_instance` failure reads.
#[test]
fn tuple_elements_and_unbuildable_chain() {
    let dir = proj_dir(
        "tuple_chain",
        &[
            (
                "foo.rbs",
                "module Foo\n  class Impl\n    def pair: () -> [Integer, Impl]\n  end\nend\n",
            ),
            (
                "sub.rbs",
                "class Sub < MissingBase\n  def real: () -> Integer\nend\n",
            ),
        ],
    );
    let idx = CoreData::load_for_project(&[], std::slice::from_ref(&dir));
    assert_eq!(
        idx.receiver_method_tuple_return("Foo::Impl", "pair"),
        Some(vec![
            RbsReturnShape::Class("Integer"),
            RbsReturnShape::Class("Foo::Impl"),
        ])
    );
    assert!(idx.project_sig_chain_ok("Foo::Impl"));
    assert!(!idx.project_sig_chain_ok("Sub"));
    let _ = std::fs::remove_dir_all(&dir);
}
