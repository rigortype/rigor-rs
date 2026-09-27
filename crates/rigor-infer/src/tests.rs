use super::*;
use rigor_parse::{lower, parse, Node};
use rigor_types::{Scalar, ShapeKey, ShapeMember, Type};

fn lower_src(src: &[u8]) -> LoweredAst {
    lower(&parse(src))
}

#[test]
fn mutator_tables_match_the_pinned_reference() {
    // Sizes at pin `e59b7b89` (`mutation_widening.rb`, `string_mutation.rb`,
    // `hash_lookup_mutation.rb`) — the same sets the effect catalogue's
    // `mutators.yml` is extracted from, so a drift here and a
    // `vendor_effects.py --check` failure arrive together.
    assert_eq!(ARRAY_MUTATORS.len(), 31);
    assert_eq!(HASH_MUTATORS.len(), 16);
    assert!(HASH_MUTATORS.contains(&"shift"));
    assert_eq!(STRING_MUTATORS.len(), 35);
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
    case(b"[1, 2][9]\n", Type::Constant(Scalar::Nil)); // out of bounds → nil
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
    assert_eq!(env.get("w"), Some(&untyped));
    assert_eq!(env.get("n"), Some(&untyped));
    assert_eq!(get("k", &i), Some(Type::Constant(Scalar::Str("s".into()))));
    assert_eq!(get("r", &i), Some(Type::Constant(Scalar::Str("t".into()))));
    assert_eq!(get("d", &i), Some(Type::Constant(Scalar::Str("s".into()))));
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
