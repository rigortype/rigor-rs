use super::*;
use rigor_parse::{lower, parse};

fn void_diags(tag: &str, src: &[u8]) -> Vec<Diagnostic> {
    // A UNIQUE dir per calling test: the two tests in this module run in
    // parallel, and a shared fixed path races one test's remove_dir_all
    // against the other's sig ingestion (flaked on ubuntu CI: the Widget
    // sig vanished mid-build and the singleton case read 0 diagnostics).
    let dir = std::env::temp_dir().join(format!("rigor_void_rule_test_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("w.rbs"),
        "class Widget\n  def fire: () -> void\n  def spin: () -> Integer\n  def self.reset: () -> void\nend\n",
    )
    .unwrap();
    let index = CoreIndex::for_project(&[], std::slice::from_ref(&dir));
    let ast = lower(&parse(src));
    let source = rigor_infer::SourceIndex::build(&ast, &index);
    let mut interner = Interner::new();
    let out = void_value_use_diagnostics(&ast, &mut interner, &index, &source);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// The three value contexts fire (assignment RHS, argument, receiver);
/// message byte-matched against the live reference under
/// `--bleeding-edge=use-of-void-value`.
#[test]
fn void_value_contexts_fire() {
    let d = void_diags(
        "contexts",
        b"w = Widget.new\nx = w.fire\nputs(w.fire)\nw.fire.to_s\n@i = w.fire\n",
    );
    assert_eq!(d.len(), 4, "{d:?}");
    assert!(d.iter().all(|x| x.rule_id == STATIC_VALUE_USE_VOID));
    assert_eq!(
        d[0].message,
        "value use of `void': `Widget#fire' declares `-> void', so its return recovers to `top' and should not be used as a value"
    );
    // The singleton spelling labels with a dot.
    let s = void_diags("contexts", b"y = Widget.reset\n");
    assert_eq!(s.len(), 1, "{s:?}");
    assert!(s[0].message.contains("`Widget.reset'"), "{}", s[0].message);
}

/// Silent: a bare-statement void call (the declared contract), a non-void
/// method, and an unresolvable receiver.
#[test]
fn void_bare_statement_and_nonvoid_stay_silent() {
    assert!(void_diags("silent", b"w = Widget.new\nw.fire\n").is_empty());
    assert!(void_diags("silent", b"w = Widget.new\ny = w.spin\n").is_empty());
    assert!(void_diags("silent", b"y = unknown_thing.fire\n").is_empty());
}

/// PARITY GUARD: a MULTI-WRITE RHS is not a value context. The reference's
/// `VoidValueUseCollector::WRITE_NODE_CLASSES` deliberately excludes
/// `Prism::MultiWriteNode` ("its `value` is a container the void call would
/// have to be spread through, not a direct use"), so adding the
/// `Node::MultiWrite` arena lowering must not make it fire.
#[test]
fn void_multi_write_rhs_stays_silent() {
    assert!(void_diags("multiwrite", b"w = Widget.new\na, b = w.fire\n").is_empty());
}
