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
