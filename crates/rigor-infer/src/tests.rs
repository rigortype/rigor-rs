use super::*;
use rigor_parse::{lower, parse, Node, StatementsKind};
use rigor_types::{Scalar, ShapeKey, ShapeMember, Type};

fn lower_src(src: &[u8]) -> LoweredAst {
    lower(&parse(src))
}

#[test]
fn mutator_tables_match_the_pinned_reference() {
    // Sizes at pin `v0.4.2` (`mutation_widening.rb`, `string_mutation.rb`,
    // `hash_lookup_mutation.rb`) — the same sets the effect catalogue's
    // `mutators.yml` is extracted from, so a drift here and a
    // `vendor_effects.py --check` failure arrive together.
    assert_eq!(ARRAY_MUTATORS.len(), 31);
    assert_eq!(HASH_MUTATORS.len(), 16);
    assert!(HASH_MUTATORS.contains(&"shift"));
    // 35 at `e59b7b89`; v0.4.2 added Ruby 4.1's seven bit operations (#1691).
    assert_eq!(STRING_MUTATORS.len(), 42);
    assert_eq!(HASH_LOOKUP_MUTATORS.len(), 3);
    // The local-widening set is exactly Array ∪ Hash ∪ String — the
    // `HashLookupMutation` names stay out (see `MUTATOR_METHODS`).
    let union: std::collections::BTreeSet<&str> = ARRAY_MUTATORS
        .iter()
        .chain(HASH_MUTATORS)
        .chain(STRING_MUTATORS)
        .copied()
        .collect();
    let widening: std::collections::BTreeSet<&str> =
        MUTATOR_METHODS.iter().copied().collect();
    assert_eq!(widening.len(), MUTATOR_METHODS.len(), "MUTATOR_METHODS repeats a name");
    assert_eq!(widening, union);
    for m in HASH_LOOKUP_MUTATORS {
        assert!(!MUTATOR_METHODS.contains(m));
        assert!(is_shape_mutator(m), "{m} is a SHAPE_MUTATORS member");
    }
    assert!(union.iter().all(|m| is_shape_mutator(m)));
    assert!(!is_shape_mutator("upcase"));
}

#[test]
fn types_string_and_integer_literals() {
    let ast = lower_src(b"\"Hello\"\n42\n");
    let mut i = Interner::new();
    let env = TypeEnv::new();
    // Locate the two literal nodes and type them.
    let str_id = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::StringLit { .. }).then_some(id))
        .unwrap();
    let int_id = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::IntegerLit { .. }).then_some(id))
        .unwrap();
    let str_ty = type_of(&ast, str_id, &env, &mut i);
    assert_eq!(i.get(str_ty), &Type::Constant(Scalar::Str("Hello".into())));
    let int_ty = type_of(&ast, int_id, &env, &mut i);
    assert_eq!(i.get(int_ty), &Type::Constant(Scalar::Int(42)));
}

/// Value-pinned Tuple projection folds: a no-arg accessor / constant index
/// on an array literal folds to the pinned element or arity.
#[test]
fn tuple_projection_folds() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let case = |src: &[u8], expect: Type| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, call_id, &env, &mut i);
        assert_eq!(i.get(ty), &expect, "src={}", String::from_utf8_lossy(src));
    };
    case(b"[1, 2, 3].first\n", Type::Constant(Scalar::Int(1)));
    case(b"[1, 2, 3].last\n", Type::Constant(Scalar::Int(3)));
    case(b"[1, 2, 3].size\n", Type::Constant(Scalar::Int(3)));
    case(b"[10, 20][1]\n", Type::Constant(Scalar::Int(20)));
    case(b"[10, 20][-1]\n", Type::Constant(Scalar::Int(20)));
    case(b"[1, 2].empty?\n", Type::Constant(Scalar::Bool(false)));
    case(b"[].first\n", Type::Constant(Scalar::Nil));
    // Out of bounds DECLINES (reference `tuple_single_index` → nil from
    // `normalise_index`, issue #479): never a proven `nil`.
    for src in [&b"[1, 2][9]\n"[..], b"[1, 2][-3]\n"] {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let call_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, call_id, &TypeEnv::new(), &mut i);
        assert_eq!(ty, i.untyped(), "src={}", String::from_utf8_lossy(src));
    }
}

/// Kernel `#p` / `#pp` identity typing on the implicit-self (`receiver:
/// None`) path — the full p01–p11 probe matrix, both firing (a folded value
/// carrier) and silent (`untyped`/Dynamic) directions. Types the LAST call
/// in each snippet: for the firing probes that is the `p`/`pp` call whose
/// value we assert; for the silent probes it is either the declined `p`/`pp`
/// call or an explicit-receiver `Kernel.p` that never reaches our path.
#[test]
fn kernel_p_pp_identity_typing() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    // Type the p/pp call of interest. `nth_from_end` selects which
    // implicit-self (receiver-None) call to type, counting from the end
    // (0 = last) — needed for p07/p11 where a `def p` / method body adds
    // additional receiver-None calls we must skip past.
    let describe_ty = |src: &[u8], want_recv_none: bool| -> String {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .filter_map(|(id, n)| match n {
                Node::Call { receiver, method, .. }
                    if receiver.is_none() == want_recv_none
                        && (method == "p" || method == "pp") =>
                {
                    Some(id)
                }
                _ => None,
            })
            .last()
            .unwrap();
        let ty = typer.type_of(&ast, call_id, &env, &mut i);
        rigor_types::describe(&i, ty)
    };

    // p01: `p 42` → identity → Constant[42].
    assert_eq!(describe_ty(b"p 42\n", true), "Constant[42]");
    // p02: `p(1, "a")` → Tuple of the arg types.
    assert_eq!(describe_ty(b"p(1, \"a\")\n", true), "Tuple[Constant[1], Constant[\"a\"]]");
    // p03: bare `p` → nil (NOT declined — rigor-rs has no RBS tier on this
    // path, so the fold must carry the nil itself).
    assert_eq!(describe_ty(b"p\n", true), "nil");
    // p04: `pp 42` → identity → Constant[42].
    assert_eq!(describe_ty(b"pp 42\n", true), "Constant[42]");
    // p09: block form still folds (a block does not block the fold).
    assert_eq!(describe_ty(b"p(42) { 1 }\n", true), "Constant[42]");
    // p10: HashShape passes through the identity unchanged.
    assert_eq!(describe_ty(b"p({a: 1})\n", true), "{:a => Constant[1]}");

    // p05: `Kernel.p(42)` — the explicit `module_function` spelling folds to
    // the SAME identity as implicit-self (upstream c9d2e473), BUT only once
    // the receiver types to `Singleton[Kernel]`, which needs a populated
    // source index. This no-source harness types `Kernel` to Dynamic, so it
    // declines here; the fold is exercised in
    // `kernel_explicit_receiver_folds_like_implicit_self` (with a real source).
    assert_eq!(describe_ty(b"Kernel.p(42)\n", false), "Dynamic[top]");

    // Silent directions — decline to Dynamic[top].
    // p07: a file-wide `def p` disables the fold file-wide.
    assert_eq!(describe_ty(b"def p(*a); nil; end\np 42\n", true), "Dynamic[top]");
    // p08: a splat arg makes arity unknown → decline.
    assert_eq!(describe_ty(b"a = [1, 2]\np(*a)\n", true), "Dynamic[top]");
    // p11: a Dynamic (unknown local) arg passes through identity as Dynamic.
    assert_eq!(describe_ty(b"p some_unknown_local\n", true), "Dynamic[top]");
}

/// The explicit `Kernel.` module_function spelling folds like implicit-self
/// across the whole intrinsic family (upstream c9d2e473): `Kernel.p`,
/// `Kernel.format`/`sprintf`, `Kernel.String`/`Integer`/`Float`. A non-fold
/// Kernel method stays Dynamic (falls through to the RBS surface).
#[test]
fn kernel_explicit_receiver_folds_like_implicit_self() {
    let index = CoreIndex::new();
    // A populated source index so the bare `Kernel` constant read types to
    // `Singleton[Kernel]` (the ConstantRead zero-FP gate resolves it via the
    // source registry) — the receiver shape the explicit-spelling fold keys on.
    let last_call_ty = |src: &[u8]| -> String {
        let ast = lower_src(src);
        let source = SourceIndex::build(&ast, &index);
        let typer = Typer::with_source(&index, &source);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .filter_map(|(id, n)| matches!(n, Node::Call { receiver: Some(_), .. }).then_some(id))
            .last()
            .unwrap();
        let ty = typer.type_of(&ast, call_id, &env, &mut i);
        rigor_types::describe(&i, ty)
    };
    // Identity printer via the module object.
    assert_eq!(last_call_ty(b"Kernel.p(42)\n"), "Constant[42]");
    assert_eq!(last_call_ty(b"Kernel.pp(1, 2)\n"), "Tuple[Constant[1], Constant[2]]");
    // Conversion + format folds, same envelope as implicit self.
    assert_eq!(last_call_ty(b"Kernel.format(\"%d\", 1)\n"), "Constant[\"1\"]");
    assert_eq!(last_call_ty(b"Kernel.String(42)\n"), "Constant[\"42\"]");
    // A non-fold Kernel method is not a fold target → Dynamic (RBS answers).
    assert_eq!(last_call_ty(b"Kernel.puts(\"x\")\n"), "Dynamic[top]");
}

/// An `if`/`unless`/ternary as an expression types to the union of its
/// branch values, with a known-polarity predicate eliding the dead branch.
#[test]
fn if_expression_unions_and_elides_branches() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let describe = |src: &[u8]| -> String {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let if_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, if_id, &env, &mut i);
        rigor_types::describe(&i, ty)
    };
    // The internal `describe` spells constants `Constant[n]`; the point here
    // is the union/elision structure, not the user-facing rendering.
    // Unknown predicate → union of both branches (a missing else ⇒ nil).
    assert_eq!(describe(b"if c then 1 else 2 end\n"), "Constant[1] | Constant[2]");
    assert_eq!(describe(b"if c then 1 end\n"), "Constant[1] | nil");
    // Truthy constant predicate → then branch only (elided).
    assert_eq!(describe(b"if true then 1 else 2 end\n"), "Constant[1]");
    // Falsey predicate → else branch only.
    assert_eq!(describe(b"if nil then 1 else 2 end\n"), "Constant[2]");
}

/// A `case`/`when` expression types to the union of its branch values + the
/// `else` value (nil when no `else`).
#[test]
fn case_expression_unions_branch_values() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let describe = |src: &[u8]| -> String {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let case_id = ast
            .iter()
            .find_map(|(id, n)| matches!(n, Node::Case { .. }).then_some(id))
            .unwrap();
        let ty = typer.type_of(&ast, case_id, &env, &mut i);
        rigor_types::describe(&i, ty)
    };
    assert_eq!(
        describe(b"case x\nwhen 1 then 10\nwhen 2 then 20\nelse 30\nend\n"),
        "Constant[10] | Constant[20] | Constant[30]"
    );
    // No else → nil joins the union (a non-exhaustive case returns nil).
    assert_eq!(
        describe(b"case x\nwhen 1 then 10\nend\n"),
        "Constant[10] | nil"
    );
}

/// The flow-constant substrate (ADR-0022) records a straight-line dominating
/// constant for an `if` predicate.
#[test]
fn flow_snapshot_folds_straight_line_constant() {
    let ast = lower_src(b"x = 5\nif x\n  noop\nend\n");
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let if_id = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .unwrap();
    let ty = snaps.get(&if_id).copied().expect("predicate snapshot recorded");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(5)));
}

/// The branch-join keystone: a conditionally reassigned local is widened, so
/// a later predicate reading it is NOT a constant (the zero-FP guarantee the
/// flat env cannot provide).
#[test]
fn flow_snapshot_widens_conditional_reassignment() {
    let ast = lower_src(b"x = 5\nif g\n  x = f\nend\nif x\n  noop\nend\n");
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let ifs: Vec<_> = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .collect();
    assert_eq!(ifs.len(), 2, "expected two if nodes");
    let ty2 = snaps.get(&ifs[1]).copied().expect("second if recorded");
    assert!(
        !matches!(i.get(ty2), Type::Constant(_)),
        "x must be widened to non-constant after a conditional reassignment"
    );
}

/// MutationWidening (parser.rb FP): a value-pinned collection local that is
/// content-mutated by an in-place mutator call must widen, so a later
/// `local.count`/`.size` predicate is NOT a folded constant. `true` means the
/// predicate folds to a `Type::Constant` (the always-truthy rule WOULD fire);
/// `false` means it was widened (declined). The predicate reads the LAST `if`.
fn last_if_predicate_is_constant(src: &[u8]) -> bool {
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps
        .get(&last_if)
        .copied()
        .expect("predicate snapshot recorded for a top-level if");
    matches!(i.get(ty), Type::Constant(_))
}

/// P2 rail: NO mutation ⇒ the `[]`-pinned `results.count > 1` still folds and
/// the always-truthy rule must KEEP firing. This is the load-bearing negative
/// control — the fix must not widen an unmutated local.
#[test]
fn mutation_widening_p2_no_mutation_keeps_firing() {
    assert!(last_if_predicate_is_constant(
        b"results = []\nif results.count > 1\n  noop\nend\n"
    ));
    // Both count directions fold (parser.rb fires on `> 1` and `< 1`).
    assert!(last_if_predicate_is_constant(
        b"results = []\nif results.count < 1\n  noop\nend\n"
    ));
}

/// A NON-mutator call on the local (`map`, a pure sibling) must NOT widen — a
/// guard that the extension keys on the mutator set, not on any call.
#[test]
fn mutation_widening_non_mutator_call_keeps_firing() {
    assert!(last_if_predicate_is_constant(
        b"results = []\nresults.map { |x| x }\nif results.count > 1\n  noop\nend\n"
    ));
}

/// P3: a straight-line `results.push(1)` (no block) widens the local — its own
/// call span is the containing span, resolved through the catch-all arm.
#[test]
fn mutation_widening_p3_straight_line_push_stops_firing() {
    assert!(!last_if_predicate_is_constant(
        b"results = []\nresults.push(1)\nif results.count > 1\n  noop\nend\n"
    ));
}

/// P4: a `push` under an `if` modifier widens (the then-branch mutation
/// disagrees with the untaken else at the join).
#[test]
fn mutation_widening_p4_push_under_if_modifier_stops_firing() {
    assert!(!last_if_predicate_is_constant(
        b"results = []\nresults.push(1) if cond\nif results.count > 1\n  noop\nend\n"
    ));
}

/// P1: the parser.rb shape — `push`/`pop` inside a nested `case` in an `each`
/// block. `ast.iter()` finds the mutation spans; the enclosing `each` call span
/// contains them, so the catch-all arm widens `results`.
#[test]
fn mutation_widening_p1_block_nested_case_stops_firing() {
    let src = b"results = []\nxs.each do |t|\n  case t\n  when 1\n    results.push(t)\n  when 2\n    results.pop\n  end\nend\nif results.count > 1\n  noop\nend\n";
    assert!(!last_if_predicate_is_constant(src));
    // Same shape, `< 1` direction.
    let src_lt = b"results = []\nxs.each do |t|\n  case t\n  when 1\n    results.push(t)\n  end\nend\nif results.count < 1\n  noop\nend\n";
    assert!(!last_if_predicate_is_constant(src_lt));
}

/// P5: a rebind (`results = results + [x]`) inside the block widens through the
/// pre-existing `LocalVariableWrite` arm — correct on both sides already, and
/// still correct after the mutator extension.
#[test]
fn mutation_widening_p5_rebind_in_block_stops_firing() {
    assert!(!last_if_predicate_is_constant(
        b"results = []\nxs.each do |t|\n  results = results + [t]\nend\nif results.count > 1\n  noop\nend\n"
    ));
}

/// P7: `results << t` inside a block — `<<` is a mutator, widened via the
/// block-containing span.
#[test]
fn mutation_widening_p7_shovel_in_block_stops_firing() {
    assert!(!last_if_predicate_is_constant(
        b"results = []\nxs.each do |t|\n  results << t\nend\nif results.count > 1\n  noop\nend\n"
    ));
}

/// ADR-0038 Slice 1: a nilable String slice bound in a NESTED block, with its
/// receiver typed by a `String.new` in an OUTER block, fires possible-nil on
/// the same-block use. The block-scope shape the substrate unlocks.
#[test]
fn nil_snapshot_fires_on_block_scope_string_slice() {
    let ast = lower_src(
        b"outer do\n  s = String.new(\"hello\")\n  inner do\n    sub = s[0..2]\n    n = sub.size\n  end\nend\n",
    );
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
    // The `sub.size` call is the nilable-receiver use; its arm is String.
    let use_id = ast
        .iter()
        .find_map(|(id, n)| match n {
            Node::Call { receiver: Some(r), method, .. }
                if method == "size"
                    && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == "sub") =>
            {
                Some(id)
            }
            _ => None,
        })
        .expect("sub.size call present");
    assert_eq!(snaps.get(&use_id).copied(), Some("String"));
}

/// ADR-0039 §2: an `Array.new(n > 16)` slice IS a source (the reference keeps
/// it `Nominal[Array]`, so `arr[Range] : Array?` fires). Provenance-gated.
#[test]
fn nil_snapshot_array_new_large_slice_fires() {
    let ast = lower_src(b"arr = Array.new(300000) { |i| i }\nsub = arr[0..5]\nn = sub.size\n");
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
    let use_id = ast
        .iter()
        .find_map(|(id, n)| match n {
            Node::Call { receiver: Some(r), method, .. }
                if method == "size"
                    && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == "sub") =>
            {
                Some(id)
            }
            _ => None,
        })
        .expect("sub.size call present");
    assert_eq!(snaps.get(&use_id).copied(), Some("Array"));
}

/// The reference `Tuple`s a small `Array.new(n ≤ 16)` and every array literal
/// (their slice is non-nil), so those slices must NOT fire — else an FP. The
/// provenance gate (small const / literal ⇒ no provenance) keeps them silent.
#[test]
fn nil_snapshot_small_array_new_and_literal_slices_decline() {
    for src in [
        b"arr = Array.new(10) { |i| i }\nsub = arr[0..5]\nn = sub.size\n".as_slice(),
        b"arr = [1, 2, 3]\nsub = arr[0..1]\nn = sub.size\n".as_slice(),
        b"arr = [1, 2, 3].map { |x| x }\nsub = arr[0..1]\nn = sub.size\n".as_slice(),
    ] {
        let ast = lower_src(src);
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
        assert!(
            snaps.is_empty(),
            "small/literal/.map array slice must not mint a nilable fact: {:?}",
            std::str::from_utf8(src).unwrap()
        );
    }
}

/// The decline backstop: a guard (`if`) between the slice source and the use
/// clears the fact, so no snapshot is recorded (zero-FP over recall).
#[test]
fn nil_snapshot_declines_on_guard_between_source_and_use() {
    let ast = lower_src(
        b"s = String.new(\"abc\")\nsub = s[0..1]\nif sub\n  noop\nend\nn = sub.size\n",
    );
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
    let use_id = ast
        .iter()
        .find_map(|(id, n)| match n {
            Node::Call { receiver: Some(r), method, .. }
                if method == "size"
                    && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == "sub") =>
            {
                Some(id)
            }
            _ => None,
        })
        .expect("sub.size call present");
    assert_eq!(snaps.get(&use_id), None, "an intervening guard must decline");
}

// -----------------------------------------------------------------------
// P2 (2026-07-17) — `Regexp.last_match` optional-local nil source
// -----------------------------------------------------------------------

/// Snapshot arm recorded for the FIRST call whose receiver is a bare local
/// read of `recv` and method is `method`, or `None`.
fn last_match_use_arm(src: &[u8], recv: &str, method: &str) -> Option<&'static str> {
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
    let use_id = ast.iter().find_map(|(id, n)| match n {
        Node::Call { receiver: Some(r), method: m, .. }
            if m == method
                && matches!(ast.get(*r), Node::LocalVariableRead { name, .. } if name == recv) =>
        {
            Some(id)
        }
        _ => None,
    })?;
    snaps.get(&use_id).copied()
}

/// `Regexp.last_match(n) -> String?`: the integer-literal arg gives a
/// concrete `String` arm, so a straight-line `content.gsub(...)` fires (the
/// `dictionary_credentials_handler` / `hugo_transformer` gitlab cluster).
/// Both `::Regexp` and `Regexp` lower to `ConstantRead "Regexp"`.
#[test]
fn p2_regexp_last_match_int_arg_is_string_source() {
    for src in [
        b"content = ::Regexp.last_match(2)\nnew = content.gsub(\"a\", \"b\")\n".as_slice(),
        b"content = Regexp.last_match(1)\nnew = content.gsub(\"a\", \"b\")\n".as_slice(),
    ] {
        assert_eq!(
            last_match_use_arm(src, "content", "gsub"),
            Some("String"),
            "Regexp.last_match(int) must mint a String|nil source: {:?}",
            std::str::from_utf8(src).unwrap()
        );
    }
}

/// `Regexp.last_match(name) -> String?` for a String / Symbol literal arg.
#[test]
fn p2_regexp_last_match_name_arg_is_string_source() {
    for src in [
        b"c = Regexp.last_match(:key)\nn = c.upcase\n".as_slice(),
        b"c = Regexp.last_match(\"key\")\nn = c.upcase\n".as_slice(),
    ] {
        assert_eq!(last_match_use_arm(src, "c", "upcase"), Some("String"));
    }
}

/// `Regexp.last_match() -> MatchData?`: the zero-arg form mints a `MatchData`
/// arm, so `match[0]` / `match.begin(0)` fire (the `collection` / second
/// `hugo_transformer` gitlab cluster).
#[test]
fn p2_regexp_last_match_zero_arg_is_matchdata_source() {
    let src = b"m = Regexp.last_match\nfull = m[0]\nb = m.begin(0)\n";
    assert_eq!(last_match_use_arm(src, "m", "[]"), Some("MatchData"));
    assert_eq!(last_match_use_arm(src, "m", "begin"), Some("MatchData"));
}

/// A NON-literal 1-arg call fires too (compat plan S2): every 1-arity
/// overload returns `String?`, so the reference resolves BY ARITY — the arg's
/// shape does not matter (fixture 65 `non_literal_arg`).
#[test]
fn p2_regexp_last_match_non_literal_arg_is_string_source() {
    assert_eq!(
        last_match_use_arm(b"i = 2\nc = Regexp.last_match(i)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
        Some("String")
    );
}

/// Decline conditions (FP backstop): a splat / multi arg to `last_match`
/// (arity unknown / raises), a NON-`Regexp` constant receiver, a guard
/// between the bind and the use, and a safe-nav deref all record no snapshot.
#[test]
fn p2_regexp_last_match_declines() {
    // splat arg — arity statically unknown (could be the 0-arg MatchData form)
    assert_eq!(
        last_match_use_arm(b"a = [1]\nc = Regexp.last_match(*a)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
        None
    );
    // multi arg — no such overload (raises at runtime)
    assert_eq!(
        last_match_use_arm(b"c = Regexp.last_match(1, 2)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
        None
    );
    // a different constant named `.last_match` is not the core Regexp source
    assert_eq!(
        last_match_use_arm(b"c = Foo.last_match(2)\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
        None
    );
    // intervening guard clears the fact
    assert_eq!(
        last_match_use_arm(b"c = Regexp.last_match(2)\nif c\n  noop\nend\nn = c.gsub(\"a\", \"b\")\n", "c", "gsub"),
        None
    );
    // safe-nav deref is not a bug (short-circuits on nil)
    assert_eq!(
        last_match_use_arm(b"c = Regexp.last_match(2)\nn = c&.gsub(\"a\", \"b\")\n", "c", "gsub"),
        None
    );
}

/// rigor-rs#449: an early-exit guard that proves a `=~` match leaves
/// `Regexp.last_match(N)` non-nil for an unconditional group N, as the
/// reference's narrowed `$N` does (`RegexpFolding.fold_last_match_group`).
/// An optional group stays `String?` on a successful match, so it still
/// fires; so does a later `=~` that rebinds `$~`.
#[test]
fn issue_449_last_match_after_early_exit_guard() {
    let guarded = b"def a(line)\n  return unless line =~ /(\\w+)/\n  k = Regexp.last_match(1)\n  k.upcase\nend\n";
    assert_eq!(last_match_use_arm(guarded, "k", "upcase"), None);
    let not_match = b"def a(line)\n  return if line !~ /(\\w+)/\n  k = Regexp.last_match(1)\n  k.upcase\nend\n";
    assert_eq!(last_match_use_arm(not_match, "k", "upcase"), None);
    let optional = b"def a(line)\n  return unless line =~ /(\\w+)?/\n  k = Regexp.last_match(1)\n  k.upcase\nend\n";
    assert_eq!(last_match_use_arm(optional, "k", "upcase"), Some("String"));
    let rebound = b"def a(line)\n  return unless line =~ /(\\w+)/\n  line =~ /x(\\d+)/\n  k = Regexp.last_match(1)\n  k.upcase\nend\n";
    assert_eq!(last_match_use_arm(rebound, "k", "upcase"), Some("String"));
    // The no-match edge falls through `return if line =~ re`.
    let no_match = b"def a(line)\n  return if line =~ /(\\w+)/\n  k = Regexp.last_match(1)\n  k.upcase\nend\n";
    assert_eq!(last_match_use_arm(no_match, "k", "upcase"), Some("String"));
}

/// A same-named block parameter must NOT inherit an outer nilable fact — the
/// fresh-per-block `nenv` makes the shadowing FP class structurally impossible.
#[test]
fn nil_snapshot_block_param_shadow_does_not_leak() {
    let ast = lower_src(b"sub = String.new(\"x\")[0..2]\n[1, 2].each do |sub|\n  n = sub.size\nend\n");
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.nilable_receiver_snapshots(&ast, &mut i);
    // Even though `sub` is nilable outside, the block's `|sub|` is a different
    // variable; the fresh block `nenv` means no snapshot leaks in.
    assert!(
        snaps.is_empty(),
        "an outer fact must not leak past a same-named block param"
    );
}

/// rigor-rs#137 (upstream rigor#1245): the closure shadow set — every literal
/// block/lambda contributes its body's descendants + bound names (Prism
/// `locals` ∪ `block_params`, so `it` and `;` block-locals count), and a
/// closure a recovery walk CROSSED (`->(o) { … } rescue nil`) records the
/// same set on its recovered children.
#[test]
fn closure_shadow_scopes_cover_params_locals_it_and_crossed_children() {
    let ast = lower_src(
        b"[1].map { |o| o }\n[1].map { it }\n[1].map { |a; bl| bl }\n->(x) { x } rescue nil\n",
    );
    let scopes = closure_shadow_scopes(&ast);
    // `o` (a param), `it` (implicit param — Prism keeps it out of `locals`),
    // `a` + `bl` (param + `;` block-local), and `x` via the crossed lambda's
    // recovered child are all bound sets.
    let all: Vec<String> = scopes.iter().flat_map(|(_, b)| b.clone()).collect();
    for name in ["o", "it", "a", "bl", "x"] {
        assert!(all.iter().any(|n| n == name), "missing bound name {name}");
    }
    // Every use site inside the first block's body is a shadow descendant:
    // the `o` read is, the enclosing `map` call is not.
    let read_in_block = ast
        .iter()
        .find_map(|(id, n)| {
            matches!(n, Node::LocalVariableRead { name, .. } if name == "o").then_some(id)
        })
        .unwrap();
    let map_call = ast
        .iter()
        .find_map(|(id, n)| {
            matches!(n, Node::Call { method, .. } if method == "map").then_some(id)
        })
        .unwrap();
    let o_scope = scopes.iter().find(|(_, b)| b == &vec!["o".to_string()]).unwrap();
    assert!(o_scope.0.contains(&read_in_block));
    assert!(!o_scope.0.contains(&map_call));
}

/// rigor-rs#137: a write/mutation of a closure-bound name must not widen or
/// rebind the outer local it shadows — `collect_flow_writes` /
/// `collect_rebind_writes` / `toplevel_rebinds` / `toplevel_mutations` all
/// drop it; a captured name's write still counts.
#[test]
fn closure_bound_writes_are_not_outer_writes() {
    let ast = lower_src(
        b"o = { x: 1 }\n[1].each { |o| o = 2; o << 3 }\n\
          k = 1\n[1].each { |y| k = 2 }\n\
          x = 1\n->(x) { x = 2 } rescue nil\n",
    );
    let names = |v: &[(rigor_parse::Span, String)]| -> Vec<String> {
        v.iter().map(|(_, n)| n.clone()).collect()
    };
    // `o`/`x` writes inside the closures are shadowed; the captured `k`
    // write is a real top-level rebind.
    assert_eq!(names(&collect_rebind_writes(&ast)), ["o", "k", "k", "x"]);
    assert_eq!(names(&toplevel_rebinds(&ast)), ["o", "k", "k", "x"]);
    // Same for the mutation half: `o << 3` inside the block is off
    // `collect_flow_writes`/`toplevel_mutations`; the captured `k << 3` stays.
    let ast = lower_src(b"o = []\n[1].each { |o| o << 3 }\nk = []\n[1].each { |y| k << 3 }\n");
    assert_eq!(names(&collect_flow_writes(&ast)), ["o", "k", "k"]);
    assert_eq!(
        toplevel_mutations(&ast)
            .iter()
            .map(|(_, n, ..)| n.as_str())
            .collect::<Vec<_>>(),
        ["k"]
    );
    // The flat envs keep the shadowed binding, widen the captured one.
    let ast = lower_src(b"o = \"s\"\n[1].each { |o| o = 2 }\nk = \"s\"\n[1].each { |y| k = 2 }\n");
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let env = typer.build_toplevel_check_env(&ast, &mut i);
    let untyped = i.untyped();
    assert_eq!(env.get("o"), Some(&i.intern(Type::Constant(Scalar::Str("s".into())))));
    assert_eq!(env.get("k"), Some(&untyped));
}

/// rigor-rs#137: a write recovered under a CROSSED lambda binds the closure's
/// local, not the outer one — the flat env keeps the outer binding.
#[test]
fn crossed_closure_write_does_not_rebind_flat_env() {
    let ast = lower_src(b"o = \"s\"\n->(o) { o = 2 } rescue nil\n");
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    assert_eq!(
        env.get("o"),
        Some(&i.intern(Type::Constant(Scalar::Str("s".into()))))
    );
}

#[test]
fn local_read_resolves_from_env() {
    let ast = lower_src(b"s = \"Hello\"\ns.length\n");
    let mut i = Interner::new();
    let env = build_toplevel_env(&ast, &mut i);
    assert_eq!(
        env.get("s").copied().map(|t| i.get(t).clone()),
        Some(Type::Constant(Scalar::Str("Hello".into())))
    );
}

/// rigor-rs#133: the check env widens a local a nested construct rebinds (a
/// loop's or a block's `next` path included), keeps one nothing rebinds, and
/// lets a later straight-line write re-establish the type.
#[test]
fn toplevel_check_env_widens_nested_rebinds() {
    let ast = lower_src(
        b"w = \"s\"\nwhile $c\n  if $d\n    w = 1\n    next\n  end\nend\n\
          n = \"s\"\n[1].each { |e| n = e; next }\n\
          k = \"s\"\nwhile $c\n  next if $d\nend\n\
          r = \"s\"\nr = 1 if $c\nr = \"t\"\n\
          d = \"s\"\ndef m\n  d = 1\nend\n",
    );
    let mut i = Interner::new();
    let empty = CoreIndex::new();
    let env = Typer::new(&empty).build_toplevel_check_env(&ast, &mut i);
    let get = |name: &str, i: &Interner| env.get(name).map(|&t| i.get(t).clone());
    let untyped = i.untyped();
    // rigor-rs#133 / #958 / #1044: a terminated then-arm write (`w = 1;
    // next`) with no else is dropped by the if-join, but the `next` sink
    // scope reaches the reference's iteration-exit join, so the jump-loop
    // overlay now JOINS it: `w` is `"s" | 1` on the oracle (`w.w` fires
    // `for "s" | 1`, `w.even?` stays silent). The brief's else-break shape
    // is the `jump_loop_keeps_if_arm_join_writes` test.
    let w = env.get("w").copied().expect("w bound");
    let w_members = match i.get(w) {
        Type::Union(members) => members
            .iter()
            .map(|&m| i.get(m).clone())
            .collect::<Vec<_>>(),
        other => panic!("the next-terminated write joins, got {other:?}"),
    };
    assert_eq!(
        w_members,
        vec![Type::Constant(Scalar::Int(1)), Type::Constant(Scalar::Str("s".into()))],
        "the next-terminated write joins the entry with the arm"
    );
    assert_eq!(env.get("n"), Some(&untyped));
    assert_eq!(get("k", &i), Some(Type::Constant(Scalar::Str("s".into()))));
    assert_eq!(get("r", &i), Some(Type::Constant(Scalar::Str("t".into()))));
    assert_eq!(get("d", &i), Some(Type::Constant(Scalar::Str("s".into()))));
}

/// rigor-rs#958: a while/until body's env floor used to drop the if-arm
/// write the reference's `join_break_scopes` keeps. The 0-iteration entry
/// joins the live arm; a nearby no-break while still uses the fixpoint.
#[test]
fn jump_loop_keeps_if_arm_join_writes() {
    let empty = CoreIndex::new();
    let typer = Typer::new(&empty);
    let env_of = |src: &[u8]| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        (env, i)
    };
    let join_as = |i: &mut Interner| {
        let a = i.intern(Type::Constant(Scalar::Str("a".into())));
        let s = i.intern(Type::Constant(Scalar::Str("s".into())));
        i.intern(Type::Union(vec![a, s]))
    };

    let (env, mut i) = env_of(
        b"x = \"a\"\nwhile rand > 0\n  if rand > 1\n    x = \"s\"\n  else\n    break 1\n  end\nend\n",
    );
    let want = join_as(&mut i);
    assert_eq!(env.get("x"), Some(&want), "while else-break");

    let (env, mut i) = env_of(
        b"x = \"a\"\nuntil rand > 0\n  if rand > 1\n    x = \"s\"\n  else\n    break 1\n  end\nend\n",
    );
    let want = join_as(&mut i);
    assert_eq!(env.get("x"), Some(&want), "until else-break");

    // Must-still-fire control: no jump, the fixpoint path already joined.
    let (env, mut i) = env_of(
        b"x = \"a\"\nwhile rand > 0\n  if rand > 1\n    x = \"s\"\n  end\nend\n",
    );
    let want = join_as(&mut i);
    assert_eq!(env.get("x"), Some(&want), "while no-break control");

    // rigor-rs#1044: a write in a TERMINATING jump arm joins too — the
    // `break`/`next` sink scope reaches the reference's `join_break_scopes`
    // / iteration-exit join, so `x` is `"a" | "s" | 1` and `x.even?` stays
    // silent (the issue's FP row). Member order is the walk's, so compare
    // as a sorted set of debug-rendered members.
    let members_of = |env: &TypeEnv, i: &Interner| {
        let mut members: Vec<String> = match i.get(*env.get("x").expect("x bound")) {
            Type::Union(ms) => ms.iter().map(|&m| format!("{:?}", i.get(m))).collect(),
            other => vec![format!("{:?}", other)],
        };
        members.sort();
        members
    };
    let mut want3 = vec![
        format!("{:?}", Type::Constant(Scalar::Int(1))),
        format!("{:?}", Type::Constant(Scalar::Str("a".into()))),
        format!("{:?}", Type::Constant(Scalar::Str("s".into()))),
    ];
    want3.sort();
    for (label, jump) in [("break", "break 1"), ("next", "next")] {
        let src = format!(
            "x = \"a\"\nwhile rand > 0\n  if rand > 1\n    x = \"s\"\n  else\n    x = 1\n    {jump}\n  end\nend\n"
        );
        let (env, i) = env_of(src.as_bytes());
        assert_eq!(members_of(&env, &i), want3, "while else-write then {label}");
    }
    // The jump-terminated THEN arm beside no else joins the same way (the
    // rigor-rs#133 row: `w.w` fires `for "s" | 1` on the oracle).
    let (env, i) = env_of(
        b"x = \"a\"\nwhile rand > 0\n  if rand > 1\n    x = \"s\"\n    break\n  end\nend\n",
    );
    assert!(
        members_of(&env, &i).iter().any(|m| m.contains("Str(\"s\"")),
        "then-break write joins, got {:?}",
        members_of(&env, &i)
    );
    // Drop-side control: a `raise`-terminated else's write still drops —
    // its scope reaches no jump sink.
    let (env, i) = env_of(
        b"x = \"a\"\nwhile rand > 0\n  if rand > 1\n    x = \"s\"\n  else\n    x = 1\n    raise\n  end\nend\n",
    );
    assert!(
        !members_of(&env, &i).iter().any(|m| m.contains("Int(1)")),
        "the raise-terminated write stays dropped, got {:?}",
        members_of(&env, &i)
    );
}

/// rigor-rs#640: the loop-pass stale-write filter's env-polarity judge. A
/// tracked local whose only body write sits in the DEAD arm of an `if` the
/// pass env pins decided (`s = (if d then 2 else w = "x"; 3 end)` on a
/// pinned `d`) keeps its pin through every fixpoint pass — the pass's own
/// replay elides the arm, but the reach-based `dead_positions` never folds
/// an env-pinned carrier inside a loop, so the #437 filter alone let the
/// write spoil the name and decline the whole fixpoint. The convergence is
/// the reference's own `BodyFixpoint` cap answer (forced here by `y = [y]`'s
/// growth): `w` holds `Constant[1]` through the passes and the CAP pass
/// lifts it to the widened class, while the still-moving `y` floors to
/// `Dynamic`.
#[test]
fn loop_pass_env_dead_arm_write_keeps_pin_to_cap_widen() {
    let src = b"c = gets\nd = 1\nw = 1\ny = nil\nwhile c\n  s = (if d then 2 else w = \"x\"; 3 end)\n  y = [y]\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let env = typer.build_toplevel_check_env(&ast, &mut i);
    let integer = typer.nominal_or_untyped("Integer", &mut i);
    assert_eq!(
        env.get("w"),
        Some(&integer),
        "the cap pass widens the stable pin to its class, got {:?}",
        env.get("w").map(|&t| i.get(t))
    );
    let y = env.get("y").copied().expect("y bound");
    assert!(
        matches!(i.get(y), Type::Dynamic(_)),
        "the still-moving name floors at the cap, got {:?}",
        i.get(y)
    );
}

/// rigor-rs#640 boundary (oracle-probed): a LITERAL-decided dead arm keeps
/// the pin through a convergent body (no cap pass: `y` is body-first and
/// settles at pass one), so `w` keeps its `Constant[1]` and `w.zz` fires
/// `for 1` on both engines — the #437 `dead_positions` filter's own row,
/// unchanged by the env judge.
#[test]
fn loop_pass_literal_dead_arm_write_keeps_pin_before_cap() {
    let src = b"w = 1\nc = gets\nwhile c\n  s = (if true then 2 else w = \"x\"; 3 end)\n  y = [y]\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let env = typer.build_toplevel_check_env(&ast, &mut i);
    assert_eq!(
        env.get("w"),
        Some(&i.intern(Type::Constant(Scalar::Int(1)))),
        "a converged body keeps the pin, got {:?}",
        env.get("w").map(|&t| i.get(t))
    );
}

/// rigor-rs#640 boundary: the env judge only excuses a DEAD arm. A write in
/// the LIVE arm of the same write-wrapped `if` is still a shell write the
/// pass's replay does not walk (`s`'s binding replays the VALUE only), so
/// the tracked name stays declined and the structural cap widens it.
/// (Oracle: `for Integer` here — the port's silence is the live-arm
/// write-walk coverage gap, deliberately not this slice's.)
#[test]
fn loop_pass_env_dead_arm_write_live_arm_still_declines() {
    let src = b"c = gets\nd = 1\nw = 1\ny = nil\nwhile c\n  s = (if d then w = 2; 3 else 4 end)\n  y = [y]\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let env = typer.build_toplevel_check_env(&ast, &mut i);
    let w = env.get("w").copied().expect("w bound");
    assert!(
        matches!(i.get(w), Type::Dynamic(_)),
        "a live-arm shell write must keep the decline, got {:?}",
        i.get(w)
    );
}

/// rigor-rs#640 boundary: an UNDECIDED predicate (`c2 = gets`) judges no
/// arm dead, so the arm write still spoils the pass and the structural cap
/// widens the tracked name. (Oracle: `for Integer | String` here — the
/// port's silence is the undecided arm-join coverage gap; the decline is
/// the FP-safe side, and this guard pins it against an over-eager judge.)
#[test]
fn loop_pass_undecided_if_arm_write_still_declines() {
    let src = b"c = gets\nc2 = gets\nw = 1\ny = nil\nwhile c\n  s = (if c2 then 2 else w = \"x\"; 3 end)\n  y = [y]\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let env = typer.build_toplevel_check_env(&ast, &mut i);
    let w = env.get("w").copied().expect("w bound");
    assert!(
        matches!(i.get(w), Type::Dynamic(_)),
        "an undecided arm write must keep the decline, got {:?}",
        i.get(w)
    );
}

/// rigor-rs#151 / #153: a `for` index and a write in a recovery carrier
/// widen; a write under `defined?`, `END`, `BEGIN` or `super(…)` neither
/// binds nor widens; a `for` whose index is not a local, and a `for` over
/// another local, leave the local alone.
#[test]
fn toplevel_check_env_for_index_and_carrier_writes() {
    let ast = lower_src(
        b"f = \"s\"\nfor f in [1]; end\n\
          a = \"s\"\nb = \"s\"\nfor a, (b, *c) in [[1, [2]]]; end\n\
          r = \"s\"\n(r = 1) rescue nil\n\
          d = \"s\"\ndefined?(d = 1)\n\
          e = \"s\"\nEND { e = 1 }\n\
          g = \"s\"\nBEGIN { g = 1 }\n\
          s = \"s\"\nsuper(s = 1)\n\
          k = \"s\"\nfor i in [1]; end\nfor @k in [1]; end\n",
    );
    let mut i = Interner::new();
    let empty = CoreIndex::new();
    let env = Typer::new(&empty).build_toplevel_check_env(&ast, &mut i);
    let get = |name: &str, i: &Interner| env.get(name).map(|&t| i.get(t).clone());
    let untyped = i.untyped();
    let s = Some(Type::Constant(Scalar::Str("s".into())));
    for widened in ["f", "a", "b", "r"] {
        assert_eq!(env.get(widened), Some(&untyped), "{widened}");
    }
    for kept in ["d", "e", "g", "s", "k"] {
        assert_eq!(get(kept, &i), s, "{kept}");
    }
    // The flat env (`type-of`, and the rules' `gate_at`) skips an inert
    // write too; it still binds a recovery carrier's write as before.
    let flat = build_toplevel_env(&ast, &mut i);
    let flat_get = |name: &str, i: &Interner| flat.get(name).map(|&t| i.get(t).clone());
    for kept in ["d", "e", "g", "s"] {
        assert_eq!(flat_get(kept, &i), s, "flat {kept}");
    }
    assert_eq!(flat_get("r", &i), Some(Type::Constant(Scalar::Int(1))));
}

/// rigor-rs#774: the element-read half applies on straight-line code too —
/// a `local[k].<mutator>` rebuilds `local`'s literal-shape carrier through
/// `k` (`element_read_widening.rb`), where the old element-rebind mark
/// floored the whole binding to `Dynamic`.
#[test]
fn element_read_mutations_rebuild_the_carrier() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let env_of = |src: &[u8]| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        (env, i)
    };
    let elem_of = |i: &Interner, ty: TypeId| -> Option<TypeId> {
        match i.get(ty) {
            Type::HashShape(ms) => ms.first().map(|m| m.value),
            Type::Tuple(es) => es.first().copied(),
            _ => None,
        }
    };

    // `h = {a: []}; h[:a] << 1` — only `:a`'s element widens.
    let (env, i) = env_of(b"h = {a: []}\nh[:a] << 1\nh.w\n");
    let h = env.get("h").copied().expect("h bound");
    let elem = elem_of(&i, h).expect("a hash-shape element");
    let a = i.get(elem).clone();
    let Type::Nominal { args, .. } = a else {
        panic!("expected Array nominal, got {a:?}")
    };
    assert_eq!(args.len(), 1, "Array[Dynamic | Integer] has one arg");
    assert_eq!(
        index.class_name_of(&i, elem),
        Some("Array")
    );

    // `h = {a: \"s\"}; h[:a] << \"t\"` — a String element under a String
    // mutator loses its pin to `String`.
    let (env, i) = env_of(b"h = {a: \"s\"}\nh[:a] << \"t\"\nh.w\n");
    let elem = elem_of(&i, *env.get("h").unwrap()).unwrap();
    assert_eq!(index.class_name_of(&i, elem), Some("String"));

    // `a = [[1], [2]]; a[0] << 5` — only the selected Tuple position
    // rebuilds; `[2]` stays pinned.
    let (env, i) = env_of(b"a = [[1], [2]]\na[0] << 5\na.w\n");
    let Type::Tuple(es) = i.get(*env.get("a").unwrap()).clone() else {
        panic!("expected a Tuple, got {:?}", i.get(*env.get("a").unwrap()))
    };
    assert_eq!(es.len(), 2);
    assert_eq!(index.class_name_of(&i, es[0]), Some("Array"));
    let Type::Tuple(inner) = i.get(es[1]).clone() else {
        panic!("the untouched position keeps its Tuple, got {:?}", i.get(es[1]))
    };
    assert!(matches!(i.get(inner[0]), Type::Constant(Scalar::Int(2))));

    // An element whose widening declines stands whole (`Integer#<<`
    // mutates nothing — the reference's `widen_through_path` nil).
    let (env, mut i) = env_of(b"a = [1]\na[0] << 1\na.w\n");
    let one = i.intern(Type::Constant(Scalar::Int(1)));
    let tup = i.intern(Type::Tuple(vec![one]));
    assert_eq!(env.get("a"), Some(&tup));
    let (env, i) = env_of(b"h = {a: 1}\nh[:a] << 1\nh.w\n");
    let elem = elem_of(&i, *env.get("h").unwrap()).unwrap();
    assert!(matches!(i.get(elem), Type::Constant(Scalar::Int(1))));

    // A conditional element mutation keeps the entry binding — the
    // rebind-join treatment master's own table gave it (`if c; a[0] << 2;
    // end` reads `[1]` on the port AND on master; the oracle declines the
    // join outright — a pre-existing divergence this slice keeps).
    let (env, mut i) = env_of(b"a = [1]\nif c\n  a[0] << 2\nend\na.w\n");
    let one = i.intern(Type::Constant(Scalar::Int(1)));
    let tup = i.intern(Type::Tuple(vec![one]));
    assert_eq!(env.get("a"), Some(&tup));
}

/// rigor-rs#943: `ElementReadWidening.widen_through_path` over every
/// element-read chain — chained reads, `first`/`last`/`dig` steps,
/// non-literal keys, block mutators — rebuilds the root local's carrier
/// (each expected type is the oracle's own `x.w` message); a chain that may
/// not have run floors the binding instead of keeping the stale literal.
#[test]
fn element_read_paths_rebuild_through_the_chain() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let show = |src: &str, name: &str| -> String {
        let ast = lower_src(src.as_bytes());
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        let ty = *env.get(name).expect("bound");
        match i.get(ty) {
            Type::Dynamic(_) => "untyped".to_string(),
            _ => rigor_types::describe_named(&i, ty, &|c| {
                index.class_name_for_id(c).map(str::to_string)
            }),
        }
    };
    for (src, want) in [
        ("b = [[[]]]\nb[0][0] << 1\n", "[[Array[Dynamic[top] | Integer]]]"),
        ("h = {a: [[1]]}\nh[:a][0] << 2\n", "{ a: [Array[1 | Dynamic[top] | Integer]] }"),
        ("h = {a: [1]}\nh[:a][0] << 2\n", "{ a: [1] }"),
        ("a = [[1]]\na.first << 1\n", "[Array[1 | Dynamic[top] | Integer]]"),
        ("a = [[1], [2]]\na.last << 3\n", "[[1], Array[2 | Dynamic[top] | Integer]]"),
        ("h = {a: [[]]}\nh.dig(:a, 0) << 1\n", "{ a: [Array[Dynamic[top] | Integer]] }"),
        ("h = {a: {b: []}}\nh[:a][:b] << 1\n", "{ a: { b: Array[Dynamic[top] | Integer] } }"),
        // `:all` — a non-literal key widens every position / pair.
        ("h = {a: [], b: \"s\"}\nk = :a\nh[k] << 1\n", "{ a: Array[Dynamic[top] | Integer], b: String }"),
        ("h = {a: []}\nh[true] << 1\n", "{ a: Array[Dynamic[top] | Integer] }"),
        (
            "a = [[1], [2]]\ni = 0\na[i] << 3\n",
            "[Array[1 | Dynamic[top] | Integer], Array[2 | Dynamic[top] | Integer]]",
        ),
        // A position names no hash key; a Bignum names no tuple position.
        ("h = {a: []}\nh[1] << 1\n", "{ a: [] }"),
        ("a = [[1]]\na[10000000000000000000000] << 2\n", "[[1]]"),
        // The block is on the mutator, not on a read step.
        ("a = [[1]]\na[0].map! { |i| i.to_s }\n", "[Array[Dynamic[top]]]"),
        // A literal-target `send` mutates as the call it names.
        ("b = [[[]]]\nb[0][0].send(:<<, 1)\n", "[[Array[Dynamic[top] | Integer]]]"),
        ("a = [[1]]\na.first.public_send(:push, 2)\n", "[Array[1 | Dynamic[top] | Integer]]"),
        // Two mutators of one carrier compose in source order, slot table
        // and chain table alike.
        ("h = {a: [[]]}\nh[:a][0] << 2\nh[:a] << 1\n", "{ a: Array[Array[Dynamic[top] | Integer] | Dynamic[top]] }"),
        ("h = {a: [[]]}\nh[:a] << 1\nh[:a][0] << 2\n", "{ a: Array[Dynamic[top] | []] }"),
        // Positions the mutation may not have reached floor the binding:
        // a `rescue` modifier (the reference's rescue join is silent), an
        // undecided arm whose rebuild would change the carrier, a block
        // body's `break` argument.
        ("b = [[[]]]\nb[0][0] << 1 rescue nil\n", "untyped"),
        ("h = {a: []}\nh[:a] << 1 rescue nil\n", "untyped"),
        ("b = [[[]]]\nif rand > 0\n  b[0][0] << 1\nend\n", "untyped"),
        ("h = {a: \"s\"}\nif rand > 0\n  h[:a] << \"b\"\nend\n", "untyped"),
        ("b = [[[]]]\n[1].each { break b[0][0] << 1 }\n", "untyped"),
        // …unless the rebuild declines: `Integer#<<` mutates nothing, so
        // the reference keeps the shape (rigor-rs#407's row).
        ("h = {a: 1}\n[1].each { break h[:a] << 2 }\n", "{ a: 1 }"),
        // …but a splat evaluates its operand, and an undecided arm's
        // declined rebuild leaves the entry standing (`Integer#<<`).
        ("b = [[[]]]\np(*(b[0][0] << 1))\n", "[[Array[Dynamic[top] | Integer]]]"),
        ("a = [1]\nif rand > 0\n  a.first << 2\nend\n", "[1]"),
        // A nominal root under a block `break` stands on both sides.
        ("h = {}\nh[:a] ||= \"s\"\n[1].each { break h[:a] << \"t\" }\n", "Hash[Dynamic[top] | Symbol, Dynamic[top] | String]"),
    ] {
        assert_eq!(show(src, src.split(' ').next().unwrap()), want, "{src:?}");
    }
}

/// rigor-rs#944: an operator write the constant fold cannot answer
/// dispatches `a.<op>(v)` on the entry (`dispatch_operator`) on the rows the
/// reference cannot fold, and keeps the decline everywhere else. Every
/// expected value is the oracle's own answer.
#[test]
fn op_write_dispatch_rebinds_the_reference_answer() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let show = |src: &[u8], name: &str| -> String {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        let ty = *env.get(name).expect("bound");
        match i.get(ty) {
            Type::Dynamic(_) => "untyped".to_string(),
            _ => rigor_types::describe_named(&i, ty, &|c| {
                index.class_name_for_id(c).map(str::to_string)
            }),
        }
    };
    for (src, want) in [
        // A raising operand falls to the RBS answer.
        (&b"s = \"abc\"\ns += nil\n"[..], "String"),
        (b"s = \"abc\"\ns += 2\n", "String"),
        (b"s = \"abc\"\ns *= nil\n", "String"),
        (b"x = 1\nx <<= nil\n", "Integer"),
        // `String#+` answers `String` for an untyped operand.
        (b"s = \"abc\"\ns += unresolved\n", "String"),
        // A plain nominal entry has no literal to fold.
        (b"s = \"abc\"\ns << \"d\"\ns *= 2\n", "String"),
        (b"s = \"abc\"\ns << \"d\"\ns *= 0\n", "\"\""),
        (b"x = rand\nx %= 1\n", "Float"),
        // The Tuple shape folds.
        (b"a = [1]\na += [2]\n", "[1, 2]"),
        (b"a = [1]\na *= 2\n", "[1, 1]"),
        (b"a = [1]\na -= [1]\n", "[]"),
        // Declines: the reference folds what the port cannot (`1 << 2.0`
        // is `4`, `"abc" * 2.0` is `"abcabc"`), a union entry never takes
        // the plain call's non-nil retry, `String#<<` turns on literalness,
        // and an erased `Array` would drop the reference's type arguments.
        (b"x = 1\nx <<= 2.0\n", "untyped"),
        (b"s = \"abc\"\ns *= 2.0\n", "untyped"),
        (b"s = \"abc\"\ns %= [2]\n", "untyped"),
        (b"x = nil\nx = 1 if c\nx += 1\n", "untyped"),
        (b"s = \"abc\"\ns <<= 1.5.to_s\n", "untyped"),
        (b"a = [1]\na += unresolved\n", "untyped"),
        (b"a = [1]\na <<= 2\n", "untyped"),
        // `tuple_concat` past 1,024 elements declines rather than
        // materialising a doubling chain (2^11 elements here).
        (b"a = [1]\na += a\na += a\na += a\na += a\na += a\na += a\na += a\na += a\na += a\na += a\na += a\n", "untyped"),
    ] {
        let name = if src.starts_with(b"a") { "a" } else if src.starts_with(b"x") { "x" } else { "s" };
        assert_eq!(show(src, name), want, "{}", String::from_utf8_lossy(src));
    }
}

/// rigor-rs#774: a certainty-folded `if`'s LIVE arm is straight-line code
/// to the check env, so a content mutation inside it lands the reference's
/// own widening — a nominal (`String`, `Hash[…]`) or a keep — where the
/// pre-change path rooted every arm mutation at the `if` and floored the
/// binding to `Dynamic` (every expected value is the oracle's own answer).
#[test]
fn folded_if_arm_mutations_land_the_reference_answer() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let env_of = |src: &[u8]| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        (env, i)
    };

    // `x << "c"` on the rebound pin widens to the `String` nominal.
    let (env, i) = env_of(b"x = \"a\"\nif true\n  x = \"s\"\n  x << \"c\"\nend\nx.w\n");
    assert_eq!(
        index.class_name_of(&i, *env.get("x").unwrap()),
        Some("String")
    );

    // `s += "e"` on the widened `String` answers `non-empty-string` — the
    // plain-call `String + non-empty` projection the op-write expands to.
    let (env, mut i) = env_of(b"if true\n  s = \"abc\"\n  s << \"d\"\n  s += \"e\"\nend\ns.w\n");
    let nes = typer.non_empty_string(&mut i).expect("String registered");
    assert_eq!(env.get("s"), Some(&nes));

    // `s ||= "e"` / `s &&= "e"` fold against the widened `String`, not
    // against `Dynamic` — the joins `"e" | String` and `"e"`.
    let (env, mut i) = env_of(b"s = \"abc\"\nif true\n  s << \"d\"\n  s ||= \"e\"\nend\ns.w\n");
    let e = i.intern(Type::Constant(Scalar::Str("e".into())));
    let string = typer.nominal_or_untyped("String", &mut i);
    let want = i.intern(Type::Union(vec![e, string]));
    assert_eq!(env.get("s"), Some(&want));
    let (env, mut i) = env_of(b"s = \"abc\"\nif true\n  s << \"d\"\n  s &&= \"e\"\nend\ns.w\n");
    let e = i.intern(Type::Constant(Scalar::Str("e".into())));
    assert_eq!(env.get("s"), Some(&e));

    // A widening that declines keeps the binding it saw (`Integer#<<`
    // mutates nothing): `x, y = 1, \"s\"; x << \"b\"` keeps `1`.
    let (env, mut i) = env_of(b"x = \"a\"\nif true\n  x, y = 1, \"s\"\n  x << \"b\"\nend\nx.w\n");
    let one = i.intern(Type::Constant(Scalar::Int(1)));
    assert_eq!(env.get("x"), Some(&one));

    // `h[:a] = 1` rebuilds the `{}` seed's nominal with the store's
    // content join — the same `Hash[Dynamic | Symbol, Dynamic | Integer]`
    // a straight-line `h[:a] = 1` mints.
    let (env, i) = env_of(b"h = {}\nif true\n  h[:a] = 1\nend\nh.w\n");
    let h = i.get(*env.get("h").unwrap()).clone();
    let Type::Nominal { args, .. } = h else {
        panic!("expected a Hash nominal, got {h:?}")
    };
    assert_eq!(args.len(), 2);
    assert_eq!(
        index.class_name_of(&i, *env.get("h").unwrap()),
        Some("Hash")
    );

    // `h[:a] << 1` rebuilds THROUGH the literal key
    // (`ElementReadWidening.widen_element_read`): the shape survives with
    // only `:a`'s element widened.
    let (env, i) = env_of(b"h = {a: []}\nif true\n  h[:a] << 1\nend\nh.w\n");
    let Type::HashShape(ms) = i.get(*env.get("h").unwrap()).clone() else {
        panic!("expected a HashShape, got {:?}", i.get(*env.get("h").unwrap()))
    };
    assert_eq!(ms.len(), 1);
    assert_eq!(index.class_name_of(&i, ms[0].value), Some("Array"));

    // A mutation in a NESTED undecided arm of the live arm still floors:
    // only straight-line positions in the live arm are unconditional.
    let (env, i) = env_of(b"x = \"a\"\nif true\n  if c\n    x << \"b\"\n  end\nend\nx.w\n");
    assert!(matches!(env.get("x").map(|&t| i.get(t)), Some(Type::Dynamic(_))));
}

/// rigor-rs#942: the folded-live-arm landing reaches through the carriers
/// `live_branch_for_if` evaluates as the arm itself — an `else` clause, a
/// `(…; …)` group, a clause-less `begin`, a folded nested `if` — and the
/// terminated-arm scope drop fires only where `eval_if`'s own gate does
/// (`branch_terminates?(statements) && subsequent.nil?`), not on a live
/// else arm or beside any else clause. Every expected value is the
/// oracle's own answer.
#[test]
fn folded_if_else_and_nested_arm_mutations_land() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let env_of = |src: &[u8]| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        (env, i)
    };
    let a_literal = |i: &mut Interner| i.intern(Type::Constant(Scalar::Str("a".into())));

    // The else arm of a falsey fold lands its mutations: the clause is a
    // clause-less `BeginRescue` carrier whose body is straight-line code.
    let (env, i) = env_of(b"x = \"a\"\nif false\n  y = 1\nelse\n  x << \"b\"\nend\nx.w\n");
    assert_eq!(index.class_name_of(&i, *env.get("x").unwrap()), Some("String"));

    // The same carrier landing widens a slot mutation's shape the way a
    // straight-line `h[:a] << 1` does.
    let (env, i) = env_of(b"h = {a: []}\nif false\nelse\n  h[:a] << 1\nend\nh.w\n");
    let Type::HashShape(ms) = i.get(*env.get("h").unwrap()).clone() else {
        panic!("expected a HashShape, got {:?}", i.get(*env.get("h").unwrap()))
    };
    assert_eq!(ms.len(), 1);
    assert_eq!(index.class_name_of(&i, ms[0].value), Some("Array"));

    // A folded `if` nested in the live arm lands its own live-arm
    // mutation — the inner arm roots it, not the outer `if`.
    let (env, i) = env_of(b"x = \"a\"\nif true\n  if true\n    x << \"b\"\n  end\nend\nx.w\n");
    assert_eq!(index.class_name_of(&i, *env.get("x").unwrap()), Some("String"));

    // A clause-less `begin` and a paren group in the live arm run their
    // bodies unconditionally too.
    let (env, i) = env_of(b"x = \"a\"\nif true\n  begin\n    x << \"b\"\n  end\nend\nx.w\n");
    assert_eq!(index.class_name_of(&i, *env.get("x").unwrap()), Some("String"));
    let (env, i) = env_of(b"x = \"a\"\nif true\n  (x << \"b\"\n   y = 1)\nend\nx.w\n");
    assert_eq!(index.class_name_of(&i, *env.get("x").unwrap()), Some("String"));

    // The terminated-then-no-else gate: the arm's writes and mutations
    // drop with its scope (`branch_terminates? && subsequent.nil?`).
    let (env, mut i) = env_of(b"x = \"a\"\nif true\n  x = \"s\"\n  raise\nend\nx.w\n");
    let a = a_literal(&mut i);
    assert_eq!(env.get("x"), Some(&a));
    let (env, mut i) = env_of(b"x = \"a\"\nif true\n  x << \"b\"\n  raise\nend\nx.w\n");
    let a = a_literal(&mut i);
    assert_eq!(env.get("x"), Some(&a));
    let (env, mut i) = env_of(b"x = \"a\"\nunless false\n  x = \"s\"\n  raise\nend\nx.w\n");
    let a = a_literal(&mut i);
    assert_eq!(env.get("x"), Some(&a));

    // …but only there: a live else arm that exits keeps its scope (the
    // `subsequent`/`else_clause` is present), and so does a terminated
    // then arm beside an else — even an empty `else; end`.
    let (env, i) = env_of(b"x = \"a\"\nif false\nelse\n  x << \"b\"\n  raise\nend\nx.w\n");
    assert_eq!(index.class_name_of(&i, *env.get("x").unwrap()), Some("String"));
    let (env, i) = env_of(b"x = \"a\"\nif true\n  x << \"b\"\n  raise\nelse\n  y = 1\nend\nx.w\n");
    assert_eq!(index.class_name_of(&i, *env.get("x").unwrap()), Some("String"));
    let (env, mut i) = env_of(b"x = \"a\"\nif true\n  x = \"s\"\n  raise\nelse\nend\nx.w\n");
    let s = i.intern(Type::Constant(Scalar::Str("s".into())));
    assert_eq!(env.get("x"), Some(&s));
}

/// rigor-rs#942 review — `stmt_terminates` recurses the shapes
/// `branch_terminates?`'s Bot half answers (an `if`/`unless` whose both
/// arms exit, a `case`/`case…in` whose every arm exits, a `begin`/`rescue`
/// whose body AND every clause exit, `exit!`'s `bot` signature) — and the
/// undecided `if_join` applies the STRICT `branch_certainly_exits?` to the
/// body arm, which a `begin`/`rescue` fails. Every expected value is the
/// oracle's own answer.
#[test]
fn terminating_arm_drops_recognise_nested_exits() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let env_of = |src: &[u8]| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        (env, i)
    };
    let a_literal = |i: &mut Interner| i.intern(Type::Constant(Scalar::Str("a".into())));
    let s_literal = |i: &mut Interner| i.intern(Type::Constant(Scalar::Str("s".into())));

    // Folded arm: a nested `if`/`case`/`begin` whose every path exits still
    // drops the live arm's scope.
    for src in [
        b"x = \"a\"\nif true\n  x = \"s\"\n  if rand > 0\n    raise\n  else\n    raise\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  case rand\n  when Integer\n    raise\n  else\n    raise\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  case rand\n  in Integer\n    raise\n  else\n    raise\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  begin\n    raise\n  rescue\n    raise\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  exit!\nend\nx.w\n".as_ref(),
    ] {
        let (env, mut i) = env_of(src);
        let a = a_literal(&mut i);
        assert_eq!(env.get("x"), Some(&a), "{}", String::from_utf8_lossy(src));
    }

    // Undecided arm: a body arm `branch_certainly_exits?` beside an else
    // contributes the else's scope alone (`if c; if d; raise; else; raise;
    // end; else; x = "s"` → `"s"`).
    let (env, mut i) = env_of(b"x = \"a\"\nif rand > 0\n  if rand > 1\n    raise\n  else\n    raise\n  end\nelse\n  x = \"s\"\nend\nx.w\n");
    let s = s_literal(&mut i);
    assert_eq!(env.get("x"), Some(&s));

    // …but a `begin`/`rescue` arm is NOT `branch_certainly_exits?` — its
    // scope joins (`"a" | "s"` on the oracle).
    let (env, mut i) = env_of(b"x = \"a\"\nif rand > 0\n  begin\n    raise\n  rescue\n    raise\n  end\nelse\n  x = \"s\"\nend\nx.w\n");
    let (a, s) = (a_literal(&mut i), s_literal(&mut i));
    let want = i.intern(Type::Union(vec![a, s]));
    assert_eq!(env.get("x"), Some(&want));

    // An else arm `branch_terminates?` DOES see the begin's all-exit type —
    // the body's scope survives alone (`"s"`).
    let (env, mut i) = env_of(b"x = \"a\"\nif rand > 0\n  x = \"s\"\nelse\n  begin\n    raise\n  rescue\n    raise\n  end\nend\nx.w\n");
    let s = s_literal(&mut i);
    assert_eq!(env.get("x"), Some(&s));
}

/// rigor-rs#955 — the check flow's branch joins see the same `retry`-bot
/// termination the oracle's `live_rescue_results` / `eval_if` see: an arm
/// ending in `retry` contributes no scope, while a conditional retry keeps
/// the entry edge joined. Expected values are the oracle's
/// `undefined method` witnesses.
#[test]
fn retry_bot_termination_reaches_the_if_scope_join() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let x_of = |src: &str| {
        let ast = lower_src(src.as_bytes());
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        rigor_types::describe(&i, *env.get("x").expect(src))
            .replace("Constant[", "")
            .replace(']', "")
    };

    // The issue's undecided-`else` row: the retrying rescue arm terminates
    // the begin, so only the body arm's write reaches the continuation.
    assert_eq!(
        x_of("x = \"a\"; if rand > 0; x = \"s\"; else; begin; raise; rescue; retry; end; end; x.w"),
        "\"s\""
    );
    // The folded-arm row: the whole live arm ends in the `bot` begin, so
    // its writes drop and the pre-`if` binding survives.
    assert_eq!(
        x_of("x = 1; if true; x = \"s\"; begin; raise; rescue; retry; end; end; x.upcase"),
        "1"
    );
    // `retry` nested in an all-`bot` conditional still terminates the
    // rescue arm.
    assert_eq!(
        x_of("x = 1; if true; x = \"s\"; begin; raise; rescue; if rand > 0; retry; else; raise; end; end; end; x.upcase"),
        "1"
    );
    // …but a conditional retry has a completing edge, so the arm stays
    // live and the entry binding joins.
    assert_eq!(
        x_of("x = \"a\"; if rand > 0; x = \"s\"; else; begin; raise; rescue; retry if rand > 0; end; end; x.w"),
        "\"a\" | \"s\""
    );
}

/// rigor-rs#951 — the undecided `if_join` joins each arm's EXIT scope: an
/// elsif chain or a nested `if` contributes its own arm join, not the
/// lexically-last write in the span, and an op-write folds from the
/// binding the arm built so far. Every expected value is the oracle's own
/// answer (`undefined method 'w' for …`).
#[test]
fn undecided_if_join_reads_each_arm_exit_scope() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let x_of = |src: &str| {
        let ast = lower_src(src.as_bytes());
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        // `describe` spells `Constant["s"]`; the message spells `"s"`.
        rigor_types::describe(&i, *env.get("x").expect(src))
            .replace("Constant[", "")
            .replace(']', "")
    };
    for (src, want) in [
        ("x = \"a\"; if rand > 1; x = \"s\"; elsif rand > 2; x = \"t\"; else; x = \"u\"; end; x.w", r#""s" | "t" | "u""#),
        ("x = \"a\"; if rand > 1; x = \"s\"; elsif rand > 2; x = \"t\"; end; x.w", r#""a" | "s" | "t""#),
        ("x = \"a\"; if rand > 0; if rand > 1; x = \"s\"; elsif rand > 2; x = \"t\"; else; x = \"u\"; end; end; x.w", r#""a" | "s" | "t" | "u""#),
        ("x = \"a\"; if rand > 0; x = \"q\"; if rand > 1; x = \"s\"; end; end; x.w", r#""a" | "q" | "s""#),
        ("x = \"a\"; if rand > 0; unless rand > 1; x = \"s\"; else; x = \"t\"; end; end; x.w", r#""a" | "s" | "t""#),
        ("x = \"a\"; if rand > 1; x = \"s\"; else; if rand > 2; x = \"t\"; end; end; x.w", r#""a" | "s" | "t""#),
        ("x = \"a\"; if rand > 1; rand > 2 ? (x = \"s\") : (x = \"t\"); end; x.w", r#""a" | "s" | "t""#),
        // A folded nested `if` contributes its live arm alone.
        ("x = \"a\"; if rand > 0; if true; x = \"s\"; else; x = \"t\"; end; end; x.w", r#""a" | "s""#),
        // The termination drops apply at every level of the chain.
        ("x = \"a\"; if rand > 1; x = \"s\"; elsif rand > 2; x = \"t\"; raise; end; x.w", r#""a" | "s""#),
        ("x = \"a\"; if rand > 1; raise; elsif rand > 2; x = \"t\"; else; x = \"u\"; end; x.w", r#""t" | "u""#),
        ("x = 1; if rand > 0; x = 2; x += 3; end; x.w", "1 | 5"),
        ("x = 1; if rand > 0; if rand > 1; x += 3; else; x = 7; end; end; x.w", "1 | 4 | 7"),
    ] {
        assert_eq!(x_of(src), want, "{src}");
    }
}

/// rigor-rs#998 — an arm's `&&`/`||` statement joins its two operand
/// scopes per name, a `for` contributes its index's element binding
/// joined with the body's exit (and the zero-iteration edge), and a
/// nested `if` whose PREDICATE writes the name joins its arms from the
/// post-predicate binding rather than keeping the span's last write.
/// Every expected value is the oracle's own answer.
#[test]
fn arm_scope_join_reads_operands_index_and_post_pred() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let x_of = |src: &str| {
        let ast = lower_src(src.as_bytes());
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        // `describe` spells `Constant["s"]`; the message spells `"s"`.
        rigor_types::describe(&i, *env.get("x").expect(src))
            .replace("Constant[", "")
            .replace(']', "")
    };
    for (src, want) in [
        // `&&` / `||` statements join their operand scopes.
        ("x = \"a\"; if rand > 0; (x = \"s\") && (x = \"t\"); end; x.w", r#""a" | "s" | "t""#),
        ("x = \"a\"; if rand > 0; (x = \"s\") || (x = \"t\"); end; x.w", r#""a" | "s" | "t""#),
        ("x = \"a\"; if rand > 0; (x = \"s\") && (x = \"m\") || (x = \"t\"); end; x.w", r#""a" | "m" | "s" | "t""#),
        ("x = \"a\"; if rand > 0; x && (x = \"t\"); end; x.w", r#""a" | "t""#),
        // `for` binds the index to the collection's element, joined with
        // the pre-loop binding (the zero-iteration edge) and read by the
        // body's own arm-scope walk.
        ("x = \"a\"; if rand > 0; for x in [1]; end; end; x.w", r#""a" | 1"#),
        ("x = \"a\"; if rand > 0; x = \"b\"; for x in [1]; end; end; x.w", r#""a" | "b" | 1"#),
        ("x = \"a\"; if rand > 0; for x in [1]; x = \"f\"; end; end; x.w", r#""a" | "f""#),
        ("x = \"a\"; if rand > 0; for x in [1]; if rand > 1; x = \"f\"; end; end; end; x.w", r#""a" | "f" | 1"#),
        // `describe` renders the Integer nominal by class id (`Class<1>`).
        ("x = \"a\"; if rand > 0; for x in 1..3; end; end; x.w", r#"Class<1> | "a""#),
        // rigor-rs#1006 — non-literal range bounds: a write or local endpoint
        // still lifts to the element class (`range_endpoint_element_type`).
        ("x = \"a\"; if rand > 0; for x in (x=1)..(x=3); end; end; x.w", r#"Class<1> | "a" | 3"#),
        ("a=1; b=3; x = \"a\"; if rand > 0; for x in a..b; end; end; x.w", r#"Class<1> | "a""#),
        // `nil..3` is not beginless: the NilNode left drops, the right keeps Integer.
        ("x = \"a\"; if rand > 0; for x in nil..3; end; end; x.w", r#"Class<1> | "a""#),
        // Receiver-call element path: `.each` / `.lazy` yield the receiver's
        // element when the collection itself types untyped.
        ("x = \"a\"; if rand > 0; for x in [1].each; end; end; x.w", r#""a" | 1"#),
        ("x = \"a\"; if rand > 0; for x in { k: 1 }.each; end; end; x.w", r#""a" | Tuple[:k, 1"#),
        ("x = \"a\"; if rand > 0; for x in [1].lazy; end; end; x.w", r#""a" | 1"#),
        // `describe` spells the `[k, v]` pair `Tuple[:k, 1]` (the probe's
        // `]` strip leaves `Tuple[:k, 1`).
        ("x = \"a\"; if rand > 0; for x in { k: 1 }; end; end; x.w", r#""a" | Tuple[:k, 1"#),
        // The zero-iteration edge is the POST-COLLECTION scope: a write
        // inside the collection precedes the iteration decision on every
        // edge, so the pre-`for` binding never resurrects (rigor-rs#998
        // review).
        ("x = \"a\"; if rand > 0; x = \"s\"; for y in (x = [1]); end; end; x.w", r#""a" | Tuple[1"#),
        ("x = \"a\"; if rand > 0; x = \"s\"; for x in (x = [1]); end; end; x.w", r#""a" | 1 | Tuple[1"#),
        // A nested `if` whose predicate writes the name joins its arms
        // from the post-predicate binding.
        ("x = \"a\"; if rand > 0; if (x = rand) > 0; x = \"s\"; else; x = \"t\"; end; end; x.w", r#""a" | "s" | "t""#),
        // The `rand` Float nominal describes as `Class<2>`.
        ("x = \"a\"; if rand > 0; if (x = rand) > 0; x = \"s\"; end; end; x.w", r#"Class<2> | "a" | "s""#),
        ("x = \"a\"; if rand > 0; if (x = 1) > 0; x = \"s\"; else; x = \"t\"; end; end; x.w", r#""a" | "s""#),
        ("x = \"a\"; y = \"b\"; if rand > 0; if (x, y = rand, \"p\"); x = \"s\"; else; x = \"t\"; end; end; x.w", r#""a" | "s""#),
        // The same post-predicate entry applies to a top-level undecided
        // `if`, not only an arm's nested one.
        ("x = \"a\"; if (x = rand) > 0; x = \"s\"; end; x.w", r#"Class<2> | "s""#),
    ] {
        assert_eq!(x_of(src), want, "{src}");
    }
}

/// rigor-rs#1042 — a statically folded mixed-type Range iterates its
/// FIRST endpoint's class (`constant_element_type` reads `first.class`),
/// and a receiver-call `each`/`lazy` collection keeps the element through
/// the chain shapes the reference's block-param dispatch answers. Every
/// expected value is the oracle's own `x.w` receiver message.
#[test]
fn for_element_mixed_ranges_and_receiver_call_chains() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let x_of = |src: &str| {
        let ast = lower_src(src.as_bytes());
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        let ty = *env.get("x").expect(src);
        rigor_types::describe_named(&i, ty, &|c| {
            index.class_name_for_id(c).map(str::to_string)
        })
    };
    for (src, want) in [
        // `constant_element_type` on the folded Constant range reads
        // `first.class`; it does not union the two endpoint classes.
        ("x=\"a\"; if rand > 0; for x in 1..3.5; end; end; x.w", "\"a\" | Integer"),
        ("x=\"a\"; if rand > 0; for x in 1.5..3; end; end; x.w", "\"a\" | Float"),
        ("x=\"a\"; if rand > 0; for x in 1.0..3.0; end; end; x.w", "\"a\" | Float"),
        ("a=1.5; x=\"a\"; if rand > 0; for x in a..3; end; end; x.w", "\"a\" | Float"),
        // An invalid mixed range never folds to a Constant, so the nominal
        // endpoint union still applies.
        ("x=\"a\"; if rand > 0; for x in 1..\"z\"; end; end; x.w", "\"a\" | Integer | String"),
        // Receiver-call chains: pure self-returners keep the shape; an
        // enumerator's own no-block `each` returns `self`, whose type args
        // widen; `lazy` constructs a fresh lazy enumerator without that
        // self-return widening.
        ("x=\"a\"; if rand > 0; for x in [1].dup.each; end; end; x.w", "\"a\" | 1"),
        ("x=\"a\"; if rand > 0; for x in [1].freeze.each; end; end; x.w", "\"a\" | 1"),
        ("x=\"a\"; if rand > 0; for x in [1].each.lazy; end; end; x.w", "\"a\" | 1"),
        ("x=\"a\"; if rand > 0; for x in [1].lazy.lazy; end; end; x.w", "\"a\" | 1"),
        ("x=\"a\"; if rand > 0; for x in [1].lazy.each; end; end; x.w", "\"a\" | Integer"),
        ("x=\"a\"; if rand > 0; for x in [1].each.each; end; end; x.w", "\"a\" | Integer"),
        ("x=\"a\"; if rand > 0; for x in [1].each.lazy.each; end; end; x.w", "\"a\" | Integer"),
        // Hash iteration yields its pair. `each_pair` is hash-only, and a
        // second enumerator `each` deep-widens the pair type argument to an
        // Array, exactly as `SelfSubstitute.deep_widen` does.
        ("x=\"a\"; if rand > 0; for x in {k:1}.each_pair; end; end; x.w", "\"a\" | [:k, 1]"),
        (
            "x=\"a\"; if rand > 0; for x in {k:1}.each_pair.each; end; end; x.w",
            "\"a\" | Array[Integer | Symbol]",
        ),
        // Arity is diagnosed independently; the fallback block-param answer
        // still types the element. A block form returns the receiver `self`,
        // so the shape's pins widen to their classes.
        ("x=\"a\"; if rand > 0; for x in [1].each(1); end; end; x.w", "\"a\" | 1"),
        ("x=\"a\"; if rand > 0; for x in [1].each { |y| y }; end; end; x.w", "\"a\" | Integer"),
        (
            "x=\"a\"; if rand > 0; for x in {k:1}.each { |y| y }; end; end; x.w",
            "\"a\" | [Symbol, Integer]",
        ),
    ] {
        assert_eq!(x_of(src), want, "{src}");
    }
}

/// rigor-rs#952 — the `else` clause of a `begin`/`rescue` joins its type
/// unconditionally: a non-exiting `else` (even unreachable behind an
/// exiting body, and even an EMPTY `else; end` — `nil` either way) keeps
/// the begin non-`bot`, and an exiting `else` takes over the protected
/// body's value path (the body itself need not exit). Every expected
/// value is the oracle's own answer.
#[test]
fn terminating_begin_else_joins_its_value() {
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let env_of = |src: &[u8]| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_check_env(&ast, &mut i);
        (env, i)
    };
    let a_literal = |i: &mut Interner| i.intern(Type::Constant(Scalar::Str("a".into())));
    let s_literal = |i: &mut Interner| i.intern(Type::Constant(Scalar::Str("s".into())));

    // Folded arm: a non-exiting or empty `else` keeps the live arm's scope
    // (`"s"` survives; the begin is not `bot`). The `unless false` fold and
    // a two-clause rescue chain behave the same, as does `ensure` — it runs
    // on every path but contributes no value.
    for src in [
        b"x = \"a\"\nif true\n  x = \"s\"\n  begin\n    raise\n  rescue\n    raise\n  else\n    nil\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nunless false\n  x = \"s\"\n  begin\n    raise\n  rescue\n    raise\n  else\n    nil\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  begin\n    raise\n  rescue\n    raise\n  else\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  begin\n    raise\n  rescue E1\n    raise\n  rescue E2\n    raise\n  else\n    nil\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  begin\n    raise\n  rescue\n    raise\n  else\n    nil\n  ensure\n    y = 1\n  end\nend\nx.w\n".as_ref(),
    ] {
        let (env, mut i) = env_of(src);
        let s = s_literal(&mut i);
        assert_eq!(env.get("x"), Some(&s), "{}", String::from_utf8_lossy(src));
    }

    // Folded arm: an exiting `else` leaves the begin `bot` — the arm drops
    // even when the protected body itself would not exit (`else` replaces
    // the body's value path), and `ensure` still counts for nothing.
    for src in [
        b"x = \"a\"\nif true\n  x = \"s\"\n  begin\n    nil\n  rescue\n    raise\n  else\n    raise\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif true\n  x = \"s\"\n  begin\n    raise\n  rescue\n    raise\n  else\n    raise\n  ensure\n    y = 1\n  end\nend\nx.w\n".as_ref(),
    ] {
        let (env, mut i) = env_of(src);
        let a = a_literal(&mut i);
        assert_eq!(env.get("x"), Some(&a), "{}", String::from_utf8_lossy(src));
    }

    // Undecided arm: the non-`bot` begin keeps its arm's scope either side
    // of the `if` — `"a" | "s"`.
    for src in [
        b"x = \"a\"\nif rand > 0\n  x = \"s\"\nelse\n  begin\n    raise\n  rescue\n    raise\n  else\n    nil\n  end\nend\nx.w\n".as_ref(),
        b"x = \"a\"\nif rand > 0\n  x = \"s\"\n  begin\n    raise\n  rescue\n    raise\n  else\n    nil\n  end\nend\nx.w\n".as_ref(),
    ] {
        let (env, mut i) = env_of(src);
        let (a, s) = (a_literal(&mut i), s_literal(&mut i));
        let want = i.intern(Type::Union(vec![a, s]));
        assert_eq!(env.get("x"), Some(&want), "{}", String::from_utf8_lossy(src));
    }

    // …while an exiting `else` drops the undecided arm exactly like no
    // `else` at all: the else arm's scope gone (`"s"`), or the body arm's
    // (`"a"` — the body arm need not exit itself).
    let (env, mut i) = env_of(b"x = \"a\"\nif rand > 0\n  x = \"s\"\nelse\n  begin\n    raise\n  rescue\n    raise\n  else\n    raise\n  end\nend\nx.w\n");
    let s = s_literal(&mut i);
    assert_eq!(env.get("x"), Some(&s));
    let (env, mut i) = env_of(b"x = \"a\"\nif rand > 0\n  x = \"s\"\n  begin\n    nil\n  rescue\n    raise\n  else\n    raise\n  end\nend\nx.w\n");
    let a = a_literal(&mut i);
    assert_eq!(env.get("x"), Some(&a));
}

/// rigor-rs#153: the always-truthy constant propagation folds past a write
/// that never runs in sequence, and declines past a conditional one.
#[test]
fn always_truthy_snapshots_skip_inert_and_widen_recovered_writes() {
    let empty = CoreIndex::new();
    let typer = Typer::new(&empty);
    let fold = |src: &[u8]| {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let snaps = typer.always_truthy_snapshots(&ast, &mut i);
        let (&_, &t) = snaps.iter().next().expect("one predicate");
        i.get(t).clone()
    };
    assert_eq!(fold(b"w = 1\nEND { w = nil }\nif w\n  1\nend\n"), Type::Constant(Scalar::Int(1)));
    assert_eq!(fold(b"w = nil\ndefined?(w = 1)\nif w\n  1\nend\n"), Type::Constant(Scalar::Nil));
    assert!(matches!(fold(b"w = nil\n(w = 1) rescue nil\nif w\n  1\nend\n"), Type::Dynamic(_)));
    assert!(matches!(fold(b"w = nil\nfor w in [1]; end\nif w\n  1\nend\n"), Type::Dynamic(_)));
}

#[test]
fn unknown_receiver_is_dynamic_top() {
    // In Ruby, a bare `x` with no prior assignment parses as the
    // implicit-self call `x()`, so the receiver of `.foo` is a `Call`, not
    // a local read. Either way, an unknown carrier types as Dynamic[top],
    // which is what keeps the call rule silent (ADR-0023 tier-5).
    let ast = lower_src(b"x.foo\n");
    let mut i = Interner::new();
    let env = build_toplevel_env(&ast, &mut i);
    // The receiver node of the outer `.foo` call.
    let recv_id = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::Call { receiver: Some(r), method, .. } if method == "foo" => Some(*r),
            _ => None,
        })
        .unwrap();
    let ty = type_of(&ast, recv_id, &env, &mut i);
    assert_eq!(ty, i.untyped());
}

/// Find the `Call` node whose method matches `name`, returning its id.
fn find_call(ast: &LoweredAst, name: &str) -> NodeId {
    ast.iter()
        .find_map(|(id, n)| match n {
            Node::Call { method, .. } if method == name => Some(id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a call to `{name}`"))
}

#[test]
fn folds_integer_addition_to_constant() {
    // `1 + 2` lowers to a Call `+` on receiver `1` with positional arg `2`;
    // now that args are lowered, binary folding runs and pins Constant[3].
    let ast = lower_src(b"1 + 2\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "+");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(3)));
}

#[test]
fn folds_nullary_integer_succ_to_constant() {
    // Nullary folding still works with the new arg threading.
    let ast = lower_src(b"42.succ\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "succ");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(43)));
}

#[test]
fn typed_literals_pin_constants() {
    let ast = lower_src(b"nil\ntrue\nfalse\n:foo\n2.5\n");
    let mut i = Interner::new();
    let env = TypeEnv::new();
    let pick = |ast: &LoweredAst, pred: fn(&Node) -> bool| {
        ast.iter().find_map(|(id, n)| pred(n).then_some(id)).unwrap()
    };
    let nil = pick(&ast, |n| matches!(n, Node::NilLit { .. }));
    let t = pick(&ast, |n| matches!(n, Node::TrueLit { .. }));
    let f = pick(&ast, |n| matches!(n, Node::FalseLit { .. }));
    let sym = pick(&ast, |n| matches!(n, Node::SymbolLit { .. }));
    let fl = pick(&ast, |n| matches!(n, Node::FloatLit { .. }));
    let ty_of = |i: &mut Interner, id| {
        let t = type_of(&ast, id, &env, i);
        i.get(t).clone()
    };
    assert_eq!(ty_of(&mut i, nil), Type::Constant(Scalar::Nil));
    assert_eq!(ty_of(&mut i, t), Type::Constant(Scalar::Bool(true)));
    assert_eq!(ty_of(&mut i, f), Type::Constant(Scalar::Bool(false)));
    assert_eq!(ty_of(&mut i, sym), Type::Constant(Scalar::Sym("foo".into())));
    assert_eq!(ty_of(&mut i, fl), Type::Constant(Scalar::Float(2.5)));
}

#[test]
fn escaped_symbol_pins_raw_source_spelling() {
    // rigor-rs#292: `symbol_type_for` pins `node.value`, not `unescaped`.
    let ast = lower_src(b":\"a\\nb\"\n");
    let mut i = Interner::new();
    let env = TypeEnv::new();
    let sym = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::SymbolLit { .. }).then_some(id))
        .unwrap();
    let ty = type_of(&ast, sym, &env, &mut i);
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Sym("a\\nb".into())));
}

#[test]
fn non_pinned_argument_declines_folding() {
    // `x` is never assigned -> Dynamic, so `"a" + x` can't fold; the call
    // widens to the nominal String return rather than minting a Constant.
    let ast = lower_src(b"\"a\" + x\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "+");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    // String#+ -> String nominal (return-type path), NOT a folded Constant.
    assert_eq!(idx.class_name_of(&i, ty), Some("String"));
    assert!(!matches!(i.get(ty), Type::Constant(_)));
}

#[test]
fn folds_string_upcase_to_constant() {
    // `"hi".upcase` -> Constant["HI"].
    let ast = lower_src(b"\"hi\".upcase\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "upcase");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("HI".into())));
}

#[test]
fn folds_string_length_to_constant() {
    // `"hello".length` -> Constant[5] (value-pinned; the core folds it).
    let ast = lower_src(b"\"hello\".length\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "length");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(5)));
}

#[test]
fn chained_call_result_types_to_string_nominal() {
    // `s = "Hello"; s.downcase` types to a String Nominal (folding pins the
    // value, but to exercise the return-type path we check the class
    // resolves to "String" via the index regardless). Then `.lenght` on a
    // String would be undefined.
    let ast = lower_src(b"s = \"Hello\"\ns.downcase\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "downcase");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    // Folding pins "hello"; its class still resolves to String, so a later
    // `.lenght` on the result is checkable as undefined.
    assert_eq!(idx.class_name_of(&i, ty), Some("String"));
    assert!(!idx.class_has_method("String", "lenght"));
}

#[test]
fn return_type_resolves_when_receiver_not_folded() {
    // A receiver typed as a (non-constant) String Nominal exercises the
    // return-type table path: `String#downcase -> String`, and that result
    // resolves back to "String" so a chained typo is flagged.
    //
    // `s` must lower to a `LocalVariableRead` (which it does once assigned),
    // so we assign then override the env binding to a bare String Nominal
    // (no value pin) — defeating folding and forcing the return-type path.
    let ast = lower_src(b"s = \"Hello\"\ns.downcase\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut env = typer.build_toplevel_env(&ast, &mut i);
    let string_id = idx.class_id("String").unwrap();
    let recv = i.intern(Type::Nominal { class: string_id, args: vec![] });
    env.insert("s".into(), recv);

    let call = find_call(&ast, "downcase");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    // Not folded (receiver isn't a Constant), so we get the Nominal return.
    assert_eq!(i.get(ty), &Type::Nominal { class: string_id, args: vec![] });
    assert_eq!(idx.class_name_of(&i, ty), Some("String"));
}

/// rigor-rs#775 — `Integer#/` carries divergent overloads (`(Integer) ->
/// Integer | (Float) -> Float | …`), so the flat `method_return` slot holds
/// nothing for a non-constant receiver. The pass-0 selector then picks the
/// first arm whose parameter names the argument's own class: `i / 3` ->
/// `Integer`, `i / 3.0` -> `Float`.
#[test]
fn divergent_overload_set_answers_from_arg_class() {
    let ast = lower_src(b"i = 0\ni / 3\ni / 3.0\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut env = typer.build_toplevel_env(&ast, &mut i);
    let int_id = idx.class_id("Integer").unwrap();
    let float_id = idx.class_id("Float").unwrap();
    env.insert(
        "i".into(),
        i.intern(Type::Nominal { class: int_id, args: vec![] }),
    );

    let calls: Vec<NodeId> = ast
        .iter()
        .filter_map(|(id, n)| match n {
            Node::Call { method, .. } if method == "/" => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    let int_ty = typer.type_of(&ast, calls[0], &env, &mut i);
    assert_eq!(
        i.get(int_ty),
        &Type::Nominal { class: int_id, args: vec![] }
    );
    let float_ty = typer.type_of(&ast, calls[1], &env, &mut i);
    assert_eq!(
        i.get(float_ty),
        &Type::Nominal { class: float_id, args: vec![] }
    );
}

/// When no overload matches at all, the reference's `no_match_fallback`
/// answers the first block-free overload in receiver-affinity order — an
/// overlay-prepped disjoint arm (the bigdecimal reopen's `Integer#/
/// (BigDecimal)`) cannot win by position, so `i / "a"` answers `Integer`.
#[test]
fn unmatched_divergent_overload_fails_soft_to_affinity_first() {
    let ast = lower_src(b"i = 0\ni / \"a\"\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut env = typer.build_toplevel_env(&ast, &mut i);
    let int_id = idx.class_id("Integer").unwrap();
    env.insert(
        "i".into(),
        i.intern(Type::Nominal { class: int_id, args: vec![] }),
    );
    let call = find_call(&ast, "/");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(i.get(ty), &Type::Nominal { class: int_id, args: vec![] });
}

/// Pass 0 requires every engaged param to be a lone class naming the
/// argument's own class. `Float#/` is `(Complex) -> Complex | (Numeric) ->
/// Float`; `f / 3` gradual-matches `(Numeric)` first, but `Numeric` names a
/// superclass, not `Integer` — the port declines (the reference answers it
/// in the strict pass, which is unported) rather than guess `Float`.
#[test]
fn gradual_match_naming_a_superclass_declines() {
    let ast = lower_src(b"f = 0.0\nf / 3\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut env = typer.build_toplevel_env(&ast, &mut i);
    let float_id = idx.class_id("Float").unwrap();
    env.insert(
        "f".into(),
        i.intern(Type::Nominal { class: float_id, args: vec![] }),
    );
    let call = find_call(&ast, "/");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(ty, i.untyped());
}

/// A non-bare `return_form` this slice cannot translate — `Integer#divmod`'s
/// `[Integer, Integer]` tuple — declines even on a proven match.
#[test]
fn non_bare_overload_return_declines() {
    let ast = lower_src(b"i = 0\ni.divmod(3)\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut env = typer.build_toplevel_env(&ast, &mut i);
    let int_id = idx.class_id("Integer").unwrap();
    env.insert(
        "i".into(),
        i.intern(Type::Nominal { class: int_id, args: vec![] }),
    );
    let call = find_call(&ast, "divmod");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(ty, i.untyped());
}

#[test]
fn array_literal_types_to_array_nominal() {
    // `[1, 2]` types to a bare Array Nominal so a typo (`.frist`) is
    // checkable against the real Array RBS.
    let ast = lower_src(b"[1, 2]\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let arr = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::ArrayLit { .. }).then_some(id))
        .unwrap();
    let ty = typer.type_of(&ast, arr, &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("Array"));
    assert!(!idx.class_has_method("Array", "frist"));
}

#[test]
fn interpolated_string_types_to_string_nominal() {
    // `"a#{x}b"` types to a bare String Nominal (a String *instance*), so a
    // typo'd / non-core method on it resolves against the real String RBS.
    let ast = lower_src(b"\"a#{x}b\"\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let interp = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::InterpolatedString { .. }).then_some(id))
        .unwrap();
    let ty = typer.type_of(&ast, interp, &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("String"));
}

#[test]
fn hash_literal_types_to_hash_nominal() {
    let ast = lower_src(b"{ a: 1 }\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let hash = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::HashLit { .. }).then_some(id))
        .unwrap();
    let ty = typer.type_of(&ast, hash, &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));
}

// ---------------------------------------------------------------------
// Scalar-key HashShape (ADR-0038 slice 2). Widened key set, last-wins
// duplicate keys, and the HashShape projection tier.
// ---------------------------------------------------------------------

fn find_hash(ast: &LoweredAst) -> NodeId {
    ast.iter()
        .find_map(|(id, n)| matches!(n, Node::HashLit { .. }).then_some(id))
        .expect("expected a hash literal")
}

fn hash_members(ty: &Type) -> &[ShapeMember] {
    match ty {
        Type::HashShape(m) => m,
        other => panic!("expected HashShape, got {other:?}"),
    }
}

#[test]
fn hash_shape_pins_widened_scalar_keys() {
    // Integer / Float / true / false / nil keys now pin shape slots (the
    // reference's widened ALLOWED_KEY_CLASSES), alongside Symbol / String.
    let ast = lower_src(b"{ 1 => 2, 1.5 => 3, true => 4, false => 5, nil => 6, :s => 7, \"k\" => 8 }\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let hash = find_hash(&ast);
    let ty = typer.type_of(&ast, hash, &env, &mut i);
    let keys: Vec<ShapeKey> = hash_members(i.get(ty)).iter().map(|m| m.key.clone()).collect();
    assert_eq!(
        keys,
        vec![
            ShapeKey::Int(1),
            ShapeKey::Float(1.5f64.to_bits()),
            ShapeKey::Bool(true),
            ShapeKey::Bool(false),
            ShapeKey::Nil,
            ShapeKey::Sym("s".into()),
            ShapeKey::Str("k".into()),
        ]
    );
}

#[test]
fn hash_last_wins_keeps_first_position_last_value() {
    // `{ a: 1, b: 2, a: 3 }` — `a` keeps its FIRST position but takes the
    // LAST value (runtime last-wins), so members are [a=3, b=2].
    let ast = lower_src(b"{ a: 1, b: 2, a: 3 }\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
    let m = hash_members(i.get(ty));
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].key, ShapeKey::Sym("a".into()));
    assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Int(3)));
    assert_eq!(m[1].key, ShapeKey::Sym("b".into()));
    assert_eq!(i.get(m[1].value), &Type::Constant(Scalar::Int(2)));
}

#[test]
fn hash_dup_integer_key_last_wins() {
    let ast = lower_src(b"{ 1 => 1, 1 => 9 }\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
    let m = hash_members(i.get(ty));
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].key, ShapeKey::Int(1));
    assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Int(9)));
}

#[test]
fn hash_float_keys_collide_by_value() {
    // `1.0` and `1.00` are the same f64 → one key, last value wins.
    let ast = lower_src(b"{ 1.0 => :a, 1.00 => :b }\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
    let m = hash_members(i.get(ty));
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].key, ShapeKey::Float(1.0f64.to_bits()));
    assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Sym("b".into())));
}

#[test]
fn hash_int_and_float_keys_are_distinct() {
    // `1` (Int) and `1.0` (Float) are DISTINCT keys (`1.eql?(1.0)` is false).
    let ast = lower_src(b"{ 1 => :i, 1.0 => :f }\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
    let m = hash_members(i.get(ty));
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].key, ShapeKey::Int(1));
    assert_eq!(m[1].key, ShapeKey::Float(1.0f64.to_bits()));
}

#[test]
fn hash_dynamic_key_degrades_to_hash_nominal() {
    // A non-literal key (a method call) can't pin a slot → bare `Hash`.
    let ast = lower_src(b"{ foo => 1 }\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = typer.type_of(&ast, find_hash(&ast), &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));
}

/// rigor-rs#892 — a jump node in VALUE position types `Bot` (the
/// reference's `type_of_jump`), so a `||`/`&&` operand that is one folds
/// out of the union: `[1] || next` types `[1]`. `next` counts too —
/// `stmt_never_completes`'s `JumpKind::Next` exclusion is the block-TIMING
/// question, not the value one.
#[test]
fn jump_nodes_type_bot_in_value_position() {
    let ast = lower_src(
        b"3.times { a = ([1] || next); b = ([1] || break); c = ([1] || redo); d = ([1] || return) }\n",
    );
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    // Every jump carrier types Bot.
    let jump_ids: Vec<NodeId> = ast
        .iter()
        .filter_map(|(id, n)| {
            matches!(
                n,
                Node::Return { .. }
                    | Node::Other { jump: Some(_), .. }
                    | Node::Statements { kind: StatementsKind::Jump(_), .. }
            )
            .then_some(id)
        })
        .collect();
    assert_eq!(jump_ids.len(), 4, "next/break/redo/return all lower");
    for id in jump_ids {
        let ty = typer.type_of(&ast, id, &env, &mut i);
        assert_eq!(i.get(ty), &Type::Bottom);
    }
    // … and each `||` union drops the Bot operand, typing `[1]`.
    let logical_ids: Vec<NodeId> = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::Logical { .. }).then_some(id))
        .collect();
    assert_eq!(logical_ids.len(), 4);
    for id in logical_ids {
        let ty = typer.type_of(&ast, id, &env, &mut i);
        let Type::Tuple(elems) = i.get(ty) else {
            panic!("expected Tuple[1], got {:?}", i.get(ty))
        };
        assert_eq!(elems.len(), 1);
        assert_eq!(i.get(elems[0]), &Type::Constant(Scalar::Int(1)));
    }
}

/// Type the outermost call in `src` (a `v = <hash>.<call>` line).
fn type_of_projection(src: &[u8], method: &str) -> (Interner, TypeId) {
    let ast = lower_src(src);
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, method);
    let ty = typer.type_of(&ast, call, &env, &mut i);
    (i, ty)
}

#[test]
fn hash_index_folds_present_and_missing_keys() {
    // h07: `{ a: 1, b: "s" }[:b]` → `"s"`; a missing key → `nil`.
    let (i, ty) = type_of_projection(b"v = { a: 1, b: \"s\" }[:b]\n", "[]");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("s".into())));
    let (i, ty) = type_of_projection(b"v = { a: 1 }[:z]\n", "[]");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Nil));
}

#[test]
fn hash_index_on_integer_key_folds() {
    let (i, ty) = type_of_projection(b"v = { 1 => \"x\" }[1]\n", "[]");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("x".into())));
}

#[test]
fn hash_fetch_present_folds_missing_declines() {
    // h08: `.fetch(:a)` folds to the value; a miss DECLINES (KeyError) →
    // the RBS Hash tier answers (not a folded Constant).
    let (i, ty) = type_of_projection(b"v = { a: 1 }.fetch(:a)\n", "fetch");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(1)));
    let (i, ty) = type_of_projection(b"v = { a: 1 }.fetch(:z)\n", "fetch");
    assert!(!matches!(i.get(ty), Type::Constant(_)), "fetch miss must not fold to a Constant");
}

#[test]
fn hash_has_key_folds_to_bool() {
    // h09: `.has_key?` / aliases fold to a precise bool.
    for (src, expect) in [
        (b"v = { a: 1 }.has_key?(:a)\n".as_slice(), true),
        (b"v = { a: 1 }.has_key?(:z)\n".as_slice(), false),
    ] {
        let (i, ty) = type_of_projection(src, "has_key?");
        assert_eq!(i.get(ty), &Type::Constant(Scalar::Bool(expect)));
    }
    let (i, ty) = type_of_projection(b"v = { a: 1 }.key?(:a)\n", "key?");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Bool(true)));
    let (i, ty) = type_of_projection(b"v = { a: 1 }.include?(:z)\n", "include?");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Bool(false)));
}

#[test]
fn hash_values_at_folds_to_tuple_in_arg_order() {
    // `{ a: 1, b: 2 }.values_at(:b, :z, :a)` → Tuple[2, nil, 1].
    let (i, ty) = type_of_projection(b"v = { a: 1, b: 2 }.values_at(:b, :z, :a)\n", "values_at");
    let Type::Tuple(elems) = i.get(ty) else { panic!("expected Tuple, got {:?}", i.get(ty)) };
    let got: Vec<Type> = elems.iter().map(|&e| i.get(e).clone()).collect();
    assert_eq!(
        got,
        vec![
            Type::Constant(Scalar::Int(2)),
            Type::Constant(Scalar::Nil),
            Type::Constant(Scalar::Int(1)),
        ]
    );
}

#[test]
fn hash_slice_keeps_present_keys_in_arg_order() {
    // `{ a: 1, b: 2, c: 3 }.slice(:c, :a)` → { c: 3, a: 1 } (arg order).
    let (i, ty) = type_of_projection(b"v = { a: 1, b: 2, c: 3 }.slice(:c, :a)\n", "slice");
    let m = hash_members(i.get(ty));
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].key, ShapeKey::Sym("c".into()));
    assert_eq!(m[1].key, ShapeKey::Sym("a".into()));
}

#[test]
fn hash_except_drops_keys_in_receiver_order() {
    let (i, ty) = type_of_projection(b"v = { a: 1, b: 2, c: 3 }.except(:b)\n", "except");
    let keys: Vec<ShapeKey> = hash_members(i.get(ty)).iter().map(|m| m.key.clone()).collect();
    assert_eq!(keys, vec![ShapeKey::Sym("a".into()), ShapeKey::Sym("c".into())]);
}

#[test]
fn hash_invert_swaps_keys_and_values() {
    // `{ a: 1, b: 2 }.invert` → { 1 => :a, 2 => :b }.
    let (i, ty) = type_of_projection(b"v = { a: 1, b: 2 }.invert\n", "invert");
    let m = hash_members(i.get(ty));
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].key, ShapeKey::Int(1));
    assert_eq!(i.get(m[0].value), &Type::Constant(Scalar::Sym("a".into())));
    assert_eq!(m[1].key, ShapeKey::Int(2));
    assert_eq!(i.get(m[1].value), &Type::Constant(Scalar::Sym("b".into())));
}

#[test]
fn hash_invert_declines_on_value_collision() {
    // A duplicate VALUE would alias under inversion → decline (falls to RBS,
    // not a folded HashShape).
    let (i, ty) = type_of_projection(b"v = { a: 1, b: 1 }.invert\n", "invert");
    assert!(!matches!(i.get(ty), Type::HashShape(_)), "collision must not fold to a HashShape");
}

#[test]
fn hash_dig_folds_single_and_nested_chains() {
    let (i, ty) = type_of_projection(b"v = { a: 1 }.dig(:a)\n", "dig");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(1)));
    let (i, ty) = type_of_projection(b"v = { a: { b: 5 } }.dig(:a, :b)\n", "dig");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(5)));
    // A missing key mid-chain short-circuits to nil.
    let (i, ty) = type_of_projection(b"v = { a: { b: 5 } }.dig(:a, :z)\n", "dig");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Nil));
}

#[test]
fn hash_projection_declines_on_dynamic_key() {
    // A non-literal key argument declines the fold (reference gates on a
    // value-pinned Constant key), so the RBS Hash tier answers.
    let (i, ty) = type_of_projection(b"v = { a: 1 }[foo]\n", "[]");
    assert!(!matches!(i.get(ty), Type::Constant(Scalar::Int(1))));
}

#[test]
fn method_param_read_is_dynamic_top() {
    // Inside `def foo(x); x.bar; end`, the receiver `x` is a param read with
    // no top-level binding -> Dynamic[top] -> the call rule stays silent.
    // This is the zero-FP keystone for lowering def bodies.
    let ast = lower_src(b"def foo(x)\n  x.bar\nend\n");
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let recv = ast
        .iter()
        .find_map(|(_, n)| match n {
            Node::Call { receiver: Some(r), method, .. } if method == "bar" => Some(*r),
            _ => None,
        })
        .unwrap();
    let ty = typer.type_of(&ast, recv, &env, &mut i);
    assert_eq!(ty, i.untyped());
}

#[test]
fn ivar_and_self_and_const_reads_are_dynamic_top() {
    // `@x`, `self`, and a constant read all type to Dynamic[top] (silent).
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    for src in [b"@x.foo\n".as_slice(), b"self.foo\n".as_slice(), b"Foo.foo\n".as_slice()] {
        let ast = lower_src(src);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        let recv = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { receiver: Some(r), method, .. } if method == "foo" => Some(*r),
                _ => None,
            })
            .unwrap();
        let ty = typer.type_of(&ast, recv, &env, &mut i);
        assert_eq!(ty, i.untyped(), "receiver of {src:?} must be Dynamic[top]");
    }
}

#[test]
fn non_deterministic_or_unknown_call_is_dynamic_top() {
    // `Array#sample` is non-deterministic: never folded, no modeled return
    // -> Dynamic[top]. Drive it on a value-pinned Integer receiver whose
    // unknown method has no return: `42.sample` (sample isn't on Integer).
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let ast = lower_src(b"42.sample\n");
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "sample");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(ty, i.untyped());
}

// --- in-source class typing (ADR-0023 tier-4) ---------------------------

#[test]
fn source_class_new_types_to_source_instance() {
    // `class Point; def x; end; end; p = Point.new` — `Point.new` types to a
    // Nominal instance whose ClassId resolves back to "Point" via the source
    // index, and the source index witnesses `y` absent (chain complete:
    // implicit Object super, fully RBS-loaded).
    let ast = lower_src(b"class Point\n  def x\n  end\nend\np = Point.new\np.y\n");
    let idx = CoreIndex::new();
    let source = SourceIndex::build(&ast, &idx);
    let typer = Typer::with_source(&idx, &source);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    // `p` binds to the Point.new instance.
    let p_ty = *env.get("p").expect("p should be bound");
    let class = match i.get(p_ty) {
        Type::Nominal { class, .. } => *class,
        other => panic!("expected Nominal instance, got {other:?}"),
    };
    assert_eq!(source.class_name_for_id(class), Some("Point"));
    // `x` is defined, `y` is not — and the chain is complete.
    assert!(source.class_has_method(&idx, "Point", "x"));
    assert!(!source.class_has_method(&idx, "Point", "y"));
    // Inherited Object method is present (no false absence).
    assert!(source.class_has_method(&idx, "Point", "frozen?"));
}

#[test]
fn unknown_superclass_makes_chain_incomplete_and_silent() {
    // `class User < ApplicationRecord; end` — ApplicationRecord is neither
    // source nor RBS ⇒ chain INCOMPLETE ⇒ any method is assumed present.
    let ast = lower_src(b"class User < ApplicationRecord\nend\nu = User.new\nu.anything\n");
    let idx = CoreIndex::new();
    let source = SourceIndex::build(&ast, &idx);
    assert!(source.knows_class("User"));
    // Even a clearly-bogus method is assumed present (zero-FP keystone).
    assert!(source.class_has_method(&idx, "User", "totally_made_up_xyz"));
    assert!(source.class_has_method(&idx, "User", "anything"));
}

#[test]
fn reopened_source_class_unions_methods() {
    // Two `class C` bodies: the SourceIndex unions their methods.
    let ast = lower_src(b"class C\n  def a\n  end\nend\nclass C\n  def b\n  end\nend\n");
    let idx = CoreIndex::new();
    let source = SourceIndex::build(&ast, &idx);
    assert!(source.class_has_method(&idx, "C", "a"));
    assert!(source.class_has_method(&idx, "C", "b"));
    // A method on neither reopen is witnessed absent (complete chain).
    assert!(!source.class_has_method(&idx, "C", "c"));
}

#[test]
fn source_superclass_chain_resolves_inherited_method() {
    // `class Animal; def speak; end; end; class Dog < Animal; end` —
    // Dog.new.speak is inherited (present); Dog.new.fly is absent (the whole
    // chain Dog -> Animal -> Object is known).
    let ast = lower_src(
        b"class Animal\n  def speak\n  end\nend\nclass Dog < Animal\nend\n",
    );
    let idx = CoreIndex::new();
    let source = SourceIndex::build(&ast, &idx);
    assert!(source.class_has_method(&idx, "Dog", "speak"));
    assert!(!source.class_has_method(&idx, "Dog", "fly"));
}

#[test]
fn rbs_class_new_types_to_rbs_instance() {
    // `Pathname.new("a")` — Pathname is RBS-known (with the stdlib tree) but
    // outside CORE_CLASSES. The stdlib `.new` leniency now lives in the
    // TYPING (`type_dot_new` declines the mint ⇒ Dynamic): the UM witness
    // gate is `knows_class`-wide for source-range Nominals, so a minted
    // Pathname instance WOULD witness — and the reference's `.new` dispatch
    // on these classes has an intricate folding/reflection boundary
    // (fixture 38 pins `Pathname.new("x").nope` silent). The registry /
    // method-existence wiring stays intact for the paths that DO mint
    // (singleton RBS returns — `Pathname.pwd` — and project classes).
    let ast = lower_src(b"p = Pathname.new(\"a\")\np.foo\nq = Pathname.pwd\nq.foo\n");
    let idx = CoreIndex::new();
    let source = SourceIndex::build(&ast, &idx);
    if idx.knows_class("Pathname") {
        let typer = Typer::with_source(&idx, &source);
        let mut i = Interner::new();
        let env = typer.build_toplevel_env(&ast, &mut i);
        // `.new` mint declined ⇒ Dynamic (the leniency).
        let p_ty = *env.get("p").expect("p should be bound");
        assert!(
            matches!(i.get(p_ty), Type::Dynamic(_)),
            "stdlib .new must decline the mint, got {:?}",
            i.get(p_ty)
        );
        // The declaration-driven singleton return still mints the instance
        // (`def self.pwd: () -> Pathname` in core pathname.rbs).
        let q_ty = *env.get("q").expect("q should be bound");
        let class = match i.get(q_ty) {
            Type::Nominal { class, .. } => *class,
            other => panic!("expected Nominal instance from Pathname.pwd, got {other:?}"),
        };
        assert_eq!(source.class_name_for_id(class), Some("Pathname"));
        // A real Pathname method is present; a typo is absent (via RBS).
        assert!(source.class_has_method(&idx, "Pathname", "basename"));
        assert!(!source.class_has_method(&idx, "Pathname", "nonexist"));
    }
}

// --- block-form call result typing (recovered, RBS-derived) -------------

#[test]
fn block_call_return_types_to_rbs_block_overload() {
    // `arr.map { }` types to a bare Array Nominal (the block-overload
    // return), so a chained `.frist` resolves against Array and is
    // witnessable; `h.select { }` types to Hash; `x.tap { }` types to the
    // receiver's own class. Guarded on the real RBS tree (under the stub
    // fallback block returns are unmodeled ⇒ Dynamic ⇒ test is vacuous).
    let idx = CoreIndex::new();
    if !idx.knows_class("Enumerable") || !idx.class_has_method("Array", "map") {
        return;
    }
    // `a = []; a.map { |x| x }` -> Array nominal.
    let ast = lower_src(b"a = [1]\na.map { |x| x }\n");
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "map");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("Array"));

    // `h = {}; h.select { }` -> Hash nominal (so `.keys` is valid, silent).
    let ast = lower_src(b"h = { a: 1 }\nh.select { |k, v| v }\n");
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "select");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));

    // `s = "x"; s.tap { }` -> String nominal (self block return = receiver).
    let ast = lower_src(b"s = \"x\"\ns.tap { |x| x }\n");
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "tap");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("String"));
}

#[test]
fn block_call_on_unmodeled_or_dynamic_is_silent_dynamic() {
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    // A block call on a Dynamic receiver (`x` is an implicit-self call) ⇒
    // Dynamic (never guess). True under both real RBS and the stub.
    let ast = lower_src(b"x.each { |e| e }\n");
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "each");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(ty, i.untyped(), "block call on Dynamic receiver must be Dynamic[top]");
}

// --- rigor-rs#194: per-element `map`/`collect` fold over a Tuple ---------
//
// The port of `try_per_element_block_fold` (`expression_typer.rb:4506`), the
// `map`/`collect` head of `PER_ELEMENT_TUPLE_METHODS`: a Tuple receiver folds
// its literal block once per position into `Tuple[U_1..U_n]` — strictly
// tighter than the RBS `Array`. Rows outside the ported subset decline to
// the prior `Array` answer (never an FP).

#[test]
fn per_element_map_fold_assembles_a_tuple() {
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    // `[1, 2].map { |x| x + 1 }` — per-position `x + 1` folds to `2` / `3`.
    let ast = lower_src(b"[1, 2].map { |x| x + 1 }\n");
    let call = find_call(&ast, "map");
    let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
    let Type::Tuple(elems) = i.get(ty).clone() else {
        panic!("map over a literal tuple must fold to a Tuple, got {}", rigor_types::describe(&i, ty));
    };
    assert_eq!(elems.len(), 2);
    assert_eq!(i.get(elems[0]), &Type::Constant(Scalar::Int(2)));
    assert_eq!(i.get(elems[1]), &Type::Constant(Scalar::Int(3)));

    // `collect` folds identically; a block-local write reads per position.
    let ast = lower_src(b"[1, 2].collect { |x| y = x + 1; y }\n");
    let call = find_call(&ast, "collect");
    let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
    let Type::Tuple(elems) = i.get(ty).clone() else {
        panic!("collect must fold identically, got {}", rigor_types::describe(&i, ty));
    };
    assert_eq!(i.get(elems[0]), &Type::Constant(Scalar::Int(2)));
    assert_eq!(i.get(elems[1]), &Type::Constant(Scalar::Int(3)));

    // The implicit `it` parameter binds the element too.
    let ast = lower_src(b"[1, 2].map { it * 10 }\n");
    let call = find_call(&ast, "map");
    let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
    let Type::Tuple(elems) = i.get(ty).clone() else {
        panic!("`it` must bind the element, got {}", rigor_types::describe(&i, ty));
    };
    assert_eq!(i.get(elems[0]), &Type::Constant(Scalar::Int(10)));
    assert_eq!(i.get(elems[1]), &Type::Constant(Scalar::Int(20)));
}

#[test]
fn per_element_fold_filter_and_find_families() {
    // The rest of `PER_ELEMENT_TUPLE_METHODS`: `select`/`reject` keep the
    // receiver's own elements at decisive-Constant positions; `filter_map`
    // keeps non-nil/false RESULTS; `flat_map` flattens Tuple positions;
    // `find`/`find_index` answer the first truthy position's element/index.
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut folded = |src: &[u8], method: &str| -> String {
        let ast = lower_src(src);
        let call = find_call(&ast, method);
        let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
        rigor_types::describe(&i, ty)
    };
    assert_eq!(
        folded(b"[1, 2].select { |x| x > 1 }\n", "select"),
        "Tuple[Constant[2]]"
    );
    assert_eq!(
        folded(b"[1, 2].reject { |x| x > 1 }\n", "reject"),
        "Tuple[Constant[1]]"
    );
    assert_eq!(
        folded(b"[1, 2].filter_map { |x| x.to_s if x > 1 }\n", "filter_map"),
        "Tuple[Constant[\"2\"]]"
    );
    assert_eq!(
        folded(b"[1, 2].flat_map { |x| [x, x] }\n", "flat_map"),
        "Tuple[Constant[1], Constant[1], Constant[2], Constant[2]]"
    );
    assert_eq!(folded(b"[1, 2].find { |x| x > 1 }\n", "find"), "Constant[2]");
    assert_eq!(folded(b"[1, 2].find { |x| x > 9 }\n", "find"), "nil");
    assert_eq!(
        folded(b"[1, 2].find_index { |x| x > 1 }\n", "find_index"),
        "Constant[1]"
    );
    // An undecided position takes `undecided_fold_floor`: `select` widens to
    // `Array[union]`; `find` answers `union(elements, nil)`.
    let undecided = folded(b"[1, 2].select { |x| unknown_call }\n", "select");
    assert!(
        !undecided.starts_with("Tuple"),
        "undecided select must not fold: {undecided}"
    );
    assert_eq!(
        folded(b"[1, 2].find { |x| unknown_call }\n", "find"),
        "Constant[1] | Constant[2] | nil"
    );
}

#[test]
fn per_element_map_fold_declines_unported_shapes() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Array", "map") {
        return; // stub index: every row declines anyway — vacuous.
    }
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut not_folded = |src: &[u8]| -> bool {
        let ast = lower_src(src);
        let method = ast
            .iter()
            .find_map(|(_, n)| match n {
                Node::Call { receiver: Some(_), method, .. } => Some(method.clone()),
                _ => None,
            })
            .expect("a receiver call");
        let call = find_call(&ast, &method);
        let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
        !matches!(i.get(ty), Type::Tuple(_))
    };
    // An argument declines (`call_node.arguments.nil?` — even though `map`
    // takes none, the gate is syntactic exactly like the reference).
    assert!(not_folded(b"[1, 2].map(1) { |x| x }\n"));
    // A block-level `next` declines — the reference joins `next` arms through
    // its evaluator; a tail-only read cannot.
    assert!(not_folded(b"[1, 2].map { |x| next x }\n"));
    // A `&:sym` pass is not a literal `BlockNode` (`block_span` is None).
    assert!(not_folded(b"[1, 2].map(&:to_s)\n"));
    // A destructured first parameter declines — `|(a, (b, c))|` needs the
    // reference's real destructure, which a flat positional bind would pin
    // wrongly.
    assert!(not_folded(b"[[1, 2], [3, 4]].map { |(a, b)| a + b }\n"));
    // A non-Tuple receiver (a plain `Array` nominal) declines.
    assert!(not_folded(b"Array.new.map { |x| x }\n"));
}

#[test]
fn per_element_map_fold_declines_captured_rebinds() {
    // A write to a CAPTURED local is the reference's cross-position
    // threading (`per_element_captured_bindings`), which the flat
    // per-position overlay would pin wrongly — decline to `Array`.
    let idx = CoreIndex::new();
    if !idx.class_has_method("Array", "map") {
        return;
    }
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let ast = lower_src(b"seen = 0\n[1, 2].map { |x| seen += 1; x }\n");
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, "map");
    let ty = typer.type_of(&ast, call, &env, &mut i);
    assert_eq!(idx.class_name_of(&i, ty), Some("Array"));
    // A write to a BLOCK-LOCAL name shadows, never captures — still folds.
    let ast = lower_src(b"[1, 2].map { |x| y = x; y + 1 }\n");
    let call = find_call(&ast, "map");
    let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
    assert!(matches!(i.get(ty), Type::Tuple(_)), "block-local writes still fold");
}

#[test]
fn per_element_map_fold_declines_unmodelled_writes() {
    // rigor-rs#194 FINAL review — the flat per-position overlay replays only
    // a direct-member `LocalVariableWrite` before the tail. Any other write
    // or in-place mutation visible in the block span must decline the whole
    // fold: answering the tail with the ENTRY binding mints a wrong constant
    // (`[1, 2].map { |x| x += 1; x }.first.even?` reads `false` where the
    // reference's `[2, 3]` gives `true`) — a false-positive vector.
    let idx = CoreIndex::new();
    if !idx.class_has_method("Array", "map") {
        return; // stub index: every row declines anyway — vacuous.
    }
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let mut not_folded = |src: &[u8]| -> bool {
        let ast = lower_src(src);
        let call = find_call(&ast, "map");
        let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
        !matches!(i.get(ty), Type::Tuple(_))
    };
    // An op-write on the block parameter — read-modify-write is unmodeled.
    assert!(not_folded(b"[1, 2].map { |x| x += 1; x }\n"));
    // …on a block-local (`y += 1` changes what the tail reads).
    assert!(not_folded(b"[1, 2].map { |x| y = x; y += 1; y }\n"));
    // A shape mutator on the parameter mutates the ELEMENT between write and
    // tail read — the reference answers the post-mutation tuple.
    assert!(not_folded(b"[[1], [2]].map { |x| x << 3; x }\n"));
    // A setter / index-write on a read binding (`x[0] = 9` lowers as `[]=`).
    assert!(not_folded(b"[[1], [2]].map { |x| x[0] = 9; x }\n"));
    // A mutation of a sub-object reachable through the binding (`x[0] << 9`).
    assert!(not_folded(b"[[1], [2]].map { |x| x[0] << 9; x }\n"));
    // Writes on non-local targets the lowering cannot reproduce: operator
    // writes (`x.f += 1`), `K::V` path writes, multiwrites, `for` index
    // binds, `rescue => e`, and `in`/`=>` pattern binds.
    assert!(not_folded(b"[1, 2].map { |x| K = x; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| @v = x; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| $g = x; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| @v += x; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| x.f += 1; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| K::V = x; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| a, b = x, x; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| for q in []; end; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| begin; rescue => e; end; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| case x; in Integer => q; end; x }\n"));
    assert!(not_folded(b"[1, 2].map { |x| x => q; x }\n"));
    // A write nested inside a conditional is not a direct member of
    // `block_body` — the overlay never replays it, so the fold declines.
    assert!(not_folded(b"[1, 2].map { |x| x = 9 if x > 1; x }\n"));
    // A plain rebind of the parameter IS replayed — still folds to `[9, 9]`.
    let ast = lower_src(b"[1, 2].map { |x| x = 9; x }\n");
    let call = find_call(&ast, "map");
    let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
    let Type::Tuple(elems) = i.get(ty).clone() else {
        panic!("plain rebind still folds, got {}", rigor_types::describe(&i, ty));
    };
    assert_eq!(i.get(elems[0]), &Type::Constant(Scalar::Int(9)));
    assert_eq!(i.get(elems[1]), &Type::Constant(Scalar::Int(9)));
    // Contents of `yield`/`super`/`defined?` operands never bind in the
    // reference's evaluator (no `YieldNode`/`SuperNode`/`DefinedNode`
    // handler), so writes inside them are EXEMPT — the fold proceeds and
    // matches the reference's entry-element answer `[1, 2]` exactly.
    for src in [
        b"def f; [1, 2].map { |x| yield (x = 9); x }; end\n".as_slice(),
        b"[1, 2].map { |x| defined?(x = 9); x }\n".as_slice(),
        b"[1, 2].map { |x| yield x; x }\n".as_slice(),
    ] {
        let ast = lower_src(src);
        let call = find_call(&ast, "map");
        let ty = typer.type_of(&ast, call, &TypeEnv::new(), &mut i);
        assert!(
            matches!(i.get(ty), Type::Tuple(_)),
            "inert contents are exempt: {}",
            String::from_utf8_lossy(src)
        );
    }
}

// --- rigor-rs#427: a block chain's answers store in the env memo ---------
//
// Every `.map`-chain node's answer is a deep `Tuple` that embeds interner
// ids. `type_of_env_memo` is scoped to the interner's uid, so those answers
// STORE and a later driver visit answers from the entry; before that was
// understood, the `type_is_interner_free` store gate dropped every such
// answer, and each driver visit (the rules walk every `Call` node)
// re-typed its whole receiver suffix — and since a block call types its
// receiver TWICE (the #479 literal-concat gate, then `type_block_call`),
// the work DOUBLED per link: exponential in the chain (2^k − 1 events for
// the k-th link's visit), and the issue's `.map` repro (100 links ×
// 9000-deep blocks, 2^100 typing work) never finished.

/// `[1]` + `blocks` × `.map { |y| [[…[y]…]] }` (each wrap `depth` deep) +
/// `.foo` — the rigor-rs#427 `.map` repro at reference-survivable sizes.
fn map_chain_src(blocks: usize, depth: usize) -> Vec<u8> {
    let block = format!(".map {{ |y| {}y{} }}", "[".repeat(depth), "]".repeat(depth));
    format!("[1]{}.foo\n", block.repeat(blocks)).into_bytes()
}

#[test]
fn block_chain_answers_store_in_the_env_memo() {
    let ast = lower_src(&map_chain_src(3, 2));
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    // Type every call node the way the driver does: one visit per `Call`.
    let calls: Vec<_> = ast
        .iter()
        .filter(|(_, n)| matches!(n, Node::Call { .. }))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(calls.len(), 4, "three map links plus the trailing .foo");
    let env = typer.build_toplevel_env(&ast, &mut i);
    for &id in &calls {
        typer.type_of(&ast, id, &env, &mut i);
    }
    // The interner-bound Tuple answers must have STORED: an entry per map
    // link (a 0 would mean every later visit re-types its whole suffix).
    for (id, n) in ast.iter() {
        if matches!(n, Node::Call { method, .. } if method == "map") {
            assert!(
                typer.reach_memo.borrow().env_type_entry_count(id) > 0,
                "map node {id:?} must store its interner-bound answer"
            );
        }
    }
    // A depth-`d` wrap binds `y` to the receiver's ELEMENT — one level
    // shallower than the link itself — so each link adds exactly `d`
    // levels: 3 links of a depth-2 wrap over `[1]` nest `Tuple` exactly
    // 1 + 3 × 2 = 7 deep down to the folded `1`. The memo must not move
    // that answer. (Lowering pushes children before parents, so the
    // OUTERMOST map — the one whose answer nests deepest — is the
    // next-to-last call, just before the trailing `.foo`.)
    const LINKS: usize = 3;
    const DEPTH: usize = 2;
    let outer = calls[calls.len() - 2];
    let ty = typer.type_of(&ast, outer, &env, &mut i);
    let mut cur = ty;
    for _ in 0..(1 + LINKS * DEPTH) {
        let Type::Tuple(elems) = i.get(cur).clone() else {
            panic!(
                "depth {} of Tuple, got {}",
                1 + LINKS * DEPTH,
                rigor_types::describe(&i, cur)
            );
        };
        assert_eq!(elems.len(), 1, "each wrap nests exactly one element");
        cur = elems[0];
    }
    assert_eq!(i.get(cur), &Type::Constant(Scalar::Int(1)));
    // A memo hit re-interns the stored carrier under the SAME interner —
    // a dedup hit — so a re-visit answers the identical TypeId.
    assert_eq!(typer.type_of(&ast, outer, &env, &mut i), ty);
}

#[test]
fn block_chain_types_once_per_driver_visit() {
    // Small enough that an unmemoized regression FAILS the assertion in
    // under a second rather than hanging CI: at 10 links the old store
    // gate costs ~2^11 events (the 8-link measurement below was 757).
    const LINKS: usize = 10;
    let ast = lower_src(&map_chain_src(LINKS, 2));
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let calls: Vec<_> = ast
        .iter()
        .filter(|(_, n)| matches!(n, Node::Call { .. }))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(calls.len(), LINKS + 1);
    crate::block_call::reset_block_call_events();
    for &id in &calls {
        typer.type_of(&ast, id, &env, &mut i);
    }
    let events = crate::block_call::block_call_events();
    assert!(
        events <= LINKS as u64 + 2,
        "each link must type once ({events} block-call events for {LINKS} links; \
         unmemoized, the literal-concat gate types the receiver once and \
         `type_block_call` once more, so each visit DOUBLES — an 8-link \
         chain already costs 757 events, 2^k - 1 per visit)"
    );
}

#[test]
fn bignum_literal_pins_its_exact_value() {
    // rigor-rs#194 — a Bignum types `Constant(BigInt)` carrying its decimal
    // spelling (the reference's `Constant[99999999999999999999]`), not a
    // widened `Integer` nominal.
    let idx = CoreIndex::new();
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let ast = lower_src(b"[99999999999999999999, 42]\n");
    let ty = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::ArrayLit { .. }).then_some(id))
        .map(|id| typer.type_of(&ast, id, &TypeEnv::new(), &mut i))
        .expect("an array literal");
    let Type::Tuple(elems) = i.get(ty).clone() else {
        panic!("array literal types Tuple, got {}", rigor_types::describe(&i, ty));
    };
    assert_eq!(
        i.get(elems[0]),
        &Type::Constant(Scalar::BigInt("99999999999999999999".to_string()))
    );
    assert_eq!(i.get(elems[1]), &Type::Constant(Scalar::Int(42)));
}

// --- rigor-rs#140: exactly-once block timing (upstream rigor#1105) ------
//
// `Kernel#tap` / `#then` / `#yield_self` invoke a literal block exactly
// once before returning, so a block that provably never completes normally
// makes the call's ordinary return unreachable: the call types to its
// `break` arms alone (`bot` when none). These pin the type-level answers
// the parity probes measure; every row is oracle-measured on the pinned
// reference.

/// Type the `x = …` binding of `src` through a real SourceIndex, the way
/// `check` sees it. The returned interner holds `TypeId`s against `idx`.
fn bound_type(idx: &CoreIndex, src: &[u8], name: &str) -> (Interner, TypeId) {
    let ast = lower_src(src);
    let source = SourceIndex::build(&ast, idx);
    let typer = Typer::with_source(idx, &source);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = *env.get(name).unwrap_or_else(|| panic!("{name} should be bound"));
    (i, ty)
}

/// rigor-rs#685: `coll_value_members` pushes Tuple elements and HashShape
/// values UNFLATTENED, so a member that is itself a union (`[c ? 1 : "a"]`
/// types `1 | "a" | Dynamic[top]` inside the tuple) reached
/// `coll_nominal_with`'s raw `intern(Type::Union(..))` and minted a `Union`
/// inside a `Union` — the same logical member set then interned under two
/// different `TypeId`s. The arg must hold the canonical flat union.
#[test]
fn coll_nominal_with_flattens_union_members() {
    let idx = CoreIndex::new();
    let Some(array) = idx.class_id("Array") else {
        return;
    };
    let typer = Typer::new(&idx);
    let mut i = Interner::new();
    let one = i.int(1);
    let s = i.intern(Type::Constant(Scalar::Str("a".into())));
    let inner = i.intern(Type::Union(vec![one, s]));
    let ty = typer
        .coll_nominal_with(&mut i, "Array", &[inner, one])
        .expect("Array resolves");
    let Type::Nominal { class, args } = i.get(ty) else {
        panic!("coll_nominal_with must mint a Nominal, got {:?}", i.get(ty));
    };
    assert_eq!(*class, array);
    let Type::Union(ms) = i.get(args[0]) else {
        panic!("the member arg must be a union, got {:?}", i.get(args[0]));
    };
    assert!(
        ms.iter().all(|&m| !matches!(i.get(m), Type::Union(_))),
        "union members must be flat, got {ms:?}"
    );
    assert!(ms.contains(&one) && ms.contains(&s));
}

#[test]
fn tap_break_value_is_the_call_type() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    // The headline row: `break "s"` leaves the call `"s"`, not the
    // receiver — the issue's false positive.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { break \"s\" }\n", "x");
    assert_eq!(
        i.get(ty),
        &Type::Constant(Scalar::Str("s".to_string())),
        "tap {{ break \"s\" }} must type to the break arm"
    );
    // A bare `break` carries `nil`.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { break }\n", "x");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Nil));
    // `tap()` is NOT a decline: Prism leaves `call.arguments` empty for
    // the empty parens, so the reference's `node.arguments` gate passes it
    // too (probed: `tap() { break "s" }; x.push 3` fires `… for "s"` on
    // both engines). The gate only rejects a NON-EMPTY argument list.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap() { break \"s\" }\n", "x");
    assert_eq!(
        i.get(ty),
        &Type::Constant(Scalar::Str("s".to_string())),
        "tap() {{ break \"s\" }} still types to the break arm"
    );
}

#[test]
fn tap_raise_is_bottom() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    // No break arms and no normal completion ⇒ `bot` — the reference's
    // own answer for `tap { raise "x" }`.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { raise \"x\" }\n", "x");
    assert_eq!(i.get(ty), &Type::Bottom, "tap {{ raise }} must be bot");
    // `self.raise` is NOT a Kernel-spelled non-returning call in the
    // reference's block-return pass — the ordinary result survives.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { self.raise \"x\" }\n", "x");
    assert_eq!(idx.class_name_of(&i, ty), Some("Array"));
}

#[test]
fn tap_decline_rows_keep_the_receiver() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    for src in [
        // `next` completes the block normally — no arms, no drop.
        b"x = [1, 2].tap { next }\n".as_slice(),
        // `next` present at all declines the bot approximation; the union
        // still contains the receiver.
        b"x = [1, 2].tap { next; break \"s\" }\n".as_slice(),
        // A NON-EMPTY argument list declines the proof — none of the
        // three takes one. (`tap()` alone does NOT decline.)
        b"x = [1, 2].tap(1) { break \"s\" }\n".as_slice(),
        // A nested block's `break` retargets onto `each`, not `tap`.
        b"x = [1, 2].tap { [3].each { break \"s\" } }\n".as_slice(),
        // `break` on a provably dead branch contributes nothing.
        b"x = [1, 2].tap { break \"s\" if false }\n".as_slice(),
        // A block-pass has no literal body to prove anything about.
        b"blk = ->(v) { }\nx = [1, 2].tap(&blk)\n".as_slice(),
    ] {
        let (mut i, ty) = bound_type(&idx, src, "x");
        let array = idx
            .class_id("Array")
            .map(|c| i.intern(Type::Nominal { class: c, args: vec![] }));
        match i.get(ty) {
            Type::Nominal { .. } => {
                assert_eq!(idx.class_name_of(&i, ty), Some("Array"), "{:?}", String::from_utf8_lossy(src))
            }
            Type::Union(members) => assert!(
                array.is_some_and(|a| members.contains(&a)),
                "union must retain Array: {:?}",
                String::from_utf8_lossy(src)
            ),
            other => panic!(
                "receiver must survive in {:?}, got {other:?}",
                String::from_utf8_lossy(src)
            ),
        }
    }
}

#[test]
fn then_and_yield_self_break_value_is_the_call_type() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    for method in ["then", "yield_self"] {
        let src = format!("x = 1.{method} {{ break \"s\" }}\n");
        let (i, ty) = bound_type(&idx, src.as_bytes(), "x");
        assert_eq!(
            i.get(ty),
            &Type::Constant(Scalar::Str("s".to_string())),
            "{method} {{ break \"s\" }} must type to the break arm"
        );
    }
}

#[test]
fn tap_override_suppresses_the_proof() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    // A project reopening of the RECEIVER class: `Array#tap` resolves to
    // the project def, never Kernel — the exactly-once drop declines, so
    // the answer is the #853 union `Array | "s"`, not the arm alone.
    let (mut i, ty) = bound_type(
        &idx,
        b"class Array\n  def tap\n    self\n  end\nend\nx = [1, 2].tap { break \"s\" }\n",
        "x",
    );
    let array = idx
        .class_id("Array")
        .map(|c| i.intern(Type::Nominal { class: c, args: vec![] }))
        .expect("Array class id");
    let Type::Union(members) = i.get(ty) else {
        panic!("override must keep the union, got {:?}", i.get(ty));
    };
    assert!(members.contains(&array), "union must retain Array, got {:?}", i.get(ty));

    // A toplevel `def tap` is a private `Object` method — it shadows
    // Kernel's, so the proof declines to the same union.
    let (mut i, ty) = bound_type(&idx, b"def tap\n  self\nend\nx = [1, 2].tap { break \"s\" }\n", "x");
    let array = idx
        .class_id("Array")
        .map(|c| i.intern(Type::Nominal { class: c, args: vec![] }))
        .expect("Array class id");
    let Type::Union(members) = i.get(ty) else {
        panic!("toplevel def must keep the union, got {:?}", i.get(ty));
    };
    assert!(members.contains(&array), "union must retain Array, got {:?}", i.get(ty));
}

#[test]
fn tap_conditional_break_unions_the_arm() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    // `break "s" if c` might not break at all ⇒ `Array | "s"`, not `"s"`.
    let (i, ty) = bound_type(&idx, b"c = unknown_read\nx = [1, 2].tap { break \"s\" if c }\n", "x");
    let Type::Union(members) = i.get(ty) else {
        panic!("conditional break must union, got {:?}", i.get(ty));
    };
    // The arm side pins the literal, beside the receiver.
    let arm_is_str = members
        .iter()
        .any(|&m| matches!(i.get(m), Type::Constant(Scalar::Str(s)) if s == "s"));
    let keeps_array = members
        .iter()
        .any(|&m| idx.class_name_of(&i, m) == Some("Array"));
    assert!(arm_is_str && keeps_array, "union must carry \"s\" beside Array, got {:?}", i.get(ty));
}

#[test]
fn tap_block_param_binding_kinds() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    // `|(a, b)|` destructures hide the outer name but stay UNBOUND —
    // the reference's destructure read off a nominal `Array[T]` is
    // optimistic, which never witnesses; `break a` is Dynamic.
    let (i, ty) = bound_type(
        &idx,
        b"a = 1\nx = [1, 2].tap { |(a, b)| break a }\n",
        "x",
    );
    assert!(
        matches!(i.get(ty), Type::Dynamic(_)),
        "destructured arm must stay Dynamic[top], got {:?}",
        i.get(ty)
    );
    // `it` is the receiver-bound self-arg — `break it` is the receiver.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { break it }\n", "x");
    assert!(
        idx.class_name_of(&i, ty) == Some("Array")
            || matches!(i.get(ty), Type::Tuple(_)),
        "`it` must read the receiver, got {:?}",
        i.get(ty)
    );
    // `**kw` binds the captured keyword `Hash`; `&blk` binds `Proc`.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { |**kw| break kw }\n", "x");
    assert_eq!(idx.class_name_of(&i, ty), Some("Hash"));
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { |&blk| break blk }\n", "x");
    assert_eq!(idx.class_name_of(&i, ty), Some("Proc"));
    // `|;local|` declarations hide the outer name and bind nothing —
    // `break v` reads the hidden local as Dynamic, so the whole result
    // declines. The reference's entered-block scope leaves `;`-locals
    // readable (it types this `x`'s arm through the outer binding, an
    // FP-shaped leak in its own output); hiding is the safe side.
    let (i, ty) = bound_type(
        &idx,
        b"v = [1]\nx = [1, 2].tap { |p; v| break v }\n",
        "x",
    );
    assert!(
        matches!(i.get(ty), Type::Dynamic(_)),
        "a `;`-local arm must hide the outer binding, got {:?}",
        i.get(ty)
    );
}

#[test]
fn tap_block_param_autosplat() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    // `|v, w|` over an array receiver spreads the element type across the
    // positionals (`BlockAutoSplat`), so `break v` contributes the
    // element union `1 | 2`, not the whole `[1, 2]` — the shape whose
    // same-class union the rule declines (upstream #1116 probe row:
    // `x.upcase` is silent in the reference).
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { |v, w| break v }\n", "x");
    let Type::Union(members) = i.get(ty) else {
        panic!("autosplatted arm must be the element union, got {:?}", i.get(ty));
    };
    let pins: Vec<i64> = members
        .iter()
        .filter_map(|&m| match i.get(m) {
            Type::Constant(Scalar::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(pins, vec![1, 2], "element union must pin 1 and 2, got {:?}", i.get(ty));
    // A single-parameter `|v|` does NOT splat — `break v` keeps the
    // receiver type (the ambiguous_param0 case of `splats?`).
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { |v| break v }\n", "x");
    assert!(
        idx.class_name_of(&i, ty) == Some("Array") || matches!(i.get(ty), Type::Tuple(_)),
        "|v| must keep the receiver, got {:?}",
        i.get(ty)
    );
    // A String receiver is not an array carrier — `|v, w|` leaves the
    // first positional on the whole SELF type, which is the receiver's
    // nominal (`receiver_descriptor` projects `Constant("ab")` to
    // `Nominal[String]`), never the value-pinned literal.
    let (i, ty) = bound_type(&idx, b"x = \"ab\".tap { |v, w| break v }\n", "x");
    assert_eq!(
        idx.class_name_of(&i, ty),
        Some("String"),
        "non-array receiver binds the first positional to the nominal self type, got {:?}",
        i.get(ty)
    );
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "self slot is not value-pinned, got {:?}",
        i.get(ty)
    );
    // The same nominal-self rule for the single-parameter form: a scalar
    // literal receiver binds `Integer`, not `Constant(1)` — the fix that
    // closed the `x == 1` always-truthy false positive.
    let (i, ty) = bound_type(&idx, b"x = 1.tap { |a| break a }\n", "x");
    assert_eq!(
        idx.class_name_of(&i, ty),
        Some("Integer"),
        "scalar self slot binds the class, got {:?}",
        i.get(ty)
    );
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "scalar self slot is not value-pinned, got {:?}",
        i.get(ty)
    );
    // And a tuple receiver's self type is `Array[union]` with the element
    // pins kept: `[1, 2]` -> `Array[1 | 2]`.
    let (i, ty) = bound_type(&idx, b"x = [1, 2].tap { |a| break a }\n", "x");
    let Type::Nominal { class, args } = i.get(ty) else {
        panic!("tuple self slot must be Nominal[Array[…]], got {:?}", i.get(ty));
    };
    assert_eq!(idx.class_name_for_id(*class), Some("Array"));
    let Type::Union(members) = i.get(args[0]) else {
        panic!("tuple self arg must be the element union, got {:?}", i.get(args[0]));
    };
    let pins: Vec<i64> = members
        .iter()
        .filter_map(|&m| match i.get(m) {
            Type::Constant(Scalar::Int(n)) => Some(*n),
            _ => None,
        })
        .collect();
    assert_eq!(pins, vec![1, 2], "element union must pin 1 and 2, got {:?}", i.get(ty));
}

#[test]
fn tap_safe_navigation_split() {
    let idx = CoreIndex::new();
    if !idx.class_has_method("Kernel", "tap") {
        return;
    }
    // `safe_navigation_call_type`: a LITERAL `nil&.tap` folds to nil;
    // an INFERRED-exactly-nil receiver keeps the plain pipeline and its
    // break arms — `x.frobnicate_zzz` fires `for "s"` in the reference.
    let (i, ty) = bound_type(&idx, b"x = nil&.tap { break \"s\" }\n", "x");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Nil));
    let (i, ty) = bound_type(
        &idx,
        b"c = nil\nx = c&.tap { break \"s\" }\n",
        "x",
    );
    assert_eq!(
        i.get(ty),
        &Type::Constant(Scalar::Str("s".to_string())),
        "inferred-nil receiver keeps the plain pipeline, got {:?}",
        i.get(ty)
    );
}

#[test]
fn unknown_constant_new_is_dynamic() {
    // `Widget.new` where Widget is neither source nor RBS ⇒ Dynamic (silent).
    let ast = lower_src(b"w = Widget.new\nw.foo\n");
    let idx = CoreIndex::new();
    let source = SourceIndex::build(&ast, &idx);
    let typer = Typer::with_source(&idx, &source);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let w_ty = *env.get("w").expect("w should be bound");
    assert_eq!(w_ty, i.untyped(), "unknown-constant .new must be Dynamic[top]");
}

/// ADR-0008: the tier-1 sidecar fallback. A `sidecar_foldable` call the Rust
/// core declines (`255.to_s(16)`) routes to a wired [`folding::RubyFolder`]
/// and interns its result as a `Constant`; with no folder it stays the nominal
/// RBS return (the sound subset). Deterministic — no real Ruby.
#[test]
fn type_call_routes_sidecar_foldable_to_folder() {
    struct MockFolder(Scalar);
    impl folding::RubyFolder for MockFolder {
        fn fold(&self, _r: &Scalar, _m: &str, _a: &[Scalar]) -> Option<Scalar> {
            Some(self.0.clone())
        }
    }

    let ast = lower_src(b"255.to_s(16)\n");
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let call_id = ast
        .iter()
        .find_map(|(id, n)| matches!(n, Node::Call { .. }).then_some(id))
        .expect("a call node");

    // With a folder: the declined-by-Rust base-arg `to_s` folds to the
    // folder's result.
    let mock = MockFolder(Scalar::Str("ff".into()));
    let typer = Typer::with_source_and_folder(&index, &source, Some(&mock));
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let ty = typer.type_of(&ast, call_id, &env, &mut i);
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Str("ff".into())));

    // Without a folder: the nominal `Integer#to_s -> String`, not a Constant
    // (the sound subset — no false constant).
    let typer2 = Typer::with_source(&index, &source);
    let ty2 = typer2.type_of(&ast, call_id, &env, &mut i);
    assert!(!matches!(i.get(ty2), Type::Constant(_)), "no folder ⇒ no constant");
}

// ------------------------------------------------------------------
// C3a: `self.class` nominal-return tail.
// ------------------------------------------------------------------

/// Type the call to `method` in `src` under a source+lexical-scope typer
/// (the full analyze wiring), returning its interned `Type`.
fn type_c3a_call(src: &[u8], method: &str) -> Type {
    let ast = lower_src(src);
    let idx = CoreIndex::new();
    let source = SourceIndex::build(&ast, &idx);
    let scopes = crate::lexical_scopes(&ast);
    let typer = Typer::with_source(&idx, &source).with_lexical_scopes(&scopes);
    let mut i = Interner::new();
    let env = typer.build_toplevel_env(&ast, &mut i);
    let call = find_call(&ast, method);
    let ty = typer.type_of(&ast, call, &env, &mut i);
    i.get(ty).clone()
}

#[test]
fn self_class_itself_is_not_witnessable_singleton() {
    // `self.class` must NOT type to a project `Singleton` — that would route
    // `self.class.<class_method>` through class-method witnessing and FP on
    // every project-defined class method. It stays Dynamic (silent).
    let ty = type_c3a_call(b"class Foo\n  def bar\n    self.class\n  end\nend\n", "class");
    assert!(!matches!(ty, Type::Singleton(_)), "self.class must stay Dynamic, got {ty:?}");
}

#[test]
fn self_class_name_and_to_s_are_string() {
    // `self.class.name` / `self.class.to_s` → `Nominal[String]` (the
    // `Module#name : String?` optional is unwrapped for witnessing).
    for (src, m) in [
        (b"class Foo\n  def bar\n    self.class.name\n  end\nend\n".as_slice(), "name"),
        (b"class Foo\n  def bar\n    self.class.to_s\n  end\nend\n".as_slice(), "to_s"),
    ] {
        let ty = type_c3a_call(src, m);
        let idx = CoreIndex::new();
        let mut i = Interner::new();
        let interned = i.intern(ty.clone());
        assert_eq!(
            idx.class_name_of(&i, interned),
            Some("String"),
            "self.class.{m} must be String, got {ty:?}"
        );
    }
}

#[test]
fn self_class_name_string_in_nested_class() {
    // Deeply nested enclosing class still resolves the tail to String.
    let ty = type_c3a_call(
        b"module Outer\n  class Runner\n    def k\n      self.class.name\n    end\n  end\nend\n",
        "name",
    );
    let idx = CoreIndex::new();
    let mut i = Interner::new();
    let interned = i.intern(ty.clone());
    assert_eq!(idx.class_name_of(&i, interned), Some("String"), "got {ty:?}");
}

#[test]
fn self_class_at_toplevel_declines() {
    // No enclosing class ⇒ `self.class` declines to Dynamic (silent), so the
    // tail never becomes String — matches the reference's toplevel silence.
    let ty = type_c3a_call(b"self.class.name\n", "class");
    assert!(!matches!(ty, Type::Singleton(_)), "toplevel self.class must not type Singleton, got {ty:?}");
    let name_ty = type_c3a_call(b"self.class.name\n", "name");
    let idx = CoreIndex::new();
    let mut i = Interner::new();
    let interned = i.intern(name_ty.clone());
    assert_ne!(idx.class_name_of(&i, interned), Some("String"), "toplevel tail must not be String");
}

#[test]
fn self_class_name_string_even_in_core_shadow_class() {
    // A nested class whose WRITTEN name shadows a core class (`Time`) still
    // resolves `self.class.name` → String (no `Singleton` is minted, so there
    // is no core-shadow witnessing hazard) — matching the reference, which
    // fires the String tail here too.
    let ty = type_c3a_call(
        b"module Shadowing\n  class Time\n    def bar\n      self.class.name\n    end\n  end\nend\n",
        "name",
    );
    let idx = CoreIndex::new();
    let mut i = Interner::new();
    let interned = i.intern(ty.clone());
    assert_eq!(idx.class_name_of(&i, interned), Some("String"), "got {ty:?}");
}

#[test]
fn core_singleton_name_is_string() {
    // Bonus: `name`/`to_s` on a core-RBS `Singleton` (`Time.name`) → String.
    let ty = type_c3a_call(b"class Foo\n  def bar\n    Time.name\n  end\nend\n", "name");
    let idx = CoreIndex::new();
    let mut i = Interner::new();
    let interned = i.intern(ty.clone());
    assert_eq!(idx.class_name_of(&i, interned), Some("String"), "Time.name must be String, got {ty:?}");
}


#[test]
fn link_writes_read_case_begin_rescue_and_rescue_modifier_arms() {
    // rigor-rs#603: `each_may_be` treats every `selection_branches`
    // construct the same — `case`/`case in`, `begin`/`rescue` clauses,
    // and the rescue modifier — not only `if`/`||`/`&&`.
    for (src, want) in [
        (b"f = [1]\ng = case c\nwhen true then f\nelse [3]\nend\n" as &[u8], true),
        (b"f = [1]\ng = case x\nin Integer then f\nelse nil\nend\n", true),
        (b"f = [1]\ng = begin\nf\nrescue\nnil\nend\n", true),
        (b"f = [1]\ng = (f rescue nil)\n", true),
        // `ensure` is not a selection branch — its statements do not
        // contribute; the main body's `f` still does.
        (b"f = [1]\ng = begin\nf\nensure\nnil\nend\n", true),
        // A `while` body is not a selection — no link.
        (b"f = [1]\ng = while c\nf\nend\n", false),
    ] {
        let ast = lower_src(src);
        let links = &flow_writes::collect_indexed_flow(&ast).alias_links;
        let has = links.iter().any(|(_, to, from)| to == "g" && from == "f");
        assert_eq!(has, want, "{}", String::from_utf8_lossy(src));
    }
}

#[test]
fn a_write_value_names_itself_not_its_source() {
    // `may_be_locals` answers the write's NAME upstream — `g = (h = f)`
    // links `g` to `h`, and `h` to `f` by its own bind.
    let ast = lower_src(b"f = [1]\ng = (h = f)\n");
    let links = &flow_writes::collect_indexed_flow(&ast).alias_links;
    let pairs: Vec<(&String, &String)> = links
        .iter()
        .map(|(_, to, from)| (to, from))
        .collect();
    let g: String = "g".into();
    let h: String = "h".into();
    let f: String = "f".into();
    assert!(pairs.contains(&(&g, &h)));
    assert!(pairs.contains(&(&h, &f)));
    assert!(!pairs.contains(&(&g, &f)));
}

#[test]
fn a_mutation_inside_its_receiver_reaches_an_earlier_link() {
    // rigor-rs#603: upstream keys the reach on `write.end <=
    // call.message_loc.start` — a copy inside the mutator's RECEIVER ran
    // before `<<` fired; one inside a later argument did not.
    let st = |src: &[u8]| flow_writes::stale_local_names(&lower_src(src));
    let t = st(b"f = [1]\ng = f\n(g2 = f) << 2\n");
    assert!(t.stale("g2"));
    assert!(t.stale("f"));
    // `f << (h = g)` — the `h`-copy runs after `<<`, so `h` keeps its pin
    // while `g` (linked earlier) floors.
    let t = st(b"f = [1]\ng = f\nf << (h = g)\n");
    assert!(t.stale("f"));
    assert!(t.stale("g"));
    assert!(!t.stale("h"));
}

#[test]
fn a_selection_receiver_names_each_arms_local() {
    // `mutated_reads` walks `ReceiverAlias.candidates`: `if` arm lasts,
    // logical operands, local writes/op-writes, and the last statement of
    // a multi-statement parens — `begin`/`else`/`in` carriers name
    // nothing.
    let st = |src: &[u8]| flow_writes::stale_local_names(&lower_src(src));
    assert!(st(b"g = [1]\nh = [2]\n(c ? g : h) << 2\n").stale("g"));
    assert!(st(b"g = [1]\nh = [2]\n(c ? g : h) << 2\n").stale("h"));
    assert!(st(b"(g ||= f) << 2\n").stale("g"));
    assert!(st(b"g = [1]\n(x(); g) << 2\n").stale("g"));
    // A real `begin` as the receiver names no local.
    assert!(!st(b"g = [1]\n(begin g; end) << 2\n").stale("g"));
    // `g.send(:<<, 2) { }` carries a block — the literal-target send
    // declines, so no receiver mutation at all.
    assert!(!st(b"f = [1]\ng = f\ng.send(:<<, 2) { }\n").stale("f"));
    // `[]=` through a selection marks each arm's local too.
    assert!(st(b"g = [1]\nh = [2]\n(c ? g : h)[0] = 9\n").stale("h"));
}


/// rigor-rs#435 (ADR-56 slice B): a loop body that does not converge inside
/// the reference's 3-pass `BodyFixpoint` widens EVERY tracked name on the cap
/// pass — `Constant -> Nominal` — so a dead-arm write (`if false; t = nil;
/// end`) must not leave `t` value-pinned where the reference already lost the
/// pin. The snapshot of `if t` after the loop must NOT be a `Type::Constant`.
#[test]
fn loop_cap_widens_tracked_names_when_body_never_converges() {
    let src = b"def f(c)\n  t = 1\n  y = nil\n  while c\n    if false\n      t = nil\n    end\n    y = [y]\n  end\n  if t\n    y = 3\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "t must shed its pin once a tracked name fails to converge, got {:?}",
        i.get(ty)
    );
}

/// The convergent control for `loop_rebind_fixpoint`: a body whose tracked
/// names settle inside two passes (`y = 2` joins `nil | 2` once, then holds)
/// keeps the constant pins — `if t` still folds to `Constant[1]` and
/// `flow.always-truthy-condition` fires on both engines.
#[test]
fn loop_converged_body_keeps_constant_pins() {
    let src = b"def f(c)\n  t = 1\n  y = nil\n  while c\n    if false\n      t = nil\n    end\n    y = 2\n  end\n  if t\n    y = 3\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(1)));
}

/// rigor-rs#437: the canonical fold memo answers only EMPTY-`seen` folds;
/// a nested fold that ran out of fuel inside another predicate's walk
/// (`seen.len() >= 4` → `None`) can no longer poison the scan's own visit.
/// Here `if d`'s first fold comes from `s = t`'s reach walk (a non-empty
/// `seen`), which starved the arm kill and memoized `None`; with the
/// canonical/contextual split the scan's own visit refolds canonically,
/// `if d` records its dead `else` arm, and the loop pass replays only the
/// live `then` arm — `y = 2` absorbs `if s`'s conditional write, the body
/// converges inside two passes, and `t`'s `Constant[1]` survives for
/// `if t` to fold (the oracle fires `always-truthy-condition`).
#[test]
fn loop_decided_if_arm_keeps_pin_after_fuel_starved_first_fold() {
    let src = b"def f(c)\n  a = 1; b = a; cc = b; d = cc; s = nil; t = 1\n  while c\n    if s then y = 1 end\n    if d then y = 2 else t = nil end\n    s = t\n  end\n  if t then y = 3 end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(1)));
}

/// rigor-rs#437 review: a name the port CANNOT type before the loop
/// (`s ||= nil` — the replay doesn't model the write, so `s`'s seed is
/// `Dynamic`) where the reference knows it is `nil`: the seed-exactness
/// gate must decline the fixpoint, or the pass converges a turn early and
/// keeps `t`'s pin where the reference's cap widen sheds it. Reviewer
/// counterexample: `if t` must NOT fold.
#[test]
fn loop_untyped_seed_still_forces_structural_cap() {
    let src = b"def f(c, e)\n  d = 1; t = 1\n  s = nil\n  s ||= nil\n  while c\n    if d then y = 2 else t = nil end\n    if s then y = 1 end\n    s = t\n  end\n  if t then y = 3 end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "an untyped seed must keep the structural cap, got {:?}",
        i.get(ty)
    );
}

/// The order-sensitive twin (rigor-rs#437, oracle-probed): with the
/// CONDITIONAL write last (`if s` after `if d`), `y` is still moving on the
/// cap pass, so the widen sheds every tracked pin — `if t` must NOT fold.
#[test]
fn loop_decided_if_before_conditional_write_sheds_pins_at_cap() {
    let src = b"def f(c)\n  a = 1; b = a; cc = b; d = cc; s = nil; t = 1\n  while c\n    if d then y = 2 else t = nil end\n    if s then y = 1 end\n    s = t\n  end\n  if t then y = 3 end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "a name still moving on the cap pass sheds every sibling pin, got {:?}",
        i.get(ty)
    );
}

/// rigor-rs#638: a value-stabilizing self-op (`x |= 1`) converges inside
/// two BodyFixpoint passes, so the last-pass widen never runs and a sibling
/// dead-arm pin stays `Constant[1]`.
#[test]
fn loop_stabilizing_op_write_keeps_constant_pins() {
    let src = b"def f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x |= 1\n  end\n  if t\n    y = 3\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert_eq!(i.get(ty), &Type::Constant(Scalar::Int(1)));
}


/// rigor-rs#638 review: the replay verdict is only trusted when every
/// tracked pass exit is value-exact. `x = [1, 2].fetch(x)` declines in
/// the port (`fetch` types `Dynamic`), and `join(Dynamic, Dynamic)`
/// makes the name LOOK stable — so the run falls back to the structural
/// criterion, which caps the sibling pin exactly as the reference's
/// last-pass widen does. (The oracle resolves `fetch`, so it caps too;
/// a receiverless `g(x)`-style decline converges instead — rigor-rs#683.)
#[test]
fn loop_fixpoint_declined_exit_falls_back_to_structural_cap() {
    let src = b"def f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x = [1, 2].fetch(x)\n  end\n  if t\n    y = 3\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "a declined tracked exit must not read as converged, got {:?}",
        i.get(ty)
    );
}

/// Same gate, decision-decline shape: `i += 1 if i < 3` guards on a typed
/// local, so the pass declines `i` and the structural cap sheds `t`'s pin
/// (the reference is silent on `if t` here).
#[test]
fn loop_fixpoint_guarded_op_write_falls_back_to_structural_cap() {
    let src = b"def f(c)\n  t = 1; i = 0\n  while c\n    if false; t = nil; end\n    i += 1 if i < 3\n  end\n  if t\n    y = 3\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "a guard-read decline must not read as converged, got {:?}",
        i.get(ty)
    );
}

/// A logical-growth RHS (`x = (x || 0) + 1`) is a self-read the fold
/// cannot decide — the exit declines and the structural cap stands.
#[test]
fn loop_fixpoint_logical_growth_falls_back_to_structural_cap() {
    let src = b"def f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x = (x || 0) + 1\n  end\n  if t\n    y = 3\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "a growing self-read must not read as converged, got {:?}",
        i.get(ty)
    );
}

/// rigor-rs#683: a `||`-topped self-write is not blanket-convergent —
/// `x = [1, 2][x] || 0` keeps moving (the operand's read can evaluate
/// while `x` is truthy), so the cap sheds `t`'s pin; the monotone
/// `x = x || 0` control must still read as converged.
#[test]
fn loop_cap_compounds_logical_operand_self_reads() {
    for (src, converged) in [
        // Operand self-read that can run while `x` is truthy: caps.
        (b"def f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x = [1, 2][x] || 0\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // The monotone `x = x || v` shape: converges, `t` keeps its pin.
        (b"def f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x = x || 0\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), true),
        // `||`'s right operand evaluates only while `x` is falsey —
        // bounded, converges.
        (b"def f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x = x || [1, 2][x]\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), true),
        // A nominal codomain is not convergence: `x.bit_length` types
        // `Integer` but the reference folds a fresh constant per pass.
        (b"def f(c)\n  t = 1; x = 8\n  while c\n    if false; t = nil; end\n    x = x.bit_length\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // A CONDITIONAL write does not cut the assumption: `x = 0 if d`
        // leaves `x`'s pass-entry binding live for `[x]`'s read.
        (b"def f(c)\n  t = 1; x = 0\n  while c\n    x = 0 if d\n    if false; t = nil; end\n    x = [x]\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // An unresolvable receiverless call declines to a stable
        // `Dynamic` union — converged, `t` keeps its pin.
        (b"def f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x = foo(x)\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), true),
        // A `for` index binding a leaf's foreign name makes the leaf
        // unprovable — `x + k - 1` on `k = 0` moves, it does not fold
        // to the seed's `x + 1 - 1`.
        (b"def f(c)\n  t = 1; x = 0; k = 1\n  while c\n    if false; t = nil; end\n    for k in [0]; end\n    x = x + k - 1\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
    ] {
        let ast = lower_src(src);
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let snaps = typer.always_truthy_snapshots(&ast, &mut i);
        let last_if = ast
            .iter()
            .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
            .last()
            .expect("at least one if node");
        let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
        assert_eq!(
            matches!(i.get(ty), Type::Constant(_)),
            converged,
            "converged={converged} expected, got {:?} for {:?}",
            i.get(ty),
            String::from_utf8_lossy(src)
        );
    }

    // A receiverless call a toplevel `def` RESOLVES is not a shared
    // decline — `g` compounds the way the reference resolves it to.
    let src = b"def g(v) = [v]\ndef f(c)\n  t = 1; x = 0\n  while c\n    if false; t = nil; end\n    x = g(x)\n  end\n  if t\n    y = 3\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let source = crate::source_index::SourceIndex::build(&ast, &index);
    let typer = Typer::with_source(&index, &source);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "a defined g(x) must compound, got {:?}",
        i.get(ty)
    );
}

/// A capped loop's widen lands on the POST-JOIN env, not the entry env:
/// pass the reference's way — `Integer | NilClass`, never a minted `nil` —
/// so `if w` keeps no `Constant`/`NilClass` pin to fire on (rigor-rs#435
/// review: the entry-env overlay turned this into an always-falsey FP).
#[test]
fn loop_cap_widen_preserves_body_writes_to_non_suspect_names() {
    let src = b"def f(c)\n  w = nil\n  i = 0\n  while c\n    w = 1\n    i += 1\n  end\n  if w\n    puts i\n  end\nend\n";
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    let ty = snaps.get(&last_if).copied().expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_))
            && index.class_name_of(&i, ty) != Some("NilClass"),
        "w must keep its body write (widened), got {:?}",
        i.get(ty)
    );
}

/// The `if t` snapshot helper shared by the rigor-rs#639 rows: `None` when
/// the predicate keeps a `Constant` pin (the diagnostic stays armed),
/// `Some(ty)` of the shed pin otherwise.
fn last_if_snapshot(src: &[u8]) -> (Interner, Option<TypeId>) {
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let snaps = typer.always_truthy_snapshots(&ast, &mut i);
    let last_if = ast
        .iter()
        .filter_map(|(id, n)| matches!(n, Node::If { .. }).then_some(id))
        .last()
        .expect("at least one if node");
    (i, snaps.get(&last_if).copied())
}

/// rigor-rs#639: a `break`/`next`/`redo`/`retry` or `begin…end while`
/// body cannot be replayed, so the JOIN keeps declining — but the
/// structural cap check is a static scan and still applies: a
/// non-convergent body's locals shed every tracked pin exactly as the
/// reference's cap pass widens them.
#[test]
fn loop_cap_still_fires_through_jump_and_once_bodies() {
    for (src, converged) in [
        // `next` keeps the cap: `y = [y]` never converges → `t` sheds.
        (b"def f(c, k)\n  t = 1; y = nil\n  while c\n    next if k\n    if false; t = nil; end\n    y = [y]\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // `break` the same.
        (b"def f(c, k)\n  t = 1; y = nil\n  while c\n    if false; t = nil; end\n    y = [y]\n    break if k\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // `redo` the same.
        (b"def f(c, k)\n  t = 1; y = nil\n  while c\n    if false; t = nil; end\n    y = [y]\n    redo if k\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // A `begin…end while` body runs at least once — the same gate.
        (b"def f(c)\n  t = 1; y = nil\n  begin\n    if false; t = nil; end\n    y = [y]\n  end while c\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // The convergent control: `y = 2` still settles, so `t` keeps its
        // pin through a `next` body (both engines fire).
        (b"def f(c, k)\n  t = 1; y = nil\n  while c\n    next if k\n    if false; t = nil; end\n    y = 2\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), true),
    ] {
        let (i, ty) = last_if_snapshot(src);
        let ty = ty.expect("predicate snapshot recorded");
        assert_eq!(
            !matches!(i.get(ty), Type::Constant(_)),
            !converged,
            "converged={converged} expected, got {:?} for {:?}",
            i.get(ty),
            String::from_utf8_lossy(src)
        );
    }
}

/// rigor-rs#639: `loop_body_ivar_writes` counts an ivar the scope binds off
/// a WRITE (`rebindable_ivar?`) — `@x += v` (an `UnmodeledWrite`), a plain
/// `@x = …`, `@x ||= v` self-reads, and a literal-name
/// `instance_variable_set` — so a body that keeps an ivar moving caps its
/// tracked siblings too. The controls: an ivar bound only BY the loop
/// (`@y = [@y]` with no prior write) is not rebindable and leaves the cap
/// alone; a non-literal `instance_variable_set` names no ivar.
#[test]
fn loop_cap_tracks_rebindable_ivar_writes() {
    for (src, converged) in [
        // `@pos += 1` keeps moving — the write the replay cannot model
        // forces the structural cap.
        (b"def f(c)\n  t = 1; @pos = 0\n  while c\n    if false; t = nil; end\n    @pos += 1\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // A plain `@pos = @pos + 1` moves every pass.
        (b"def f(c)\n  t = 1; @pos = 0\n  while c\n    if false; t = nil; end\n    @pos = @pos + 1\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // `@pos = [@pos]` grows.
        (b"def f(c)\n  t = 1; @pos = 0\n  while c\n    if false; t = nil; end\n    @pos = [@pos]\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // `instance_variable_set(:@pos, [@pos])` on `self` is a literal
        // ivar write (`ReflectiveIvarWrite`).
        (b"def f(c)\n  t = 1; @pos = 0\n  while c\n    if false; t = nil; end\n    instance_variable_set(:@pos, [@pos])\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), false),
        // An ivar bound only BY the loop is not rebindable — no cap, `t`
        // keeps its pin (the oracle fires `always-truthy-condition`).
        (b"def f(c)\n  t = 1; y = nil\n  while c\n    if false; t = nil; end\n    @y = [@y]\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), true),
        // `instance_variable_set(:@pos, 1)` on an UNBOUND ivar names no
        // rebindable write — the oracle keeps firing.
        (b"def f(c)\n  t = 1\n  while c\n    if false; t = nil; end\n    instance_variable_set(:@pos, 1)\n  end\n  if t\n    y = 3\n  end\nend\n".as_slice(), true),
    ] {
        let (i, ty) = last_if_snapshot(src);
        let ty = ty.expect("predicate snapshot recorded");
        assert_eq!(
            !matches!(i.get(ty), Type::Constant(_)),
            !converged,
            "converged={converged} expected, got {:?} for {:?}",
            i.get(ty),
            String::from_utf8_lossy(src)
        );
    }
}

/// rigor-rs#639: `moved` reads the body's OWN writes, not the entry env —
/// `k = y` types at the write's position where `y = [k]` already landed,
/// so `k` is a pass-1 mover and the cap sheds `t`'s pin (the entry-env
/// verdict saw `k`'s unbound seed and called it converged).
#[test]
fn loop_cap_moved_reads_the_body_binding() {
    let src = b"def f(c)\n  t = 1; y = nil; k = nil\n  while c\n    if false; t = nil; end\n    y = [k]\n    k = y\n  end\n  if t\n    y = 3\n  end\nend\n";
    let (i, ty) = last_if_snapshot(src);
    let ty = ty.expect("predicate snapshot recorded");
    assert!(
        !matches!(i.get(ty), Type::Constant(_)),
        "k = y must read the body's own y, got {:?}",
        i.get(ty)
    );
}

#[test]
fn nested_alternating_logical_guard_is_linear() {
    // rigor-rs#453: `subject_edge` re-evaluated a Logical's left operand on
    // the non-sequential edge, so 34 nested `&&`/`||` terms took ~14 s in
    // release. Both edges in one pass is linear in the predicate.
    let mut expr = String::from("ENV.key?(\"k0\")");
    for i in 1..40 {
        let op = if i % 2 == 1 { "&&" } else { "||" };
        expr = format!("({expr} {op} x{i})");
    }
    let src = format!("if {expr}\n  1\nend\n");
    let ast = lower_src(src.as_bytes());
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let start = std::time::Instant::now();
    let _dead = typer.guard_suppressed_calls(&ast, &mut i);
    let dt = start.elapsed();
    assert!(
        dt.as_millis() < 500,
        "40 nested Logical terms took {dt:?} (must stay linear)"
    );
}

/// rigor-rs#1013 — `join_arm_writes_bound`'s memberwise join must intersect a
/// `\u{1f}`-keyed indexed-narrowing record on PRESENCE, not nil-inject or
/// resurrect it: an arm whose `h = {}` rebind invalidates the slot
/// (`bind_statement`'s `drop_indexed_narrowings`) leaves the record absent in
/// that arm's scope, and the reference's `join_bindings` keeps a record only
/// when EVERY joined scope binds it. The replay envs the check surface hands
/// `bind_statements_into` (`branch_value_type`'s arm envs) do carry the record.
#[test]
fn arm_join_drops_indexed_narrowing_record_an_arm_rebind_killed() {
    let entry_ast = lower_src(b"h = {}\nh[:a] ||= \"s\"\n");
    let index = CoreIndex::new();
    let typer = Typer::new(&index);
    let mut i = Interner::new();
    let entry = typer.build_toplevel_check_env(&entry_ast, &mut i);
    let key = crate::flow_writes::indexed_narrowing_key("h", &ShapeKey::Sym("a".into()));
    assert!(
        entry.contains_key(key.as_str()),
        "the check env records the operand `||=` slot"
    );

    // `if $c; h = {}; end` — the then-arm rebinds `h`, dropping its slot
    // records; the empty else arm leaves `entry` untouched. The joined env
    // must not keep `entry`'s record through the absent-arm fallback.
    let ast = lower_src(b"if $c\n  h = {}\nend\n");
    let Node::Program { body, .. } = ast.get(ast.root()) else {
        panic!("program body")
    };
    let joined = typer.bind_statements_into(&ast, &entry, body, &mut i);
    assert!(
        !joined.contains_key(key.as_str()),
        "a record one arm dropped must not survive the join"
    );
}

