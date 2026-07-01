//! End-to-end integration of the tracer-bullet `check` pipeline:
//! parse -> lower -> rules, asserting the headline `call.undefined-method`
//! firing, its precise span/message, and the zero-false-positive cases
//! (ADR-0002/0023/0030).

use rigor_index::CoreIndex;
use rigor_parse::{lower, parse};
use rigor_rules::{analyze, Diagnostic, CALL_UNDEFINED_METHOD};
use rigor_types::Interner;

fn check(src: &[u8]) -> Vec<Diagnostic> {
    let ast = lower(&parse(src));
    let mut interner = Interner::new();
    let index = CoreIndex::new();
    analyze(&ast, &mut interner, &index)
}

#[test]
fn headline_lenght_typo_fires_once_with_precise_span_and_message() {
    let src = b"s = \"Hello\"\ns.lenght\n";
    let diags = check(src);

    assert_eq!(diags.len(), 1, "expected exactly one diagnostic: {diags:?}");
    let d = &diags[0];
    assert_eq!(d.rule_id, CALL_UNDEFINED_METHOD);
    assert_eq!(d.message, "undefined method `lenght' for \"Hello\"");
    // The span keys exactly on the `lenght` token (parity surface, ADR-0002).
    assert_eq!(&src[d.start_offset..d.end_offset], b"lenght");
}

#[test]
fn known_method_yields_zero_diagnostics() {
    let diags = check(b"s = \"Hello\"\ns.length\n");
    assert!(diags.is_empty(), "expected zero diagnostics: {diags:?}");
}

#[test]
fn dynamic_receiver_yields_zero_diagnostics() {
    // `@x` is an untyped ivar (Dynamic[top]); `foo` on it must stay silent rather
    // than guess (zero-false-positive, ADR-0023). An ivar receiver (not a bare
    // implicit-self call) also keeps `call.unresolved-toplevel` out of the way —
    // a bare `x.foo` would (correctly) fire unresolved-toplevel on the receiver
    // `x`, which is a separate rule with its own coverage.
    let diags = check(b"@x.foo\n");
    assert!(diags.is_empty(), "expected zero diagnostics: {diags:?}");
}
