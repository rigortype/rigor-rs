use super::*;

/// rigor-rs#482: load a project `sig/` holding `files` (name, source).
fn load(tag: &str, files: &[(&str, &str)]) -> (CoreData, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("rigor_overloading_reopen_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, src) in files {
        std::fs::write(dir.join(name), src).unwrap();
    }
    (CoreData::load_for_project(&[], std::slice::from_ref(&dir)), dir)
}

/// A reopen of an original declared on the class itself: the envelopes
/// union and a disagreeing return declines.
#[test]
fn reopen_combines_with_own_original() {
    let (idx, dir) = load(
        "own",
        &[("a.rbs", "class Foo\n  def m: () -> Integer\n  def g: () -> String\nend\nclass Foo\n  def m: (Integer) -> Integer | ...\n  def g: (Integer) -> Integer | ...\nend\n")],
    );
    assert_eq!(idx.method_arity("Foo", "m"), Some((0, Some(1))));
    assert_eq!(idx.method_return("Foo", "m"), Some("Integer"));
    assert_eq!(idx.method_return("Foo", "g"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A reopen in an earlier file than its original combines with it and does
/// not make the class unbuildable.
#[test]
fn reopen_before_original_in_project_phase() {
    let (idx, dir) = load(
        "order",
        &[
            ("a.rbs", "class Foo\n  def m: (Integer) -> Integer | ...\nend\n"),
            ("b.rbs", "class Foo\n  def m: () -> Integer\n  def n: () -> Integer\nend\n"),
        ],
    );
    assert_eq!(idx.method_arity("Foo", "m"), Some((0, Some(1))));
    assert_eq!(idx.method_return("Foo", "n"), Some("Integer"));
    assert!(!idx.class_has_method("Foo", "zork"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A reopen continuing an INHERITED original (superclass, `Object`) keeps
/// the class buildable and unions with the inherited envelope.
#[test]
fn reopen_of_inherited_original() {
    let (idx, dir) = load(
        "inh",
        &[(
            "a.rbs",
            "class Object\n  def inspect: (Integer) -> String | ...\nend\nclass Foo\n  def bar: () -> Integer\nend\nclass Sub < Foo\n  def bar: (Integer) -> Integer | ...\nend\n",
        )],
    );
    assert_eq!(idx.method_arity("Object", "inspect"), Some((0, Some(1))));
    assert_eq!(idx.method_arity("Sub", "bar"), Some((0, Some(1))));
    assert_eq!(idx.method_return("Sub", "bar"), Some("Integer"));
    assert!(!idx.class_has_method("Sub", "zork"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A reopen with no original anywhere fails the class's build, and the
/// instance-side failure takes the singleton side too.
#[test]
fn reopen_without_original_is_unbuildable() {
    let (idx, dir) = load(
        "none",
        &[("a.rbs", "class Foo\n  def nope: () -> Integer | ...\n  def self.ok: () -> Integer\n  def fine: () -> Integer\nend\n")],
    );
    assert_eq!(idx.method_return("Foo", "fine"), None);
    assert_eq!(idx.singleton_method_return("Foo", "ok"), None);
    // Unbuildable reads as "may have it" — never witnessed absent.
    assert!(idx.class_has_method("Foo", "zork"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// rigor-rs#494: an instance-side build failure that reaches a class through
/// the ancestor CLOSURE (a cyclic superclass/mixin chain, or an included
/// module whose own build fails) takes the SINGLETON side with it — the
/// reference builds the singleton definition on the instance one
/// (`RecursiveAncestorError`, `InvalidOverloadMethodError`). Only
/// `(q, singleton)` joins the blanking set; the instance side already
/// declines these classes at dispatch (`qualified_ancestors` reports the
/// chain incomplete).
#[test]
fn closure_failure_marks_singleton_unbuildable() {
    // Cyclic chains (fixture 156's sig/, un-reopened): `A < B < A`,
    // `Self < Self` (with `Sub < Self`), and `M1 ⇄ M2` (with `UsesM`
    // including `M1`). `Both`'s diamond stays buildable.
    let (idx, dir) = load(
        "cyc",
        &[(
            "x.rbs",
            "class A < B\n  def a: () -> Integer\nend\nclass B < A\nend\nclass C\n  def c: () -> Integer\nend\nclass Self < Self\n  def s: () -> Integer\nend\nmodule M1\n  include M2\nend\nmodule M2\n  include M1\nend\nclass UsesM\n  include M1\nend\nclass Sub < Self\nend\nmodule M3\nend\nmodule M4\n  include M3\nend\nclass Both\n  include M3\n  include M4\nend\n",
        )],
    );
    for q in ["A", "B", "Self", "Sub", "M1", "M2", "UsesM"] {
        assert!(
            idx.class_has_singleton_method(q, "zork"),
            "{q}: a cyclic ancestor chain must mark the singleton side unbuildable"
        );
    }
    // Controls: off-cycle `C` and diamond `Both` still witness.
    assert!(!idx.class_has_singleton_method("C", "zork"));
    assert!(!idx.class_has_singleton_method("Both", "zork"));
    let _ = std::fs::remove_dir_all(&dir);

    // An included module whose own instance build fails (`nope` overloads
    // no original — `InvalidOverloadMethodError`) fails the includer's
    // singleton through the closure.
    let (idx, dir) = load(
        "inc",
        &[(
            "i.rbs",
            "module M\n  def nope: () -> Integer | ...\nend\nclass Inc\n  include M\n  def self.iok: () -> Integer\nend\n",
        )],
    );
    assert!(
        idx.class_has_singleton_method("Inc", "nosuch"),
        "Inc: an included module's build failure must reach the singleton side"
    );
    assert_eq!(idx.singleton_method_return("Inc", "iok"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// rigor-rs#494 x #493 item 12: a param-mismatch (`def_nil`) class — a bare
/// `class Hash` — must NOT seed the closure-failure singleton mirror. Its nil
/// def is traversable-empty for descendants (`Kernel` fails one hop through
/// `Kernel#Hash`, yet `String.zork` / `C.zork` still fire, oracle-measured),
/// while a genuine cyclic chain in the same sig still blanks its singleton.
#[test]
fn def_nil_class_is_not_a_closure_seed() {
    let (idx, dir) = load(
        "defnil_seed",
        &[(
            "x.rbs",
            "class Hash\nend\nclass C\n  def c: () -> Integer\nend\nclass A < B\nend\nclass B < A\nend\n",
        )],
    );
    // Controls: surviving singletons keep witnessing past a nil `Kernel`.
    assert!(!idx.class_has_singleton_method("String", "zork"));
    assert!(!idx.class_has_singleton_method("C", "zork"));
    // The genuine cycle still fails its singleton (#494).
    assert!(idx.class_has_singleton_method("A", "zork"));
    assert!(idx.class_has_singleton_method("B", "zork"));
    let _ = std::fs::remove_dir_all(&dir);
}
