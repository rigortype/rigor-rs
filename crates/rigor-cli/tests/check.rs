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

/// rigor-rs#366: a compound attribute write inside a literal block body is
/// still EVALUATED — in the block's own scope — so
/// `x.each { h.default ||= 0; h[:a].m }` runs `widen_attribute_write`
/// there and drops `h`'s indexed narrowings for the in-body reads that
/// follow it, while the OUTER scope's records survive and fire
/// post-block. Before `closure_evaluated` fed the in-block replay the
/// port kept the narrowing inside the body too, reporting a
/// `call.undefined-method` the reference never emits.
///
/// Every row below ends with a post-block `h[:a].frobnicate`, so a
/// passing row always reports exactly ONE diagnostic — the post-block
/// read — and the in-block read stays silent.
#[test]
fn block_attr_write_drops_indexed_narrowing_inside_block_only() {
    let probe = |body: &str| -> Vec<u8> {
        format!(
            "h = {{a: 1}}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\n[1].each {{ {body} }}\nh[:a].frobnicate\n",
        )
        .into_bytes()
    };
    for body in [
        // The issue row.
        "h.default ||= 0; h[:a].frobnicate",
        // Every hash-lookup mutator writer drops the record.
        "h.default_proc ||= nil; h[:a].frobnicate",
        "h.default &&= 0; h[:a].frobnicate",
        "h.default += 1; h[:a].frobnicate",
        "h&.default ||= 0; h[:a].frobnicate",
        // A write in an earlier conditional arm still drops: the scope
        // join removes a record absent on any path.
        "if x; h.default ||= 0; end; h[:a].frobnicate",
        "if x; h.default ||= 0; else; 1; end; h[:a].frobnicate",
        // Evaluation order, not source order — the modifier predicate
        // runs before the body, the loop predicate before each pass.
        "h[:a].frobnicate if h.default ||= 0",
        "h[:a].frobnicate while h.default ||= 0",
        // A write in a `case` arm drops for that arm's later statements.
        "case x; when 1; h.default ||= 0; h[:a].frobnicate; else; 2; end",
        // `ensure` runs on the joined scope — the protected body's drop
        // reaches it.
        "begin; h.default ||= 0; ensure; h[:a].frobnicate; end",
        // An outer-body write reaches a nested block body's read.
        "h.default ||= 0; [2].each { h[:a].frobnicate }",
    ] {
        let src = probe(body);
        let diags = check(&src);
        assert_eq!(
            diags.len(),
            1,
            "expected only the post-block read to fire for {src:?}, got {diags:?}"
        );
        assert!(
            diags[0].message.contains("frobnicate"),
            "expected `frobnicate` on the post-block read for {src:?}, got {diags:?}"
        );
    }
    // The same drop lands inside a lambda body's own scope.
    let src = b"h = {a: 1}\nh[:a] ||= \"s\"\n-> { h.default ||= 0; h[:a].frobnicate }\nh[:a].frobnicate\n";
    let diags = check(src);
    assert_eq!(
        diags.len(),
        1,
        "expected only the post-lambda read to fire for {src:?}, got {diags:?}"
    );
}

/// #366 controls — reads whose scope the write does NOT reach keep the
/// narrowing and still fire: a read evaluated BEFORE the write in the
/// body, a read in a sibling conditional arm or `rescue` clause (the
/// write's path never reaches it), a read inside the write's own RHS
/// operand, and a write whose scope is a NESTED block. Each row therefore
/// reports TWO diagnostics — in-block and post-block.
#[test]
fn block_attr_write_keeps_narrowing_for_reads_it_does_not_reach() {
    let probe = |body: &str| -> Vec<u8> {
        format!(
            "h = {{a: 1}}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\n[1].each {{ {body} }}\nh[:a].frobnicate\n",
        )
        .into_bytes()
    };
    for body in [
        // Read before the write — source order.
        "h[:a].frobnicate; h.default ||= 0",
        // Sibling conditional arms are alternative paths.
        "if x; h[:a].frobnicate; else; h.default ||= 0; end",
        "if x; h.default ||= 0; else; h[:a].frobnicate; end",
        // The protected body and a `rescue` clause are alternatives.
        "begin; h[:a].frobnicate; rescue; h.default ||= 0; end",
        "begin; h.default ||= 0; rescue; h[:a].frobnicate; end",
        // A `case` arm does not reach the `else` arm.
        "case x; when 1; h.default ||= 0; else; h[:a].frobnicate; end",
        // The write's own RHS operand reads the pre-write scope.
        "h.default ||= (h[:a].frobnicate; 0)",
        // A nested block's write belongs to the INNER body's scope — the
        // outer body's later read keeps the record.
        "[2].each { h.default ||= 0 }; h[:a].frobnicate",
        // A typed operand with no outliving effect never evaluates —
        // `puts(h.default ||= 0)` keeps the narrowing (rigor-rs#361).
        "puts(h.default ||= 0); h[:a].frobnicate",
        // A non-mutator writer (`foo=`) drops nothing.
        "h.foo ||= 0; h[:a].frobnicate",
    ] {
        let src = probe(body);
        let diags = check(&src);
        assert_eq!(
            diags.len(),
            2,
            "expected both reads to fire for {src:?}, got {diags:?}"
        );
    }
    // The operand-effects gate still lands the write inside the block:
    // `puts(h.default ||= (y = 1))` evaluates, so the later in-body read
    // is silent — one diagnostic.
    let src = b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { puts(h.default ||= (y = 1)); h[:a].frobnicate }\nh[:a].frobnicate\n";
    let diags = check(src);
    assert_eq!(
        diags.len(),
        1,
        "expected only the post-block read to fire for {src:?}, got {diags:?}"
    );
}

/// rigor-rs#379: inside a NESTED deferred body — a block in a block, a
/// lambda in a lambda — an earlier body statement's attr write still
/// drops `h`'s indexed narrowings for the reads that follow it in the
/// same inner body. `closure_mutations` keys the write to the innermost
/// body, but `closure_descend` applied earlier siblings under the
/// inherited (outer) owner — the re-key to `id` happened only on descent
/// into the site-holding child — so an inner-body write one statement
/// earlier than the read never matched.
///
/// Every row ends with a post-block `h[:a].frobnicate`, so a passing row
/// reports exactly ONE diagnostic — the post-block read.
#[test]
fn nested_block_attr_write_drops_indexed_narrowing() {
    for src in [
        // The issue row.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { h.default ||= 0; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // Nested lambdas.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n-> { -> { h.default ||= 0; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // The nested block sits under another statement of the outer body.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = [1].each { [2].each { h.default ||= 0; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // Three levels — the write keys to the innermost body.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { [3].each { h.default ||= 0; h[:a].frobnicate } } }\nh[:a].frobnicate\n"[..],
        // A write in a completing container's subtree still drops — the
        // joined post-scope has no record on the zero-iteration path.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\n[1].each { [2].each { while x.nil?; h.default ||= 0; break; end; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\n[1].each { [2].each { case x; when 1; h.default ||= 0; else; 2; end; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // Evaluation order inside the inner body — the modifier
        // predicate runs before its body.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { h[:a].frobnicate if h.default ||= 0 } }\nh[:a].frobnicate\n"[..],
    ] {
        let diags = check(src);
        assert_eq!(
            diags.len(),
            1,
            "expected only the post-block read to fire for {src:?}, got {diags:?}"
        );
        assert!(
            diags[0].message.contains("frobnicate"),
            "expected `frobnicate` on the post-block read for {src:?}, got {diags:?}"
        );
    }
    // Controls — inner-body writes that must NOT reach the read keep the
    // narrowing: each row reports TWO diagnostics.
    for src in [
        // Read before the write in the inner body.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { h[:a].frobnicate; h.default ||= 0 } }\nh[:a].frobnicate\n"[..],
        // An inner-block write does not reach the outer body's later
        // read (the deferred body may never run).
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { h.default ||= 0 }; h[:a].frobnicate }\nh[:a].frobnicate\n"[..],
        // … nor a sibling inner block's read.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { h.default ||= 0 }; [3].each { h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // A write inside a nested lambda is deferred past the read.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { -> { h.default ||= 0 }; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // Sibling `if` arms are alternative paths.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\n[1].each { [2].each { if x; h[:a].frobnicate; else; h.default ||= 0; end } }\nh[:a].frobnicate\n"[..],
        // A non-mutator writer drops nothing.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { h.foo ||= 0; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // The operand-effects gate: `puts(h.default ||= 0)` keeps the
        // narrowing (rigor-rs#361).
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { puts(h.default ||= 0); h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // A write in a block-parameter default does not order against
        // the body statements.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { |a = (h.default ||= 0)| h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // A write in a receiver-position block (`[3].map { … }`) keys to
        // that call, not to the `.each` body holding the read.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [3].map { h.default ||= 0 }.each { h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
    ] {
        let diags = check(src);
        assert_eq!(
            diags.len(),
            2,
            "expected both reads to fire for {src:?}, got {diags:?}"
        );
    }
}

#[test]
fn container_nested_block_attr_write_drops_indexed_narrowing() {
    // rigor-rs#380 — a literal block under a non-descending container
    // (`while`/`until`/`for`, `case`/`when`/`in`, `begin`'s else/ensure
    // arms) still replays its own closure mutations: the in-body read
    // drops `h`'s indexed narrowing and only the post-container read
    // fires.
    for src in [
        // `while`/`until` statement + modifier forms.
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\nwhile c\n  [1].each { h.default ||= 0; h[:a].frobnicate }\n  break\nend\nh[:a].frobnicate\n"[..],
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\nuntil c\n  [1].each { h.default ||= 0; h[:a].frobnicate }\n  break\nend\nh[:a].frobnicate\n"[..],
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\n[1].each { h.default ||= 0; h[:a].frobnicate } while c\nh[:a].frobnicate\n"[..],
        // `for` lowers to `Node::Loop` too.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nfor i in [1]\n  [1].each { h.default ||= 0; h[:a].frobnicate }\nend\nh[:a].frobnicate\n"[..],
        // `case`/`when` and `case`/`in` arms.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\ncase 1\nwhen Integer\n  [1].each { h.default ||= 0; h[:a].frobnicate }\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\ncase 1\nin Integer\n  [1].each { h.default ||= 0; h[:a].frobnicate }\nend\nh[:a].frobnicate\n"[..],
        // `begin`'s `else` and `ensure` arms.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nbegin\n  nil\nrescue\n  nil\nelse\n  [1].each { h.default ||= 0; h[:a].frobnicate }\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nbegin\n  nil\nensure\n  [1].each { h.default ||= 0; h[:a].frobnicate }\nend\nh[:a].frobnicate\n"[..],
        // Containers compose — nested `while`, `if` inside `while`.
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\nwhile c\n  while c\n    [1].each { h.default ||= 0; h[:a].frobnicate }\n    break\n  end\n  break\nend\nh[:a].frobnicate\n"[..],
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\nwhile c\n  if c\n    [1].each { h.default ||= 0; h[:a].frobnicate }\n  end\n  break\nend\nh[:a].frobnicate\n"[..],
        // A lambda body under a container descends the same way.
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\nuntil c\n  -> { h.default ||= 0; h[:a].frobnicate }\n  break\nend\nh[:a].frobnicate\n"[..],
        // A `rescue` clause's own replay path already descended.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nbegin\n  raise \"x\"\nrescue\n  [1].each { h.default ||= 0; h[:a].frobnicate }\nend\nh[:a].frobnicate\n"[..],
    ] {
        let diags = check(src);
        assert_eq!(
            diags.len(),
            1,
            "expected only the post-container read to fire for {src:?}, got {diags:?}"
        );
        assert!(
            diags[0].message.contains("frobnicate"),
            "expected `frobnicate` on the post-container read for {src:?}, got {diags:?}"
        );
    }
    // Controls — carriers the oracle does NOT evaluate as call sites keep
    // the in-body diagnostic: a `rescue`-modifier operand, a `break`
    // argument, an `END {}` body (inert). And the in-body read before the
    // write still fires under a `while` (positional replay). Each row
    // reports TWO diagnostics.
    for src in [
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ([1].each { h.default ||= 0; h[:a].frobnicate }) rescue nil\nh[:a].frobnicate\n"[..],
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\nwhile c\n  break [1].each { h.default ||= 0; h[:a].frobnicate }\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nEND { [1].each { h.default ||= 0; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        &b"c = true\nh = {a: 1}\nh[:a] ||= \"s\"\nwhile c\n  [1].each { h[:a].frobnicate; h.default ||= 0 }\n  break\nend\nh[:a].frobnicate\n"[..],
    ] {
        let diags = check(src);
        assert_eq!(
            diags.len(),
            2,
            "expected both reads to fire for {src:?}, got {diags:?}"
        );
    }
    // A block inside a `when`/`in` CONDITION does not replay on the
    // oracle either — the in-body read keeps firing.
    let diags = check(
        b"h = {a: 1}\nh[:a] ||= \"s\"\ncase 1\nwhen [1].each { h.default ||= 0; h[:a].frobnicate }\n  nil\nend\n",
    );
    assert_eq!(
        diags.len(),
        1,
        "expected the when-condition's in-body read to fire, got {diags:?}"
    );
}
