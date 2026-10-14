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
/// predicates, loop/`for` bodies, rescue clauses, `ensure`,
/// parenthesised statements — including under a splat that
/// types the whole expression. (`when`/`in` conditions are NOT here:
/// `Narrowing.case_when_scopes` shape-narrows them without `sub_eval` —
/// rigor-rs#362 moved that row to the keeps test.)
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
        "case c; when h.default ||= 0; end",           // when condition (rigor-rs#362)
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
/// `[1].each { h.default ||= 0; h[:a].m }` runs `widen_attribute_write`
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

/// rigor-rs#381 — an attr write inside an INERT carrier in a block body
/// (`END { }`/`BEGIN { }`/`defined?`/`super`/`yield` operands — the
/// reference never evaluates them in sequence) must not be replayed into
/// the block's scope: replaying dropped `h[:a]`'s indexed records where
/// the oracle keeps them. `closure_mutations` now applies the same
/// `in_inert_carrier` retain `toplevel_mutations` has. Every row keeps
/// `h[:a]` = `"s" | 1`, so BOTH the in-body and the post-block read fire.
#[test]
fn inert_carrier_attr_write_inside_block_keeps_indexed_narrowing() {
    for src in [
        // The issue row — `END { }` inside a block.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { END { h.default ||= 0 }; h[:a].frobnicate }\nh[:a].frobnicate\n"[..],
        // … at nested depth (the review extension: the owner re-key now
        // applies those writes one level in too).
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { END { h.default ||= 0 }; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
        // A `defined?` operand inside a block.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { defined?(h.default ||= 0); h[:a].frobnicate }\nh[:a].frobnicate\n"[..],
        // A block nested INSIDE `END` — its body never runs either.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nEND { [1].each { h.default ||= 0; h[:a].frobnicate } }\nh[:a].frobnicate\n"[..],
    ] {
        let diags = check(src);
        assert_eq!(
            diags.len(),
            2,
            "expected both reads to fire for {src:?}, got {diags:?}"
        );
    }
    // Must-still-fire: an EVALUATED attr write inside the block still
    // lands — it widens the receiver for the in-body read (silent there)
    // while the post-block read keeps `"s" | 1` and fires.
    let src = b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { h.default ||= 0; h[:a].frobnicate }\nh[:a].frobnicate\n";
    let diags = check(src);
    assert_eq!(
        diags.len(),
        1,
        "expected only the post-block read to fire for {src:?}, got {diags:?}"
    );
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

#[test]
fn container_nested_block_keeps_flat_env() {
    // rigor-rs#380 (review follow-up) — descending to a nested block's
    // barrier under a non-descending container must NOT recapture
    // `flow.env`, the end-of-file env: a local reassigned AFTER the
    // container stays invisible inside the earlier block, matching the
    // oracle. Every row is fully silent.
    for src in [
        &b"x = \"s\"\nwhile x\n  [1].each { x.upcase }\n  break\nend\nx = 1\n"[..],
        &b"x = \"s\"\nuntil x\n  [1].each { x.upcase }\n  break\nend\nx = 1\n"[..],
        &b"x = \"s\"\nfor _ in [1]\n  [1].each { x.upcase }\nend\nx = 1\n"[..],
        &b"x = \"s\"\ncase x\nwhen \"s\"\n  [1].each { x.upcase }\nend\nx = 1\n"[..],
        &b"x = \"s\"\nbegin\n  nil\nensure\n  [1].each { x.upcase }\nend\nx = 1\n"[..],
        &b"x = \"s\"\nbegin\n  raise \"e\"\nrescue\n  nil\nelse\n  [1].each { x.upcase }\nend\nx = 1\n"[..],
        &b"x = \"s\"\nwhile x\n  l = -> { x.upcase }\n  break\nend\nx = 1\n"[..],
    ] {
        let diags = check(src);
        assert!(
            diags.is_empty(),
            "expected silence — the post-container rebind must not leak into the block — for {src:?}, got {diags:?}"
        );
    }
    // Nested containers compose the same way (the only diagnostic is the
    // oracle-matching always-truthy warning on `if c`).
    let diags = check(
        b"c = true\nx = \"s\"\nif c\n  while x\n    [1].each { x.upcase }\n    break\n  end\nend\nx = 1\n",
    );
    assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
    assert!(
        diags[0].message.contains("always truthy"),
        "expected only the always-truthy warning, got {diags:?}"
    );
    // Control — with `x = 1` BEFORE the container the read still fires,
    // and it fires `for 1` (the flat env's answer), not `for "s"` (the
    // end-of-file env's): the barrier must not widen the message either.
    let diags = check(b"x = 1\nwhile x\n  [1].each { x.upcase }\n  break\nend\nx = \"s\"\n");
    assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
    assert!(
        diags[0].message.contains("for 1"),
        "expected `upcase` reported `for 1`, got {diags:?}"
    );
}

/// rigor-rs#388: a `local[k]` index write inside a content write-back
/// position — a literal block/lambda body or a `while`/`until` body —
/// REBINDS the receiver on the reference
/// (`content_writeback_block_captures` / `loop_content_writeback`), so
/// every indexed narrowing rooted at it dies and the post-block read is
/// silent. The port kept the pre-block record: `h[:a] ||= "t"` inside
/// `each` widened `h` but left `h[:a]` -> `"s" | 1` live, and the later
/// `h[:a].frobnicate` fired where the oracle never emits.
///
/// Every row below is fully silent on BOTH engines.
#[test]
fn writeback_index_write_drops_indexed_narrowing() {
    for src in [
        // The issue row — and the `&&=` / `op=` compound forms.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { h[:a] ||= \"t\" }\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { h[:a] &&= \"t\" }\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { h[:a] += 1 }\nh[:a].frobnicate\n"[..],
        // A plain `h[k] = v` drops the whole record set the same way —
        // the write-back rebind is not slot-scoped (a `h[:b]` write also
        // kills the `h[:a]` record).
        &b"h = {a: 1, b: 2}\nh[:a] ||= \"s\"\nh[:b] ||= \"x\"\n[1].each { h[:b] = \"t\" }\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\nwhile x\n  h[:a] = \"t\"\n  break\nend\nh[:a].frobnicate\n"[..],
        // Nested deferred bodies and `while`/`until`/`loop do` bodies.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n[1].each { [2].each { h[:a] ||= \"t\" } }\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\nwhile x\n  h[:a] ||= \"t\"\n  break\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\nuntil x\n  h[:a] ||= \"t\"\n  break\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nloop do\n  h[:a] ||= \"t\"\n  break\nend\nh[:a].frobnicate\n"[..],
        // `while` stays a write-back boundary under an `if` arm, and a
        // block stays one under a `for` body.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\nwhile x\n  if x\n    h[:a] ||= \"t\"\n  end\n  break\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nfor _i in [1]\n  [1].each { h[:a] ||= \"t\" }\nend\nh[:a].frobnicate\n"[..],
        // Lambda and `proc`/`lambda` bodies capture the same way, even
        // unassigned or passed to an unresolved receiver.
        &b"h = {a: 1}\nh[:a] ||= \"s\"\n->() { h[:a] ||= \"t\" }\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nproc { h[:a] ||= \"t\" }\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ->() { h[:a] ||= \"t\" }\nh[:a].frobnicate\n"[..],
    ] {
        let diags = check(src);
        assert!(
            diags.is_empty(),
            "expected silence — the write-back rebind drops the record — for {src:?}, got {diags:?}"
        );
    }
}

/// #388 controls — JOINED positions keep the record (the arm's own eval
/// re-records the slot into the join), so the post-statement read still
/// fires: an `if`/`case` arm, a `&&`/`?:` operand, a `rescue`/`ensure`
/// clause, a `rescue`-modifier operand, a `for` body and a `while`
/// predicate. A straight-line `h[k] ||= v` keeps updating the slot to
/// `narrow_truthy(old) | rhs` — `for "s" | "t" | 1` — unchanged.
#[test]
fn joined_index_write_keeps_indexed_narrowing() {
    for src in [
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\nif x\n  h[:a] ||= \"t\"\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\ncase 1\nwhen Integer\n  h[:a] ||= \"t\"\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\nx && (h[:a] ||= \"t\")\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = ENV[\"K\"]\nx ? (h[:a] ||= \"t\") : nil\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nbegin\n  raise \"e\"\nrescue\n  h[:a] ||= \"t\"\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nbegin\n  nil\nensure\n  h[:a] ||= \"t\"\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nx = (h[:a] ||= \"t\") rescue nil\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nfor _i in [1]\n  h[:a] ||= \"t\"\nend\nh[:a].frobnicate\n"[..],
        &b"h = {a: 1}\nh[:a] ||= \"s\"\nwhile (h[:a] ||= \"t\")\n  break\nend\nh[:a].frobnicate\n"[..],
    ] {
        let diags = check(src);
        assert_eq!(
            diags.len(),
            1,
            "expected the joined record to keep the read firing for {src:?}, got {diags:?}"
        );
        assert!(
            diags[0].message.contains("frobnicate"),
            "expected `frobnicate` on the post-statement read for {src:?}, got {diags:?}"
        );
    }
    // Straight-line `h[k] ||= v` still records the narrowed slot — the
    // exact union is the parity surface (`"s" | "t" | 1`).
    let diags =
        check(b"h = {a: 1}\nh[:a] ||= \"s\"\nh[:a] ||= \"t\"\nh[:a].frobnicate\n");
    assert_eq!(diags.len(), 1, "expected one diagnostic: {diags:?}");
    assert!(
        diags[0].message.contains("\"s\" | \"t\" | 1"),
        "expected the `||=` stored-slot union `for \"s\" | \"t\" | 1`, got {diags:?}"
    );
}

/// rigor-rs#382: `call.argument-type-mismatch`'s class-level rejection is the
/// reference's `class_subtype_result`, which resolves BOTH names via
/// `const_get` on the host process (acceptance.rb `resolve_class`). An arg
/// class that is index-known but never host-loaded — a project-`sig/` class,
/// vendored-but-unrequired stdlib — answers `maybe`, never a proven `no`, so
/// `String`/`String?` params stay silent on it; host-resolvable args still
/// reject (including `Superclass`, `actual <= target` being false).
#[test]
fn argument_mismatch_requires_host_resolvable_arg_class() {
    let dir = std::env::temp_dir().join("rigor_i382_sig");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("w.rbs"),
        "class Widget\n  def take_optstr: (String? x) -> void\n  def take_string: (String x) -> void\n  def take_widget: (Widget w) -> void\nend\n",
    )
    .unwrap();
    let index = CoreIndex::for_project(&[], std::slice::from_ref(&dir));
    let mut interner = Interner::new();

    let src = b"w = Widget.new\nw.take_optstr(w)\nw.take_string(w)\nw.take_widget(\"s\")\nw.take_string(42)\n";
    let diags = analyze(&lower(&parse(src)), &mut interner, &index);
    assert_eq!(
        diags.len(),
        2,
        "sig-only `Widget` args must not reject; resolvable args still fire: {diags:?}"
    );
    assert!(
        diags.iter().any(|d| d.message.contains("expected Widget, got \"s\"")),
        "host-resolvable `String` arg into sig-only `Widget` param still fires: {diags:?}"
    );
    assert!(
        diags.iter().any(|d| d.message.contains("expected String, got 42")),
        "core `Integer` arg into `String` param still fires: {diags:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// rigor-rs#386 — dead-position leaks: writes the oracle never lets bind
/// must not reach the reads `local_reach` scans, and an arm that provably
/// never returns keeps its writes out of the post-`begin` scope. Every row
/// is oracle-SILENT; each was a `call.undefined-method` FP on master.
#[test]
fn dead_position_writes_do_not_leak() {
    for src in [
        // `branch_terminates?`'s Bot half: a clause ending in a
        // non-returning call contributes nothing to the post-`begin`
        // scope — `Kernel.raise`, and a same-file toplevel `def` whose
        // body is provably bot (`def die; raise; end`).
        &b"def die\n  raise\nend\nbegin\n  1\nrescue\n  q = 1\n  die\nend\nFloat(q).w\n"[..],
        &b"begin\n  1\nrescue\n  q = 1\n  Kernel.raise\nend\nFloat(q).w\n"[..],
        // A write in a folded-dead `if` arm inside an iterated body
        // (`loop do`/`while`) reaches no read: the loop is not a carrier
        // for a dead write, and the post-loop `q = 1` cannot loop back
        // into the body's `if q` predicate.
        &b"loop do\n  if false\n    q = 1\n  end\n  Float(q).w\n  break\nend\n"[..],
        &b"q = nil\nwhile c\n  if q\n    [1].each { q.frobnicate }\n  end\n  break\nend\nq = 1\n"[..],
        // A `rescue` clause evaluates from the `begin`'s ENTRY scope —
        // a write in the protected body never reaches a read inside a
        // clause, whether the clause retries or not
        // (`widen_entry_for_retry` carries only entry-bound names).
        &b"begin\n  q = 1\nrescue\n  Float(q).w\nend\n"[..],
        &b"tries = 0\nbegin\n  q = 1\nrescue\n  tries += 1\n  retry if tries < 2\n  Float(q).w\nend\n"[..],
        // …but a loop holding BOTH the write and the clause read is a
        // carrier: a later pass's protected-body write lands in the
        // clause's entry scope, so `q` is `nil | 1` there and the call
        // declines (rigor-rs#386 review — the clause-boundary rule must
        // not fire across a loop back-edge).
        &b"q = nil\nwhile c\n  begin\n    q = 1\n  rescue\n    q.frobnicate\n  end\nend\n"[..],
        &b"q = nil\nuntil c\n  begin\n    q = 1\n  rescue\n    q.frobnicate\n  end\nend\n"[..],
        &b"q = nil\nwhile true\n  begin\n    q = 1\n  rescue\n    q.frobnicate\n  end\nend\n"[..],
        // `begin q = 1 rescue Float(q).w end` lowers as
        // `q = (1 rescue Float(q).w)` — the write's own RHS is evaluated
        // BEFORE the binding lands, so its `q` read sees nothing.
        &b"begin q = 1 rescue Float(q).w end\n"[..],
        // `Const.method` predicates fold through the singleton
        // literal-tail table (`def self.off? = !on?`), so the then-arm
        // is dead and its write reaches nothing.
        &b"class C\n  def self.on? = true\n  def self.off? = !on?\nend\nif C.off?\n  q = 1\nend\nFloat(q).w\n"[..],
        &b"class C\n  def self.off? = false\nend\nif C.off?\n  q = 1\nend\nFloat(q).w\n"[..],
        &b"[1].each do\n  begin\n    raise\n  rescue\n    q = 1\n    raise\n  end\n  Float(q).w\nend\n"[..],
        // A statement-level block's own PARAMETER binds the name — `v`
        // reads the yielded value (the untyped carrier for `h.each`), so
        // neither an outer write nor a conditional in-block write lands a
        // literal `Float` could accept (the `u18` row of corpus
        // `99_untyped_arg_overloads.rb`).
        &b"def u(h)\n  h.each do |_k, v|\n    v = \"x\" if v.nil?\n    Float(v).frobnicate\n  end\nend\n"[..],
    ] {
        let diags = check(src);
        assert!(
            diags.iter().all(|d| d.rule_id != CALL_UNDEFINED_METHOD),
            "expected no call.undefined-method for {src:?}, got {diags:?}"
        );
    }
    // Must-still-fire controls — neighbouring shapes whose reads DO see a
    // binding on the oracle.
    for src in [
        // The clause's own writes reach its own reads.
        &b"begin\n  1\nrescue\n  q = 1\n  Float(q).w\nend\n"[..],
        // A live (falling-through) clause contributes to the post-begin
        // scope — `q` is `1 | nil` there.
        &b"begin\n  1\nrescue\n  q = 1\nend\nFloat(q).w\n"[..],
        // `die` returning normally keeps the arm live.
        &b"def die\n  nil\nend\nbegin\n  1\nrescue\n  q = 1\n  die\nend\nFloat(q).w\n"[..],
        // A redefinition replaces the binding — the LAST `def die` wins.
        &b"def die\n  raise\nend\ndef die\n  nil\nend\nbegin\n  1\nrescue\n  q = 1\n  die\nend\nFloat(q).w\n"[..],
        // A write bound at the `begin` entry is what the clause sees.
        &b"q = 0\nbegin\n  q = 1\nrescue\n  Float(q).w\nend\n"[..],
        // In-block reads of a name the same block writes bind there.
        &b"[1].each do\n  if c\n    q = 1\n  end\n  Float(q).w\nend\n"[..],
        &b"[1].each do\n  1 rescue q = 1\n  Float(q).w\nend\n"[..],
        // A `begin` main write still binds in `ensure` and post-`begin`.
        &b"begin\n  q = 1\nrescue\nensure\n  Float(q).w\nend\n"[..],
        // A `class << C` reopen after the `if` counts as a second site
        // for `(C, off?, Singleton)` — disagreeing folds decline, so the
        // arm stays live and `q`'s write binds (rigor-rs#386 review).
        &b"class C\n  def self.off? = false\nend\nif C.off?\n  q = 1\nend\nFloat(q).w\nclass << C\n  def off? = true\nend\n"[..],
    ] {
        let diags = check(src);
        assert!(
            diags.iter().any(|d| d.rule_id == CALL_UNDEFINED_METHOD),
            "expected call.undefined-method for {src:?}, got {diags:?}"
        );
    }
}

/// rigor-rs#386 — a plain write AS A PREDICATE folds to its RHS
/// (`type_of_assignment_write`): `if (q = nil)` warns always-falsey and
/// `if (q = 1)` warns always-truthy. Op-writes decline (`q ||= nil`
/// evaluates the old value on one edge, not the RHS).
#[test]
fn write_predicates_fold_to_their_rhs() {
    for (src, polarity) in [
        (&b"if (q = nil)\n  1\nend\n"[..], "falsey"),
        (&b"if (@q = nil)\n  1\nend\n"[..], "falsey"),
        (&b"if (Q = nil)\n  1\nend\n"[..], "falsey"),
        (&b"if (q = 1)\n  1\nend\n"[..], "truthy"),
    ] {
        let diags = check(src);
        assert_eq!(diags.len(), 1, "expected one warning for {src:?}: {diags:?}");
        assert!(
            diags[0].message.contains(polarity),
            "expected an always-{polarity} warning for {src:?}, got {diags:?}"
        );
    }
    let diags = check(b"q = 0\nif (q ||= nil)\n  1\nend\n");
    assert!(
        diags.is_empty(),
        "op-write predicate must not fold to the RHS: {diags:?}"
    );
}

/// rigor-rs#362 — an attr write in a position whose scope the reference
/// DISCARDS lands no `widen_attribute_write`, so the receiver's indexed
/// narrowing survives: `Narrowing.case_when_scopes` shape-narrows
/// `when`/`in` conditions without `sub_eval`; `eval_if`/`eval_unless`
/// returns the surviving edge when the only alternative arm terminates
/// and no `else` remains; `eval_case`'s `join_case_branch_scopes`
/// filters `branch_terminates?` arms even with an `else`; `eval_begin`'s
/// `live_rescue_results` drops a clause whose body exits; `and_or`'s
/// terminating right side never rejoins; and the rescue modifier's
/// exiting arm is a typed position. Every row keeps `h[:a]` = `"s" | 1`,
/// so the post read fires `call.undefined-method`.
#[test]
fn blocked_position_attr_write_keeps_indexed_narrowing() {
    // `def`s keep `c`/`d`/`y` resolvable (no unresolved-toplevel noise)
    // without making the predicates foldable constants.
    let prelude = "def c; rand(2).even?; end\ndef d; rand(2).odd?; end\nh = {a: 1}\nh[:a] ||= \"s\"\n";
    for tail in [
        // The issue rows.
        "case c; when (h.default ||= 0); end\n",
        "case c; when (h.default ||= 0), 2; end\n",
        "if c; h.default ||= 0; return; end\n",
        "case c; when 1; h.default ||= 0; return; end\n",
        "begin; rescue; h.default ||= 0; raise; end\n",
        "c && (h.default ||= 0; return)\n",
        "x = y rescue (h.default ||= 0; raise)\n",
        // Boundary rows — same discard on each sibling position.
        "unless c; h.default ||= 0; return; end\n",
        "c || (h.default ||= 0; return)\n",
        "case c; in Integer if (h.default ||= 0); end\n",
        "if c; nil; elsif d; h.default ||= 0; return; end\n",
        "if true; nil; else; h.default ||= 0; end\n",
        "case c; when 1; h.default ||= 0; return; else; nil; end\n",
        "if c; nil; else; h.default ||= 0; return; end\n",
    ] {
        let src = format!("{prelude}{tail}h[:a].frobnicate\n");
        let diags = check(src.as_bytes());
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD)
            .collect();
        assert_eq!(
            errors.len(),
            1,
            "expected the post read to keep `\"s\" | 1` for {tail:?}, got {diags:?}"
        );
        assert!(
            errors[0].message.contains("\"s\" | 1"),
            "expected the record's union in the message for {tail:?}, got {diags:?}"
        );
    }
    // The same suppression inside a deferred block — `closure_evaluated`
    // also gates on `blocked`: the in-body AND post-block reads both
    // keep the record.
    let src = b"def c; rand(2).even?; end\nh = {a: 1}\nh[:a] ||= \"s\"\n[1].each { case c; when (h.default ||= 0); end; h[:a].frobnicate }\nh[:a].frobnicate\n";
    let diags = check(src);
    let errors: Vec<_> = diags
        .iter()
        .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD)
        .collect();
    assert_eq!(
        errors.len(),
        2,
        "expected both reads to keep `\"s\" | 1`, got {diags:?}"
    );
    // Must-still-land controls — positions the reference DOES scope-
    // evaluate drop the narrowing, so no `\"s\" | 1` survives: a live
    // `if` arm (its scope joins via nil-injection), a `case` `when` arm
    // that doesn't exit, a `begin` main body, and a `while` body.
    for tail in [
        "if c; h.default ||= 0; end\n",
        "if c; h.default ||= 0; else; nil; end\n",
        "case c; when 1; h.default ||= 0; end\n",
        "begin; h.default ||= 0; rescue; end\n",
        "while c; h.default ||= 0; break; end\n",
    ] {
        let src = format!("{prelude}{tail}h[:a].frobnicate\n");
        let diags = check(src.as_bytes());
        assert!(
            !diags
                .iter()
                .any(|d| d.rule_id == CALL_UNDEFINED_METHOD
                    && d.message.contains("\"s\" | 1")),
            "expected the evaluated write to drop the record for {tail:?}, got {diags:?}"
        );
    }
}

/// rigor-rs#363 — `break`'s argument scope lands on `join_break_scopes`,
/// a LOCALS-only union: a scope effect there (an indexed-record drop or
/// a slot mutation) never propagates — not to the post-loop scope, not
/// to a later in-body read, not even to a read inside the argument
/// itself. `eval_for` joins break scopes the same way. The exception is
/// the `CONTENT_MUTATORS` family (`CONTENT_ADDERS` — `[]=`/`store`/
/// `replace`/`<<`/`push` … — plus the String-only mutators such as
/// `upcase!`/`gsub!`): the reference joins their stored arguments into
/// the continuation binding as element evidence, so a later element
/// read declines even though the record survives — the port emulates
/// that by keeping the drop. Every other row keeps `h[:a]` = `"s" | 1`.
#[test]
fn break_arg_attr_write_keeps_indexed_narrowing() {
    let prelude =
        "def c; rand(2).even?; end\ndef d; rand(2).odd?; end\nh = {a: 1}\nh[:a] ||= \"s\"\n";
    // One `"s" | 1` diagnostic each — the post-loop read.
    for tail in [
        // The issue rows: lexical while / until / for.
        "while c; break h.default ||= 0; end\n",
        "until c; break h.default ||= 0; end\n",
        "for i in [1]; break h.default ||= 0; end\n",
        // Nested under an arm still targets the loop.
        "while c; if d; break h.default ||= 0; end; end\n",
        "while c; case d; when 1; break h.default ||= 0; end; end\n",
        // A slot mutation joins the same way.
        "while c; break (h[:a] << 2); end\n",
        // Non-content-adder mutators join the same way: their drops are
        // scope effects the locals-only union never carries.
        "while c; break h.merge!({b: 2}); end\n",
        "while c; break h.update({b: 2}); end\n",
        "while c; break h.clear; end\n",
        "while c; break h.delete(:a); end\n",
        "while c; break h.delete_if { |k, _| k == :a }; end\n",
        "while c; break h.keep_if { |k, _| k == :a }; end\n",
        "while c; break h.transform_values! { |v| v }; end\n",
        // `for` runs `join_break_scopes` with no `loop_content_writeback`
        // — even the adder family keeps its record, and so does a
        // content write nested in a block inside the argument.
        "for i in [1]; break h[:a] = 9; end\n",
        "for i in [1]; break h.store(:a, 9); end\n",
        "for i in [1]; break h.replace({b: 2}); end\n",
        "for i in [1]; break ([1].each { h[:a] = 9 }); end\n",
        // `break` inside a block targets the yielding call, whose scope
        // the flat env models as dropped — but the block body's own
        // write is a closure mutation, so the record survives anyway.
        "[1].each { break h.default ||= 0 }\n",
    ] {
        let src = format!("{prelude}{tail}h[:a].frobnicate\n");
        let diags = check(src.as_bytes());
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD && d.message.contains("\"s\" | 1"))
            .collect();
        assert_eq!(
            errors.len(),
            1,
            "expected the post-loop read to keep `\"s\" | 1` for {tail:?}, got {diags:?}"
        );
    }
    // The suppression is positional, not post-loop only: a read after
    // the `break` in the same body and a read inside the argument
    // itself both keep the record — two diagnostics each.
    for tail in [
        "while c; if d; break h.default ||= 0; end; h[:a].frobnicate; end\n",
        "while c; break (h.default ||= 0; h[:a].frobnicate); end\n",
    ] {
        let src = format!("{prelude}{tail}h[:a].frobnicate\n");
        let diags = check(src.as_bytes());
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD && d.message.contains("\"s\" | 1"))
            .collect();
        assert_eq!(
            errors.len(),
            2,
            "expected both reads to keep `\"s\" | 1` for {tail:?}, got {diags:?}"
        );
    }
    // The positional edge extends to every use the iterated body still
    // contains: a scanned `[]=` inside a `break` argument drops the
    // record ONLY past the join — a sibling `break` argument, a later
    // in-body read, an `ensure` read and in-argument reads all keep it.
    // `next`'s argument DOES join the fixpoint (post-loop it drops),
    // but body/argument reads keep the record the same way. And a
    // scan-visible `END { }` operand never evaluates, so only the
    // post-loop read declines.
    for (tail, expected) in [
        ("while c; break (h[:a] = 9; h[:a].frobnicate); break (h[:a] = 8; h[:a].frobnicate); end\n", 2),
        ("while c; if d; break h[:a] = 9; end; h[:a].frobnicate; end\n", 1),
        ("while c; h[:a].frobnicate; break h[:a] = 9; end\n", 1),
        ("while c; begin; break h[:a] = 9; ensure; h[:a].frobnicate; end; end\n", 1),
        ("while c; next (h[:a] = 9; h[:a].frobnicate); end\n", 1),
        ("while c; h[:a].frobnicate; next h[:a] = 9; end\n", 1),
        ("while c; next h[:a] = 9; h[:a].frobnicate; end\n", 1),
        ("for i in [1]; next (h[:a] = 9; h[:a].frobnicate); end\n", 1),
        ("while c; END { h[:a] = 9; h[:a].frobnicate }; break; end\n", 1),
        ("while c; END { h[:a] = 9 }; h[:a].frobnicate; break; end\n", 1),
        // Controls: never-evaluated writes the scan does NOT cover keep
        // the record even post-loop — a top-level `END`, an `END`
        // inside a `for` (no writeback), and a compound attribute write
        // (not a CONTENT_MUTATORS call).
        ("END { h[:a] = 9 }\n", 1),
        ("for i in [1]; END { h[:a] = 9 }; end\n", 1),
        ("while c; END { h.default ||= 9 }; break; end\n", 1),
    ] {
        let src = format!("{prelude}{tail}h[:a].frobnicate\n");
        let diags = check(src.as_bytes());
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| d.rule_id == CALL_UNDEFINED_METHOD && d.message.contains("\"s\" | 1"))
            .collect();
        assert_eq!(
            errors.len(),
            expected,
            "expected {expected} `\"s\" | 1` diagnostic(s) for {tail:?}, got {diags:?}"
        );
    }
    // Must-still-land controls — effects the reference DOES join still
    // drop or shadow the record: a `next` argument feeds the loop
    // fixpoint carrying the drop; the CONTENT_ADDERS family joins stored
    // arguments as element evidence, so the read declines (`[]=`/`store`/
    // `replace` — the port emulates by keeping the drop); a rebind
    // rewrites the binding; and a body write lands on the joined
    // iteration scope.
    for tail in [
        "while c; next h.default ||= 0; end\n",
        "while c; break h[:a] = 9; end\n",
        "while c; break (h[:a] ||= 9); end\n",
        "while c; break h.store(:a, 9); end\n",
        "while c; break h.replace({b: 2}); end\n",
        "while c; break (h = {}); end\n",
        "while c; h.default ||= 0; break; end\n",
        // String-only mutators are `CONTENT_MUTATORS` too — the writeback
        // scan covers them.
        "while c; break h.upcase!; end\n",
        "while c; break h.gsub!(\"x\", \"y\"); end\n",
        "while c; break h.force_encoding(\"utf-8\"); end\n",
        // The scan is textual over the whole iterated body: a content
        // mutation in a dead `rescue` clause, a folded `if` arm or a
        // nested block still lands on the post-body scope.
        "while c; begin; raise; rescue; break h[:a] = 9; end; end\n",
        "while c; begin; raise; rescue; h[:a] = 9; break; end; end\n",
        "while c; if false; h[:a] = 9; end; end\n",
        "while c; if nil; h[:a] = 9; end; end\n",
        "while c; if d; h[:a] = 9; return; end; end\n",
        "while c; break ([1].each { h[:a] = 9 }); end\n",
        "while c; break ([1].each { if d; h[:a] = 9; return; end }); end\n",
        // A `for` nested under a `while` is still writeback-scanned
        // (the scan is textual over the whole body), and a `while` under
        // a `for` shadows via its own writeback — `[]=` declines the
        // read in both.
        "while c; for i in [1]; break h[:a] = 9; end; end\n",
        "for i in [1]; while c; break h[:a] = 9; end; end\n",
        // A scan-visible mutation inside a never-EVALUATED operand
        // still lands past the body's join — `END { }` (and `BEGIN`,
        // `super`, `yield`) bodies are textually read by
        // `loop_content_writeback` / `content_writeback_block_captures`
        // even under a `break` argument.
        "while c; END { h[:a] = 9 }; break; end\n",
        "until c; END { h[:a] = 9 }; end\n",
        "loop do; END { h[:a] = 9 }; break; end\n",
        "[1].each { END { h[:a] = 9 } }\n",
        "while c; END { if false; h[:a] = 9; end }; break; end\n",
    ] {
        let src = format!("{prelude}{tail}h[:a].frobnicate\n");
        let diags = check(src.as_bytes());
        assert!(
            !diags
                .iter()
                .any(|d| d.rule_id == CALL_UNDEFINED_METHOD
                    && d.message.contains("\"s\" | 1")),
            "expected the joined write to drop the record for {tail:?}, got {diags:?}"
        );
    }
}
