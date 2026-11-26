//! Rescue clause entry: keep unrelated bindings while declining stale globals.

use rigor_index::CoreIndex;
use rigor_parse::{lower, parse};
use rigor_rules::{CALL_UNDEFINED_METHOD, analyze};
use rigor_types::Interner;

fn undefined_methods(source: &[u8]) -> Vec<(usize, String)> {
    let ast = lower(&parse(source));
    analyze(&ast, &mut Interner::new(), &CoreIndex::new())
        .into_iter()
        .filter(|diag| diag.rule_id == CALL_UNDEFINED_METHOD)
        .map(|diag| (diag.start_offset, diag.message))
        .collect()
}

#[test]
fn rescue_keeps_last_line_when_protected_body_cannot_set_it() {
    let source = b"$_ = 1\nbegin\n  0\nrescue\n  $_.chomp\nend\n";
    assert_eq!(
        undefined_methods(source),
        [(29, "undefined method `chomp' for 1".into())],
    );
}
