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

/// rigor-rs#343: `h.attr ||= v` / `&&=` / `+=` run
/// `eval_attribute_compound_write` — `widen_attribute_write` under the
/// WRITER name drops the indexed narrowings rooted at the receiver when
/// the writer is a known mutator (`default=`/`default_proc=`/
/// `compare_by_identity`). Before the port owned the node it lowered as
/// an unmodeled write: `h.default ||= 0` left the `h[:a]` -> `1`
/// narrowing live and `h[:a].upcase` reported on `Integer` — a false
/// positive the reference never emits.
#[test]
fn compound_attr_write_on_hash_mutator_invalidates_indexed_narrowing() {
    for src in [
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nh.default ||= 0\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nh.default &&= 0\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nh.default += 0\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nh.default_proc ||= ->(x, k) { 0 }\nh[:a].upcase\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nh.compare_by_identity\nh[:a].upcase\n"[..],
    ] {
        assert!(
            check(src).is_empty(),
            "expected zero diagnostics for {src:?}, got {:?}",
            check(src)
        );
    }
}

/// #343 — the write's widening lands from every position the reference
/// `eval`s: write RHS, `&&`/`||` operands, `if`/`unless`/`case` arms,
/// predicates, loop/`for` bodies, rescue clauses, `ensure`, `when`
/// conditions, parenthesised statements — including under a splat that
/// types the whole expression.
#[test]
fn compound_attr_write_evaluated_positions_land_the_widening() {
    let probe = |write: &str| -> Vec<u8> {
        format!(
            "h = {{a: 1}}\nh[:a] ||= \"s\"\n{write}\nh[:a].frobnicate\n",
        )
        .into_bytes()
    };
    for write in [
        "h.default ||= 0",
        "x = h.default ||= 0",
        "x = (h && (h.default ||= 0))",
        "x = (h.default ||= 0) && 1",
        "if h; h.default ||= 0; end",
        "x = (if h; h.default ||= 0; end)",
        "unless h.nil?; h.default ||= 0; end",
        "case 1; when 1; h.default ||= 0; end",
        "case h; when h.default ||= 0; end",
        "if h.default ||= 0; end",
        "while h[:b]; h.default ||= 0; end",
        "for i in [1]; h.default ||= 0; end",
        "begin; h.default ||= 0; rescue; end",
        "begin; nil; rescue; h.default ||= 0; end",
        "begin; nil; ensure; h.default ||= 0; end",
        "x = (h.default ||= 0; 1)",
        "x = (h.default ||= 0; 1) if h",
        "h&.default ||= 0",
        "h.default ||= (h.x ||= 1)",            // outer write evaluates, inner is its typed operand
    ] {
        let src = probe(write);
        let diags = check(&src);
        assert!(diags.is_empty(), "expected zero diagnostics for {src:?}, got {diags:?}");
    }
}

/// #343 controls — in a pure `type_of` operand position
/// `eval_attribute_compound_write` never runs, the `h[:a]` -> `"s" | 1`
/// narrowing survives, and `h[:a].frobnicate` keeps firing. Without
/// these rows the suppression above could be masking a plain decline.
#[test]
fn compound_attr_write_typed_positions_keep_the_narrowing() {
    let probe = |write: &str| -> Vec<u8> {
        format!(
            "h = {{a: 1}}\nh[:a] ||= \"s\"\n{write}\nh[:a].frobnicate\n",
        )
        .into_bytes()
    };
    for write in [
        "puts(h.default ||= 0)",                 // call argument
        "x = (h.default ||= 0).class",           // call receiver
        "puts(*[h.default ||= 0])",              // splat argument
        "x = [h.default ||= 0]",                 // array element
        "x = {b: h.default ||= 0}",              // hash value
        "x = \"#{h.default ||= 0}\"",          // interpolation
        "x = (h.default ||= 0)..9",              // range bound
        "x = h.default ||= 0, h[:a]",            // multi-assign RHS
        "x = (h.default ||= 0) rescue nil",      // rescue modifier
        "case h; in {a: x} if (h.default ||= 0); end", // in-pattern guard
        "x = defined?(h.default ||= 0)",         // never evaluated
        "[1].each { h.default ||= 0 }",          // deferred block body
        "l = -> { h.default ||= 0 }",            // lambda body
        "h.x ||= 0",                             // non-mutator writer
        "puts(*[(h.default ||= 0; 1)])",         // statements group inside a typed splat operand
    ] {
        let src = probe(write);
        let diags = check(&src);
        assert!(
            diags.iter().any(|d| d.rule_id == CALL_UNDEFINED_METHOD
                && d.message.contains("frobnicate")),
            "expected `frobnicate` to keep firing for {src:?}, got {diags:?}"
        );
    }
}

/// #343 — a nil receiver still fires through the write: `h.default ||= 0`
/// reads `h` (the reference's `scope.type_of(node)` reads it on `nil`), so
/// `h[:a]` afterwards still reports `for nil`.
#[test]
fn compound_attr_write_nil_receiver_still_fires() {
    let src = b"h = nil\nh.default ||= 0\nh[:a]\n";
    let diags = check(src);
    assert!(
        diags.iter().any(|d| d.message.contains("for nil")),
        "expected a nil diagnostic for {src:?}, got {diags:?}"
    );
}
