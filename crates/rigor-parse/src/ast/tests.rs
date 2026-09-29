use super::*;

/// Upstream #540's census, rendered as `prefix|receiver|path?|method`.
fn census(src: &str) -> Vec<String> {
    let result = crate::parse(src.as_bytes());
    lower(&result)
        .const_mutations()
        .iter()
        .map(|m| {
            format!(
                "{}|{}|{}|{}",
                m.prefix.join("::"),
                m.receiver,
                m.receiver_is_path,
                m.method.clone().unwrap_or_else(|| "*".into())
            )
        })
        .collect()
}

#[test]
fn const_mutation_census_records_the_three_mutating_shapes() {
    // The `Index{Or,And,Operator}Write` family has no owned arena variant —
    // it is the whole reason the census lives in the lowering.
    assert_eq!(census("C[0] ||= 1\n"), ["|C|false|*"]);
    assert_eq!(census("C[0] &&= 1\n"), ["|C|false|*"]);
    assert_eq!(census("C[0] += 1\n"), ["|C|false|*"]);
    // Prism's `attribute_write?` — the method name is irrelevant.
    assert_eq!(census("C[0] = 1\n"), ["|C|false|*"]);
    assert_eq!(census("C.x = 1\n"), ["|C|false|*"]);
    // A plain send carries its NAME; `rigor-infer` applies the mutator tables.
    assert_eq!(census("C.push(1)\n"), ["|C|false|push"]);
    assert_eq!(census("C.each { |x| x }\n"), ["|C|false|each"]);
    // A receiver that is not a constant, or not statically nameable, is out.
    assert!(census("x[0] = 1\n").is_empty());
    assert!(census("@x[0] = 1\n").is_empty());
    assert!(census("expr::Bar[0] = 1\n").is_empty());
    assert!(census("C\n").is_empty());
}

#[test]
fn const_mutation_census_tracks_only_the_lexical_prefix() {
    // Scope-INSENSITIVE below the class/module level: a method body, a block
    // and the top level all record the same prefix.
    assert_eq!(
        census("module A\n  module B\n    def m\n      [1].each { C[0] = 1 }\n    end\n  end\nend\n"),
        ["A::B|C|false|*"]
    );
    // A `class A::B` header contributes ONE rendered segment.
    assert_eq!(census("class A::B\n  C[0] = 1\nend\n"), ["A::B|C|false|*"]);
    // A PATH receiver keeps the name as written and is flagged as a path.
    assert_eq!(census("Outer::T[0] = 1\n"), ["|Outer::T|true|*"]);
    assert_eq!(census("::Outer::T[0] = 1\n"), ["|Outer::T|true|*"]);
    // A named class is entered through its BODY only, so a mutation in the
    // SUPERCLASS expression is not recorded (the reference `return`s there).
    assert!(census("class K < D[C.push(1)]\nend\n").is_empty());
    // A class HEADER is rendered LENIENTLY, exactly as the reference's
    // `Source::ConstantPath.qualified_name` does: a dynamic base is dropped
    // and the trailing name still opens a prefix. (Only the mutated
    // RECEIVER uses the strict `qualified_name_or_nil` policy — see the
    // `expr::Bar[0] = 1` case above.)
    assert_eq!(census("class expr::K\n  C[0] = 1\nend\n"), ["K|C|false|*"]);
}

#[test]
fn lowers_assignment_and_call_with_precise_spans() {
    let src = b"s = \"Hello\"\ns.lenght\n";
    let result = crate::parse(src);
    let ast = lower(&result);

    // Find the single Call node and assert its method + message span maps
    // back to `lenght` in the source.
    let call = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::Call { method, message_span, .. } => {
                Some((method.clone(), *message_span))
            }
            _ => None,
        })
        .expect("expected a Call node");
    assert_eq!(call.0, "lenght");
    let (start, end) = call.1;
    assert_eq!(&src[start..end], b"lenght");
}

/// The first `MultiWrite`'s target tree in `src`.
fn multi_targets(src: &[u8]) -> MultiTargets {
    let result = crate::parse(src);
    let ast = lower(&result);
    ast.iter()
        .find_map(|(_, n)| match n {
            Node::MultiWrite { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .expect("expected a MultiWrite node")
}

#[test]
fn lowers_multi_write_targets_and_rhs() {
    let src = b"a, b = foo\n";
    let result = crate::parse(src);
    let ast = lower(&result);
    let (targets, value) = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::MultiWrite { targets, value, .. } => Some((targets.clone(), *value)),
            _ => None,
        })
        .expect("expected a MultiWrite node");
    let names: Vec<String> = targets.bound_names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["a", "b"]);
    // The RHS is a fully lowered child, so calls inside it stay reachable.
    assert!(matches!(ast.get(value), Node::Call { method, .. } if method == "foo"));
    // Name spans point at the target tokens.
    let (_, span_a) = targets.bound_names()[0].clone();
    assert_eq!(&src[span_a.0..span_a.1], b"a");
}

#[test]
fn lowers_nested_splat_and_ignorable_multi_targets() {
    // `a, (b, c), *r, @d = xs` — nested group, splat, and an ivar target
    // that binds no local but must hold its position.
    let t = multi_targets(b"a, (b, c), *r, @d = xs\n");
    let names: Vec<String> = t.bound_names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["a", "b", "c", "r"]);
    assert!(matches!(t.lefts[1], MultiTarget::Nested(_)), "the `(b, c)` group");
    assert!(matches!(t.rest.as_deref(), Some(MultiTarget::Local { .. })), "the `*r` splat");
    assert!(matches!(t.rights[0], MultiTarget::Ignored { .. }), "the `@d` ivar target");
}

#[test]
fn lowers_expressions_embedded_in_ignorable_multi_targets() {
    // `item[3], item[5] = info` binds no local, but READS `item` twice —
    // those reads must reach the arena or `flow.dead-assignment` falsely
    // flags an earlier `item = …` (netrc 0.11.0 `Netrc#[]=`).
    let src = b"item = fetch\nitem[3], item[5] = info\n";
    let result = crate::parse(src);
    let ast = lower(&result);
    let reads = ast
        .iter()
        .filter(|(_, n)| matches!(n, Node::LocalVariableRead { name, .. } if name == "item"))
        .count();
    assert_eq!(reads, 2, "both `item` receivers must be lowered");
    let exprs = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::MultiWrite { target_exprs, .. } => Some(target_exprs.len()),
            _ => None,
        })
        .expect("expected a MultiWrite node");
    assert_eq!(exprs, 2);
}

#[test]
fn lowers_anonymous_and_implicit_rest_as_present_but_ignored() {
    let t = multi_targets(b"a, *, z = xs\n");
    assert!(t.bound_names().iter().all(|(n, _)| n != "*"));
    assert!(matches!(t.rest.as_deref(), Some(MultiTarget::Ignored { .. })));
    // `a, = xs` — Prism records an ImplicitRestNode; presence still matters.
    let t = multi_targets(b"a, = xs\n");
    assert!(t.rest.is_some(), "an implicit rest is a PRESENT rest slot");
    assert!(matches!(t.rest.as_deref(), Some(MultiTarget::Ignored { .. })));
}

#[test]
fn lowers_local_write_and_string_literal() {
    let src = b"s = \"Hello\"\n";
    let result = crate::parse(src);
    let ast = lower(&result);

    let has_write = ast.iter().any(|(_, n)| {
        matches!(n, Node::LocalVariableWrite { name, .. } if name == "s")
    });
    let has_str = ast.iter().any(|(_, n)| {
        matches!(n, Node::StringLit { value, .. } if value == "Hello")
    });
    assert!(has_write, "expected a LocalVariableWrite for `s`");
    assert!(has_str, "expected a StringLit \"Hello\"");
}

#[test]
fn lowers_operator_and_or_writes_to_op_write_variant() {
    // `x += 1`, `y ||= 2`, `z &&= w` all lower to LocalVariableOpWrite with
    // their target name preserved (so the dead-assignment walk sees the
    // implicit read). Each must lower its assigned value for reachability.
    for (src, name) in [
        (&b"x = 0\nx += 1\n"[..], "x"),
        (&b"y = 0\ny ||= 2\n"[..], "y"),
        (&b"z = 0\nz &&= 3\n"[..], "z"),
    ] {
        let ast = lower(&crate::parse(src));
        let found = ast.iter().any(|(_, n)| {
            matches!(n, Node::LocalVariableOpWrite { name: nm, .. } if nm == name)
        });
        assert!(found, "expected LocalVariableOpWrite for `{name}` in {src:?}");
    }
}

#[test]
fn lowers_index_compound_writes_to_index_write_variant() {
    // `h[:a] ||= 1`, `h[:a] &&= 1`, `h[:a] += 1` all lower to `IndexWrite`
    // with the receiver, the index arguments and the stored value as
    // children — the `[]=` store the reference's `IndexWriteWidening` widens
    // on (`index_write_widening.rb`, upstream #560). NOT a `Call`, so the
    // synthesized `[]`/`[]=` never reach the `call.*` rules.
    for src in [
        &b"h[:a] ||= 1\n"[..],
        &b"h[:a] &&= 1\n"[..],
        &b"h[:a] += 1\n"[..],
        &b"h[:a, :b] -= 1\n"[..],
    ] {
        let ast = lower(&crate::parse(src));
        let found = ast.iter().any(|(_, n)| {
            matches!(n, Node::IndexWrite { receiver: Some(_), indices, .. } if !indices.is_empty())
        });
        assert!(found, "expected IndexWrite in {src:?}");
    }
    // The operands are fully lowered children — a call in the value stays
    // reachable exactly as it did under the recovered carrier.
    let ast = lower(&crate::parse(b"h[:a] ||= foo\n"));
    assert!(ast.iter().any(|(_, n)| {
        matches!(n, Node::Call { method, .. } if method == "foo")
    }));
    // A plain `h[:a] = 1` stays a `[]=` `Call`.
    let ast = lower(&crate::parse(b"h[:a] = 1\n"));
    assert!(ast.iter().any(|(_, n)| {
        matches!(n, Node::Call { method, .. } if method == "[]=")
    }));
    assert!(!ast.iter().any(|(_, n)| matches!(n, Node::IndexWrite { .. })));
}

#[test]
fn reads_local_within_finds_reads_inside_a_span_only() {
    let src = b"s = 1\n\"abc\"[s]\n\"abc\"[0]\n";
    let ast = lower(&crate::parse(src));
    let span_of = |needle: &[u8]| {
        let lo = src.windows(needle.len()).position(|w| w == needle).unwrap();
        (lo, lo + needle.len())
    };
    assert!(ast.reads_local_within(span_of(b"\"abc\"[s]")));
    assert!(!ast.reads_local_within(span_of(b"\"abc\"[0]")));
    assert!(!ast.reads_local_within(span_of(b"s = 1")));
}

/// rigor-rs#151: a `for` index carries the local names it binds, each
/// keyed inside the loop's span; a non-local index binds nothing.
#[test]
fn for_index_names_are_carried_on_the_loop() {
    let src = b"for w in xs; end\nfor a, (b, *c) in xs; end\nfor @i in xs; end\nfor A in xs; end\nwhile x; end\n";
    let ast = lower(&crate::parse(src));
    let loops: Vec<Vec<String>> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Loop { index, span, .. } => {
                assert!(index.iter().all(|(_, s)| span.0 <= s.0 && s.1 <= span.1));
                Some(index.iter().map(|(n, _)| n.clone()).collect())
            }
            _ => None,
        })
        .collect();
    let expect: Vec<Vec<String>> = vec![
        vec!["w".into()],
        vec!["a".into(), "b".into(), "c".into()],
        vec![],
        vec![],
        vec![],
    ];
    assert_eq!(loops, expect);
}

/// rigor-rs#134: an `h[k]` multi-assign target stores through `[]=` on `h` —
/// it binds no local, keeps its position in the tuple decomposition, and
/// reports its receiver locals for widening.
#[test]
fn multi_write_index_targets_report_their_receiver_writes() {
    // `h` must be a LOCAL for the receiver read — `h = {}` first, else Prism
    // parses the bare `h` as a method call and it names no local.
    let t = multi_targets(b"h = {}\nh[:a], z = 1, 2\n");
    let names: Vec<String> = t.bound_names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["z"], "the index target binds no local");
    assert!(matches!(t.lefts[0], MultiTarget::Index { .. }));
    let writes: Vec<String> = t.index_writes().into_iter().map(|(n, _)| n).collect();
    assert_eq!(writes, ["h"]);
}

#[test]
fn nested_and_splatted_index_targets_keep_their_writes() {
    let t = multi_targets(b"h = {}\ns = []\n(h[:a], q), *s[0] = xs\n");
    let names: Vec<String> = t.bound_names().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["q"]);
    let writes: Vec<String> = t.index_writes().into_iter().map(|(n, _)| n).collect();
    assert_eq!(writes, ["h", "s"], "nested and splatted receivers, in source order");
}

#[test]
fn index_target_receivers_walk_branching_expressions() {
    // `(c ? a : b)[:k]` mutates whichever local the ternary selects — the
    // local half of the reference's `ReceiverAlias.mutated_reads`.
    let t = multi_targets(b"a = {}\nb = {}\n(c ? a : b)[:k], z = 1, 2\n");
    let writes: Vec<String> = t.index_writes().into_iter().map(|(n, _)| n).collect();
    assert_eq!(writes, ["a", "b"]);
    // An ivar receiver names no local — a strict decline, still `Index`.
    let t = multi_targets(b"@h[:k], z = 1, 2\n");
    assert!(t.index_writes().is_empty());
    assert!(matches!(t.lefts[0], MultiTarget::Index { .. }));
}

/// rigor-rs#134: a `for` index target's `[]=` store rides the loop's
/// `index_writes` — the whole index (`for h[:a] in xs`), a multi-target slot
/// (`for w, p[:k] in ys`) and a bare splat index (`for *s[0] in zs`).
#[test]
fn for_index_targets_report_their_receiver_writes() {
    let src = b"h = {}\np = {}\ns = []\nfor h[:a] in xs; end\nfor w, p[:k] in ys; end\nfor *s[0] in zs; end\nfor q in qs; end\n";
    let ast = lower(&crate::parse(src));
    let loops: Vec<(Vec<String>, Vec<String>)> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Loop { index, index_writes, span, .. } => {
                assert!(index_writes.iter().all(|(_, s)| span.0 <= s.0 && s.1 <= span.1));
                Some((
                    index.iter().map(|(n, _)| n.clone()).collect(),
                    index_writes.iter().map(|(n, _)| n.clone()).collect(),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        loops,
        vec![
            (vec![], vec!["h".into()]),
            (vec!["w".into()], vec!["p".into()]),
            (vec![], vec!["s".into()]),
            (vec!["q".into()], vec![]),
        ]
    );
}

/// rigor-rs#134: `rescue => h[:e]` stores the exception through `[]=` on `h`;
/// a local `rescue => e` binds a name and reports no index write.
#[test]
fn rescue_index_reference_reports_its_receiver_writes() {
    let src = b"h = {}\nbegin; foo; rescue => h[:e]; end\nbegin; foo; rescue => e; end\n";
    let ast = lower(&crate::parse(src));
    let seen: Vec<(Option<String>, Vec<String>)> = ast
        .iter()
        .flat_map(|(_, n)| match n {
            Node::BeginRescue { clauses, .. } => clauses.clone(),
            _ => Vec::new(),
        })
        .map(|c| {
            (
                c.bound_name.clone(),
                c.index_writes.iter().map(|(n, _)| n.clone()).collect(),
            )
        })
        .collect();
    assert_eq!(seen, vec![(None, vec!["h".into()]), (Some("e".into()), vec![])]);
}

/// rigor-rs#153: the carrier kinds. A real statement list is a sequence;
/// `defined?`, `END`, `BEGIN`, `super` and `yield` are inert; any other
/// recovery (a `rescue` modifier) is `Recovered`. Every write stays in the
/// arena for the structural walks.
#[test]
fn statements_carriers_record_their_kind() {
    let kinds = |src: &[u8]| -> Vec<StatementsKind> {
        let ast = lower(&crate::parse(src));
        ast.iter()
            .filter_map(|(_, n)| match n {
                Node::Statements { kind, .. } => Some(*kind),
                _ => None,
            })
            .collect()
    };
    use StatementsKind::*;
    assert_eq!(kinds(b"defined?(w = 1)\n"), [Inert]);
    assert_eq!(kinds(b"END { w = 1 }\n"), [Inert]);
    assert_eq!(kinds(b"BEGIN { w = 1 }\n"), [Inert]);
    assert_eq!(kinds(b"super(w = 1)\n"), [Inert]);
    assert_eq!(kinds(b"def m\n  yield(w = 1)\nend\n"), [Inert]);
    assert_eq!(kinds(b"(w = 1) rescue nil\n"), [Recovered]);
    assert_eq!(kinds(b"\"#{w = 1}\"\n"), [Sequence]);
    let ast = lower(&crate::parse(b"x = 1\ndefined?(w = 1)\n"));
    let writes: Vec<(Span, bool)> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::LocalVariableWrite { span, .. } => Some((*span, ast.in_inert_carrier(*span))),
            _ => None,
        })
        .collect();
    assert_eq!(writes.iter().map(|w| w.1).collect::<Vec<_>>(), [false, true]);
}

#[test]
fn integer_literals_lower_across_i64_and_preserve_bignum_digits() {
    // Beyond `i32` used to lower to `0`; beyond `i64` `value` stays `None`
    // (never a wrong pin) while `digits` keeps the exact decimal spelling for
    // `Scalar::BigInt` (rigor-rs#194) — including a negative Bignum's sign.
    let src = b"[1, -2, 3_000_000_000, 0x7fff_ffff_ffff_ffff, \
                -9223372036854775808, 9223372036854775808, 100000000000000000000, \
                -9223372036854775809]\n";
    let ast = lower(&crate::parse(src));
    let lits: Vec<(Option<i64>, Option<String>)> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::IntegerLit { value, digits, .. } => Some((*value, digits.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        lits,
        [
            (Some(1), None),
            (Some(-2), None),
            (Some(3_000_000_000), None),
            (Some(i64::MAX), None),
            (Some(i64::MIN), None),
            (None, Some("9223372036854775808".to_string())),
            (None, Some("100000000000000000000".to_string())),
            (None, Some("-9223372036854775809".to_string())),
        ]
    );
}

#[test]
fn local_write_records_name_span() {
    // The name_span anchors on the NAME token only (`result`), not the whole
    // `result = 1` — mirroring the reference's `from_name_loc`.
    let src = b"result = 1\n";
    let ast = lower(&crate::parse(src));
    let name_span = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::LocalVariableWrite { name, name_span, .. } if name == "result" => {
                Some(*name_span)
            }
            _ => None,
        })
        .expect("expected a LocalVariableWrite for `result`");
    assert_eq!(&src[name_span.0..name_span.1], b"result");
}

#[test]
fn lowers_interpolated_string_to_node() {
    // `"a#{x}b"` lowers to an InterpolatedString whose parts are non-empty
    // (the `#{x}` segment is lowered, keeping its calls reachable).
    let src = b"\"a#{x}b\"\n";
    let result = crate::parse(src);
    let ast = lower(&result);

    let parts = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::InterpolatedString { parts, .. } => Some(parts.clone()),
            _ => None,
        })
        .expect("expected an InterpolatedString node");
    assert!(!parts.is_empty(), "expected non-empty interpolation parts");
}

#[test]
fn lowers_call_positional_arguments() {
    // `s.include?("e", "x")` lowers two positional string-literal args, in
    // source order, as children of the Call node.
    let src = b"s = \"Hello\"\ns.include?(\"e\", \"x\")\n";
    let result = crate::parse(src);
    let ast = lower(&result);

    let args = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::Call { method, args, .. } if method == "include?" => Some(args.clone()),
            _ => None,
        })
        .expect("expected an include? Call node");
    assert_eq!(args.len(), 2, "expected two positional args");
    let vals: Vec<String> = args
        .iter()
        .map(|id| match ast.get(*id) {
            Node::StringLit { value, .. } => value.clone(),
            other => panic!("expected StringLit arg, got {other:?}"),
        })
        .collect();
    assert_eq!(vals, vec!["e".to_string(), "x".to_string()]);
}

#[test]
fn lowers_nil_true_false_symbol_float_literals() {
    let src = b"nil\ntrue\nfalse\n:foo\n3.5\n";
    let result = crate::parse(src);
    let ast = lower(&result);
    assert!(ast.iter().any(|(_, n)| matches!(n, Node::NilLit { .. })));
    assert!(ast.iter().any(|(_, n)| matches!(n, Node::TrueLit { .. })));
    assert!(ast.iter().any(|(_, n)| matches!(n, Node::FalseLit { .. })));
    assert!(ast
        .iter()
        .any(|(_, n)| matches!(n, Node::SymbolLit { value, .. } if value == "foo")));
    assert!(ast
        .iter()
        .any(|(_, n)| matches!(n, Node::FloatLit { value, .. } if (*value - 3.5).abs() < f64::EPSILON)));
}

#[test]
fn lowering_is_total_for_unhandled_nodes() {
    // A construct still outside the owned subset (a `yield`) must lower
    // without panicking, landing in `Other`.
    let src = b"def foo; yield; end\n";
    let result = crate::parse(src);
    let ast = lower(&result);
    assert!(!ast.is_empty());
    assert!(ast.iter().any(|(_, n)| matches!(n, Node::Other { .. })));
}

/// True iff the arena contains a `Call` to `method`.
fn has_call(ast: &LoweredAst, method: &str) -> bool {
    ast.iter()
        .any(|(_, n)| matches!(n, Node::Call { method: m, .. } if m == method))
}

#[test]
fn lowers_call_inside_method_def() {
    // A call in a `def` body must reach the arena (the whole point).
    let src = b"def slug(t)\n  t.downcase\nend\n";
    let ast = lower(&crate::parse(src));
    assert!(
        ast.iter().any(|(_, n)| matches!(n, Node::Definition { .. })),
        "expected a Definition node for the def"
    );
    assert!(has_call(&ast, "downcase"), "call inside def must be lowered");
}

#[test]
fn lowers_parameter_default_value_calls() {
    // C2: a call inside a POSITIONAL or KEYWORD parameter default must reach
    // the arena so the call rules can witness a typo on a literal/constant
    // receiver there (the reference checks parameter defaults).
    let src = b"def f(t = Time.current, a: Foo.bar)\n  t\nend\n";
    let ast = lower(&crate::parse(src));
    assert!(has_call(&ast, "current"), "positional default call must be lowered");
    assert!(has_call(&ast, "bar"), "keyword default call must be lowered");
}

#[test]
fn lowers_calls_inside_if_and_else_branches() {
    let src = b"if x\n  a.foo\nelse\n  b.bar\nend\n";
    let ast = lower(&crate::parse(src));
    assert!(ast.iter().any(|(_, n)| matches!(n, Node::If { .. })));
    assert!(has_call(&ast, "foo"), "then-branch call must be lowered");
    assert!(has_call(&ast, "bar"), "else-branch call must be lowered");
}

#[test]
fn lowers_if_and_unless_keyword_distinctly() {
    // The `is_unless` flag must survive lowering: Prism keeps `IfNode` and
    // `UnlessNode` distinct, and `flow.unreachable-branch` relies on the
    // keyword to decide which branch a literal predicate kills. Both keywords
    // must also preserve BOTH branches (then + else).
    let if_ast = lower(&crate::parse(b"if x\n  a.foo\nelse\n  b.bar\nend\n"));
    let (_, if_node) = if_ast
        .iter()
        .find(|(_, n)| matches!(n, Node::If { .. }))
        .expect("if must lower to a Node::If");
    match if_node {
        Node::If { is_unless, then_body, else_body, .. } => {
            assert!(!is_unless, "`if` must lower with is_unless == false");
            assert!(!then_body.is_empty(), "then branch preserved");
            assert!(!else_body.is_empty(), "else branch preserved");
        }
        _ => unreachable!(),
    }

    let unless_ast = lower(&crate::parse(b"unless x\n  a.foo\nelse\n  b.bar\nend\n"));
    let (_, unless_node) = unless_ast
        .iter()
        .find(|(_, n)| matches!(n, Node::If { .. }))
        .expect("unless must lower to a Node::If");
    match unless_node {
        Node::If { is_unless, then_body, else_body, .. } => {
            assert!(is_unless, "`unless` must lower with is_unless == true");
            assert!(!then_body.is_empty(), "then (unless body) preserved");
            assert!(!else_body.is_empty(), "else branch preserved");
        }
        _ => unreachable!(),
    }
}

#[test]
fn lowers_calls_inside_case_when_branches() {
    let src = b"case v\nwhen 1\n  a.foo\nwhen 2\n  b.bar\nelse\n  c.baz\nend\n";
    let ast = lower(&crate::parse(src));
    assert!(ast.iter().any(|(_, n)| matches!(n, Node::Case { .. })));
    assert!(has_call(&ast, "foo"));
    assert!(has_call(&ast, "bar"));
    assert!(has_call(&ast, "baz"));
}

/// A `when` clause lowers to the dedicated `When` variant with its
/// conditions and body in SEPARATE lists (multi-condition clauses keep
/// every condition), and the `Case`'s branches are those `When` nodes.
#[test]
fn lowers_when_clause_with_split_conditions_and_body() {
    let src = b"case v\nwhen 1, 2\n  a.foo\nwhen 3\nelse\n  c.baz\nend\n";
    let ast = lower(&crate::parse(src));
    let branches = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::Case { branches, .. } => Some(branches.clone()),
            _ => None,
        })
        .expect("case present");
    assert_eq!(branches.len(), 2);
    match ast.get(branches[0]) {
        Node::When { conditions, body, .. } => {
            assert_eq!(conditions.len(), 2, "both conditions kept");
            assert_eq!(body.len(), 1, "body separate from conditions");
        }
        other => panic!("first branch must be a When, got {other:?}"),
    }
    // An empty-bodied `when` keeps its condition and an empty body.
    match ast.get(branches[1]) {
        Node::When { conditions, body, .. } => {
            assert_eq!(conditions.len(), 1);
            assert!(body.is_empty());
        }
        other => panic!("second branch must be a When, got {other:?}"),
    }
    // A `case`/`in` pattern branch still uses the BeginRescue carrier.
    let pm = lower(&crate::parse(b"case v\nin [x]\n  a.foo\nend\n"));
    let pm_branches = pm
        .iter()
        .find_map(|(_, n)| match n {
            Node::Case { branches, .. } => Some(branches.clone()),
            _ => None,
        })
        .expect("case/in present");
    assert!(matches!(pm.get(pm_branches[0]), Node::BeginRescue { .. }));
}

#[test]
fn lowers_calls_inside_loops_and_begin_rescue() {
    let w = lower(&crate::parse(b"while x\n  a.foo\nend\n"));
    assert!(w.iter().any(|(_, n)| matches!(n, Node::Loop { .. })));
    assert!(has_call(&w, "foo"));

    let b = lower(&crate::parse(b"begin\n  a.foo\nrescue => e\n  b.bar\nensure\n  c.baz\nend\n"));
    assert!(b.iter().any(|(_, n)| matches!(n, Node::BeginRescue { .. })));
    assert!(has_call(&b, "foo"));
    assert!(has_call(&b, "bar"));
    assert!(has_call(&b, "baz"));
}

#[test]
fn defined_operand_drops_calls_but_keeps_local_reads() {
    // Upstream #318 (pin v0.3.4): `defined?`'s operand is never evaluated, so
    // no call under it is reachable code — but the reference's
    // `DeadAssignmentCollector` still counts the local reads there, so those
    // must survive.
    let ast = lower(&crate::parse(b"defined?(s.frobnicate)\n"));
    assert!(!has_call(&ast, "frobnicate"), "a call under defined? is not live code");

    let ast = lower(&crate::parse(b"defined?(helper_method)\n"));
    assert!(
        !has_call(&ast, "helper_method"),
        "an implicit-self call under defined? is not live code either"
    );

    let ast = lower(&crate::parse(b"y = 1\ndefined?(y)\n"));
    assert!(
        ast.iter()
            .any(|(_, n)| matches!(n, Node::LocalVariableRead { name, .. } if name == "y")),
        "a local read under defined? is still a read"
    );

    // Through a suppressed call: `defined?(foo(z))` still reads `z`.
    let ast = lower(&crate::parse(b"z = 1\ndefined?(foo(z))\n"));
    assert!(!has_call(&ast, "foo"), "the call itself stays out");
    assert!(
        ast.iter()
            .any(|(_, n)| matches!(n, Node::LocalVariableRead { name, .. } if name == "z")),
        "a read inside a suppressed call is still a read"
    );

    // The parenthesised guard leaves the second call OUTSIDE the operand.
    let ast = lower(&crate::parse(b"defined?(x) && \"lit\".frobparened\n"));
    assert!(
        has_call(&ast, "frobparened"),
        "a call outside the operand is live code"
    );

    // A `defined?` buried in an unhandled wrapper is recovered whole, so the
    // suppression still applies to its operand.
    let ast = lower(&crate::parse(b"def m\n  super(defined?(q.frobwrapped))\nend\n"));
    assert!(
        !has_call(&ast, "frobwrapped"),
        "a wrapped defined? operand is suppressed too"
    );
}

#[test]
fn lowers_call_inside_block_body() {
    // `[1,2].each { |n| n.foo }` — the block's inner call must be lowered.
    let src = b"[1, 2].each { |n| n.foo }\n";
    let ast = lower(&crate::parse(src));
    assert!(has_call(&ast, "foo"), "block-body call must be lowered");
    // The outer `each` call carries the block body ids.
    let has_block = ast.iter().any(|(_, n)| {
        matches!(n, Node::Call { method, block_body, .. } if method == "each" && !block_body.is_empty())
    });
    assert!(has_block, "the each call should record its block body");
}

#[test]
fn safe_nav_flag_distinguishes_dot_from_amp_dot() {
    // `x&.foo` lowers with safe_nav: true; `x.foo` with safe_nav: false.
    let safe = lower(&crate::parse(b"x&.foo\n"));
    let safe_flag = safe.iter().find_map(|(_, n)| match n {
        Node::Call { method, safe_nav, .. } if method == "foo" => Some(*safe_nav),
        _ => None,
    });
    assert_eq!(safe_flag, Some(true), "x&.foo must lower safe_nav: true");

    let plain = lower(&crate::parse(b"x.foo\n"));
    let plain_flag = plain.iter().find_map(|(_, n)| match n {
        Node::Call { method, safe_nav, .. } if method == "foo" => Some(*safe_nav),
        _ => None,
    });
    assert_eq!(plain_flag, Some(false), "x.foo must lower safe_nav: false");
}

#[test]
fn lowers_array_and_hash_literals() {
    let a = lower(&crate::parse(b"[1, 2, 3]\n"));
    assert!(a.iter().any(|(_, n)| matches!(n, Node::ArrayLit { .. })));
    let h = lower(&crate::parse(b"{ a: 1, b: 2 }\n"));
    assert!(h.iter().any(|(_, n)| matches!(n, Node::HashLit { .. })));
}

#[test]
fn lowers_call_inside_keyword_hash_value() {
    // Bare keyword args wrap a KeywordHashNode; the value call must be lowered.
    let src = b"foo(wait: 30.minutes)\n";
    let ast = lower(&crate::parse(src));
    assert!(
        has_call(&ast, "minutes"),
        "keyword-hash value call must be lowered"
    );
}

#[test]
fn lowers_calls_inside_parenthesized_range_bounds() {
    // `(30.seconds)..(10.minutes)` — both parenthesized bounds must be reachable.
    let src = b"x = (30.seconds)..(10.minutes)\n";
    let ast = lower(&crate::parse(src));
    assert!(has_call(&ast, "seconds"), "parenthesized left-bound call must be lowered");
    assert!(has_call(&ast, "minutes"), "parenthesized right-bound call must be lowered");
}

#[test]
fn lowers_logical_operands() {
    // Both sides of `&&` must be reachable.
    let src = b"a.foo && b.bar\n";
    let ast = lower(&crate::parse(src));
    assert!(ast.iter().any(|(_, n)| matches!(n, Node::Logical { .. })));
    assert!(has_call(&ast, "foo"));
    assert!(has_call(&ast, "bar"));
}

#[test]
fn variable_reads_and_writes_carry_their_sigilled_name() {
    // The #521 untyped-argument gate keys an ivar/cvar/gvar root on this
    // spelling, and it is the ONLY discriminator between the three kinds
    // (they share one nameless-until-now node variant). Prism's `name` is
    // already sigilled (`:@x`, `:@@x`, `:$x`); pin that it stays that way.
    let ast = lower(&crate::parse(b"@@c = 1\n$g = 2\n@i.foo\n@@c.bar\n$g.baz\n"));
    let names: Vec<&str> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::VariableRead { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["@i", "@@c", "$g"]);
    let writes: Vec<&str> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::VariableWrite { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(writes, vec!["@@c", "$g"]);
}

#[test]
fn lowers_ivar_and_constant_writes_recursively() {
    // The assigned value's call must be lowered.
    let iv = lower(&crate::parse(b"@x = a.foo\n"));
    assert!(iv.iter().any(
        |(_, n)| matches!(n, Node::InstanceVariableWrite { name, .. } if name == "@x")
    ));
    assert!(has_call(&iv, "foo"));
    let cw = lower(&crate::parse(b"FOO = a.bar\n"));
    assert!(cw.iter().any(|(_, n)| matches!(n, Node::ConstantWrite { .. })));
    assert!(has_call(&cw, "bar"));
}

#[test]
fn lowers_call_inside_string_interpolation() {
    let src = b"x = \"hi #{a.foo}\"\n";
    let ast = lower(&crate::parse(src));
    assert!(has_call(&ast, "foo"), "interpolated call must be lowered");
}

/// Locate the single `ClassDef` and return its (name, superclass, methods).
fn class_def(ast: &LoweredAst) -> (String, Option<String>, Vec<String>) {
    ast.iter()
        .find_map(|(_, n)| match n {
            Node::ClassDef { name, superclass, methods, .. } => {
                Some((name.clone(), superclass.clone(), methods.clone()))
            }
            _ => None,
        })
        .expect("expected a ClassDef node")
}

#[test]
fn lowers_class_def_name_super_and_methods() {
    // `class Point; def x; end; def y; end; end` — name "Point", no super,
    // instance methods [x, y]. The body's calls still reach the arena.
    let src = b"class Point\n  def x\n    1\n  end\n  def y\n    @a.foo\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let (name, sup, methods) = class_def(&ast);
    assert_eq!(name, "Point");
    assert_eq!(sup, None);
    assert_eq!(methods, vec!["x".to_string(), "y".to_string()]);
    // A call inside a method body is still lowered (reachability preserved).
    assert!(has_call(&ast, "foo"), "call inside def body must be lowered");
}

#[test]
fn lowers_class_def_superclass_name() {
    // `class User < ApplicationRecord; end` — superclass recorded as the
    // simple last-component name.
    let ast = lower(&crate::parse(b"class User < ApplicationRecord\nend\n"));
    let (name, sup, _) = class_def(&ast);
    assert_eq!(name, "User");
    assert_eq!(sup.as_deref(), Some("ApplicationRecord"));
}

#[test]
fn lowers_namespaced_class_name_and_super_path() {
    // `class Foo::Bar < Base::Thing; end` — dotted name, super last comp.
    let ast = lower(&crate::parse(b"class Foo::Bar < Base::Thing\nend\n"));
    let (name, sup, _) = class_def(&ast);
    assert_eq!(name, "Foo::Bar");
    assert_eq!(sup.as_deref(), Some("Thing"));
}

#[test]
fn singleton_def_is_not_an_instance_method() {
    // `def self.make` is a singleton method — it must NOT be collected as an
    // instance method (else `X.new.make` would wrongly look defined).
    let ast = lower(&crate::parse(b"class C\n  def self.make\n  end\n  def go\n  end\nend\n"));
    let (_, _, methods) = class_def(&ast);
    assert_eq!(methods, vec!["go".to_string()]);
}

#[test]
fn reopened_class_lowers_two_class_defs() {
    // Two `class C` bodies lower to two ClassDef nodes; the SourceIndex
    // unions them (tested in rigor-infer). Here we just assert both appear.
    let ast = lower(&crate::parse(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\n"));
    let defs: Vec<_> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::ClassDef { name, methods, .. } => Some((name.clone(), methods.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(defs.len(), 2);
    assert_eq!(defs[0], ("C".to_string(), vec!["a".to_string()]));
    assert_eq!(defs[1], ("C".to_string(), vec!["b".to_string()]));
}

/// The `method_bodies` of the single ClassDef in `ast`.
fn class_method_bodies(ast: &LoweredAst) -> Vec<MethodBody> {
    ast.iter()
        .find_map(|(_, n)| match n {
            Node::ClassDef { method_bodies, .. } => Some(method_bodies.clone()),
            _ => None,
        })
        .expect("expected a ClassDef node")
}

#[test]
fn harvests_method_body_with_name() {
    // `def full_name; "#{first} #{last}"; end` — harvested as ("full_name",
    // <non-empty body>, has_explicit_return=false).
    let src = b"class User\n  def full_name\n    \"#{first} #{last}\"\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let mbs = class_method_bodies(&ast);
    assert_eq!(mbs.len(), 1);
    assert_eq!(mbs[0].name, "full_name");
    assert!(!mbs[0].body.is_empty(), "body ids must be captured");
    assert!(!mbs[0].has_explicit_return);
}

#[test]
fn harvest_excludes_singleton_def() {
    // `def self.make` is a singleton method — not harvested as a tier-4b body.
    let src = b"class C\n  def self.make\n    1\n  end\n  def go\n    2\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let mbs = class_method_bodies(&ast);
    let names: Vec<_> = mbs.iter().map(|m| m.name.clone()).collect();
    assert_eq!(names, vec!["go".to_string()]);
}

#[test]
fn harvest_excludes_nested_and_conditional_defs() {
    // A def inside an `if` and a def inside an inner class are NOT direct
    // children of the outer class body, so they are not harvested for it.
    let src = b"class Outer\n  def direct\n    1\n  end\n  if cond\n    def conditional\n      2\n    end\n  end\n  class Inner\n    def nested\n      3\n    end\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    // Locate the OUTER ClassDef (name "Outer") specifically.
    let mbs = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::ClassDef { name, method_bodies, .. } if name == "Outer" => {
                Some(method_bodies.clone())
            }
            _ => None,
        })
        .expect("expected an Outer ClassDef");
    let names: Vec<_> = mbs.iter().map(|m| m.name.clone()).collect();
    assert_eq!(names, vec!["direct".to_string()]);
}

#[test]
fn harvest_records_has_explicit_return() {
    // A body with `return` is flagged; a tail-only body is not.
    let with = lower(&crate::parse(
        b"class C\n  def m\n    return 1 if x\n    2\n  end\nend\n",
    ));
    assert!(class_method_bodies(&with)[0].has_explicit_return);
    let without = lower(&crate::parse(b"class C\n  def m\n    1\n  end\nend\n"));
    assert!(!class_method_bodies(&without)[0].has_explicit_return);
}

#[test]
fn lowers_module_def_name_and_methods() {
    let ast = lower(&crate::parse(b"module M\n  def helper\n  end\nend\n"));
    let (name, methods) = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::ModuleDef { name, methods, .. } => Some((name.clone(), methods.clone())),
            _ => None,
        })
        .expect("expected a ModuleDef node");
    assert_eq!(name, "M");
    assert_eq!(methods, vec!["helper".to_string()]);
}

// --- ADR-35 slice 1: visibility-table + include discovery ----------------

/// The `(method_visibilities, includes)` of the single ClassDef in `ast`.
fn class_vis_includes(
    ast: &LoweredAst,
) -> (Vec<(String, Visibility)>, Vec<String>) {
    ast.iter()
        .find_map(|(_, n)| match n {
            Node::ClassDef { method_visibilities, includes, .. } => {
                Some((method_visibilities.clone(), includes.clone()))
            }
            _ => None,
        })
        .expect("expected a ClassDef node")
}

#[test]
fn discovers_bare_modifier_flips_running_default() {
    // `def a` is public; after a bare `private`, `def b` is private; a
    // subsequent bare `public` makes `def c` public again.
    let src = b"class C\n  def a\n  end\n  private\n  def b\n  end\n  public\n  def c\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let (vis, _) = class_vis_includes(&ast);
    assert_eq!(
        vis,
        vec![
            ("a".to_string(), Visibility::Public),
            ("b".to_string(), Visibility::Private),
            ("c".to_string(), Visibility::Public),
        ]
    );
}

#[test]
fn discovers_named_arg_back_patch() {
    // `private :foo` back-patches an already-recorded `foo` to private,
    // leaving the running default (and `bar`) public.
    let src = b"class C\n  def foo\n  end\n  def bar\n  end\n  private :foo\nend\n";
    let ast = lower(&crate::parse(src));
    let (vis, _) = class_vis_includes(&ast);
    assert_eq!(
        vis,
        vec![
            ("foo".to_string(), Visibility::Private),
            ("bar".to_string(), Visibility::Public),
        ]
    );
}

#[test]
fn discovers_string_arg_back_patch() {
    // `protected "foo"` (a string literal arg) marks `foo` protected.
    let src = b"class C\n  def foo\n  end\n  protected \"foo\"\nend\n";
    let ast = lower(&crate::parse(src));
    let (vis, _) = class_vis_includes(&ast);
    assert_eq!(vis, vec![("foo".to_string(), Visibility::Protected)]);
}

#[test]
fn private_def_modifier_records_at_default_not_private() {
    // `private def foo; end` — the wrap-around form is NOT tracked as a
    // visibility change: `foo` records at the running default (Public),
    // mirroring the reference gap (keeps the witness set ⊆ reference's).
    let src = b"class C\n  private def foo\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let (vis, _) = class_vis_includes(&ast);
    assert_eq!(vis, vec![("foo".to_string(), Visibility::Public)]);
}

#[test]
fn discovers_include_and_prepend_full_path() {
    // `include Foo::Bar` / `prepend Baz` collect the FULL written constant
    // path (so the override walk can resolve against lexical nesting).
    let src = b"class C\n  include Foo::Bar\n  prepend Baz\n  def a\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let (_, includes) = class_vis_includes(&ast);
    assert_eq!(includes, vec!["Foo::Bar".to_string(), "Baz".to_string()]);
}

#[test]
fn singleton_def_excluded_from_visibility_table() {
    // `def self.x` is a singleton method — never in the visibility table.
    let src = b"class C\n  private\n  def self.x\n  end\n  def y\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let (vis, _) = class_vis_includes(&ast);
    // Only the instance method `y` (at the running private default) appears.
    assert_eq!(vis, vec![("y".to_string(), Visibility::Private)]);
}

#[test]
fn module_discovers_visibility_and_includes() {
    // The ModuleDef carries the same tables.
    let src = b"module M\n  include Helper\n  def a\n  end\n  private\n  def b\n  end\nend\n";
    let ast = lower(&crate::parse(src));
    let (vis, includes) = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::ModuleDef { method_visibilities, includes, .. } => {
                Some((method_visibilities.clone(), includes.clone()))
            }
            _ => None,
        })
        .expect("expected a ModuleDef node");
    assert_eq!(
        vis,
        vec![
            ("a".to_string(), Visibility::Public),
            ("b".to_string(), Visibility::Private),
        ]
    );
    assert_eq!(includes, vec!["Helper".to_string()]);
}

#[test]
fn definition_records_name_span_on_name_token() {
    // The `Definition` node anchors `name_span` on the method-NAME token.
    let src = b"def foo\nend\n";
    let ast = lower(&crate::parse(src));
    let name_span = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::Definition { name: Some(nm), name_span: Some(sp), .. } if nm == "foo" => {
                Some(*sp)
            }
            _ => None,
        })
        .expect("expected a named Definition");
    assert_eq!(&src[name_span.0..name_span.1], b"foo");
}

#[test]
fn lowers_valued_break_and_next_as_jump_carriers() {
    // rigor-rs#140: `break e` / `next e` keep their value expressions in a
    // `StatementsKind::Jump` carrier — the exactly-once block-timing proof
    // discriminates the kind, and the values stay lowered/reachable.
    let src = b"[1].each { break \"s\" }
[1].each { next 1 }
";
    let ast = lower(&crate::parse(src));

    let mut kinds = Vec::new();
    for (_, n) in ast.iter() {
        if let Node::Statements { body, kind: StatementsKind::Jump(jk), .. } = n {
            assert_eq!(body.len(), 1, "valued jump carries exactly one arg");
            kinds.push(*jk);
        }
    }
    assert_eq!(kinds, vec![JumpKind::Break, JumpKind::Next]);
}

#[test]
fn lowers_bare_break_and_next_as_tagged_other() {
    // The argument-less forms stay the tagged `Node::Other` leaf.
    let src = b"[1].each { break }
[1].each { next }
";
    let ast = lower(&crate::parse(src));

    let mut kinds = Vec::new();
    for (_, n) in ast.iter() {
        if let Node::Other { jump: Some(jk), .. } = n {
            kinds.push(*jk);
        }
    }
    assert_eq!(kinds, vec![JumpKind::Break, JumpKind::Next]);
}

#[test]
fn lowers_redo_and_retry_as_jump_carriers() {
    // `redo` / `retry` take no argument list; they lower to `Jump` carriers
    // with an empty `body` so `never_completes_normally?` can see them.
    let src = b"[1].each { redo }\nbegin\nrescue\n  retry\nend\n";
    let ast = lower(&crate::parse(src));

    let mut kinds = Vec::new();
    for (_, n) in ast.iter() {
        if let Node::Statements { body, kind: StatementsKind::Jump(jk), .. } = n {
            assert!(body.is_empty(), "argument-less jump carries no children");
            kinds.push(*jk);
        }
    }
    assert_eq!(kinds, vec![JumpKind::Redo, JumpKind::Retry]);
}

#[test]
fn call_records_explicit_arg_list_presence() {
    // `x.tap` and `x.tap()` both lower `args: []` AND both leave
    // `explicit_arg_list` false — Prism produces no ArgumentsNode for
    // empty parens, and the reference's `node.arguments` gate (rigor#1105)
    // sees the same nil. Only a non-empty argument list sets the flag.
    let src = b"x.tap { }\nx.tap() { }\nx.tap(1) { }\nx.tap\n";
    let ast = lower(&crate::parse(src));

    let flags: Vec<bool> = ast
        .iter()
        .filter_map(|(_, n)| match n {
            Node::Call { method, explicit_arg_list, .. } if method == "tap" => {
                Some(*explicit_arg_list)
            }
            _ => None,
        })
        .collect();
    assert_eq!(flags, vec![false, false, true, false]);
}

#[test]
fn begin_rescue_main_body_excludes_rescue_else_ensure() {
    // `main_body` is JUST the protected statements — the flat `body` still
    // appends the rescue / `else` / `ensure` children, but an `else` must
    // not count toward `never_completes_normally?` (rigor-rs#140).
    let src = b"begin\n  \"x\"\nrescue\n  \"r\"\nelse\n  \"e\"\nensure\n  \"n\"\nend\n";
    let ast = lower(&crate::parse(src));

    let (body, main_body, ensure_body) = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::BeginRescue { body, main_body, ensure_body, .. } => {
                Some((body.clone(), main_body.clone(), ensure_body.clone()))
            }
            _ => None,
        })
        .expect("expected a BeginRescue node");
    assert_eq!(main_body.len(), 1, "main_body holds only the protected stmt");
    assert_eq!(ensure_body.len(), 1, "ensure_body holds only the ensure stmt");
    assert_eq!(body.len(), 4, "flat body still appends every clause");
    assert_eq!(&body[..main_body.len()], &main_body[..]);
}

