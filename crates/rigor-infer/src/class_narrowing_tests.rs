use super::*;
use rigor_parse::{lower, parse, Node};

fn lower_src(src: &[u8]) -> LoweredAst {
    lower(&parse(src))
}

/// The class-narrowing snapshot map for `src`, wired exactly as the analyze
/// pass wires it (per-file source index + lexical scopes, so the shadow
/// gate is live).
fn class_snaps(src: &[u8]) -> (LoweredAst, HashMap<NodeId, String>) {
    let ast = lower_src(src);
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let scopes = lexical_scopes(&ast);
    let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
    let mut i = Interner::new();
    let snaps = typer.class_narrowing_snapshots(&ast, &mut i);
    (ast, snaps)
}

/// The node id of the first call named `method`, or panic.
fn call_named(ast: &LoweredAst, method: &str) -> NodeId {
    ast.iter()
        .find_map(|(id, n)| match n {
            Node::Call { method: m, .. } if m == method => Some(id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("call `{method}` present"))
}

/// a1: `if value.is_a?(Hash)` narrows the branch use; the use AFTER the
/// `if` (no terminating opposite branch) stays un-narrowed.
#[test]
fn class_narrowing_if_branch_narrows_use_after_does_not() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash)\n    value.frobnicate_zzz\n  end\n  value.after_zzz\nend\n",
    );
    assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
    assert!(!snaps.contains_key(&call_named(&ast, "after_zzz")));
}

/// a2: a ternary narrows its truthy arm only (the falsey arm is UNCHANGED).
#[test]
fn class_narrowing_ternary_truthy_arm_only() {
    let (ast, snaps) = class_snaps(
        b"def f(rule)\n  rule.is_a?(Hash) ? rule.frobnicate_zzz : rule.other_zzz\nend\n",
    );
    assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
    assert!(!snaps.contains_key(&call_named(&ast, "other_zzz")));
}

/// a4: rebinding the local inside the branch invalidates the narrowing for
/// subsequent uses (`scope.rb:194`).
#[test]
fn class_narrowing_rebind_in_branch_invalidates() {
    let (ast, snaps) = class_snaps(
        b"def f(value, other)\n  if value.is_a?(Hash)\n    value = other\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// a5: a TERMINATING opposite branch propagates the truthy edge past the
/// guard (`eval_if:481` early-return narrowing) — both the `return` and the
/// `raise` idiom.
#[test]
fn class_narrowing_early_return_propagates() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  return unless value.is_a?(Hash)\n  value.frobnicate_zzz\nend\n",
    );
    assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));

    let (ast, snaps) = class_snaps(
        b"def f(value)\n  raise ArgumentError unless value.is_a?(Hash)\n  value.frobnicate_zzz\nend\n",
    );
    assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
}

/// a5 counterpart: a rebind AFTER the propagated guard kills the fact.
#[test]
fn class_narrowing_rebind_after_guard_invalidates() {
    let (ast, snaps) = class_snaps(
        b"def f(value, other)\n  return unless value.is_a?(Hash)\n  value = other\n  value.frobnicate_zzz\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// `kind_of?` and `instance_of?` route identically (`narrowing.rb:979`).
#[test]
fn class_narrowing_kind_of_and_instance_of_narrow() {
    for method in ["kind_of?", "instance_of?"] {
        let src = format!(
            "def f(value)\n  if value.{method}(Hash)\n    value.frobnicate_zzz\n  end\nend\n"
        );
        let (ast, snaps) = class_snaps(src.as_bytes());
        assert_eq!(
            snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str),
            Some("Hash"),
            "{method} must narrow"
        );
    }
}

/// Decline: facts do NOT enter a block body (ADR-0038 §3) — but the
/// receiver of the block-bearing call itself, OUTSIDE the block, records.
#[test]
fn class_narrowing_block_body_declines_receiver_outside_records() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash)\n    value.deep_frobnicate_zzz { |k| value.inner_zzz }\n  end\nend\n",
    );
    assert_eq!(
        snaps.get(&call_named(&ast, "deep_frobnicate_zzz")).map(String::as_str),
        Some("Hash")
    );
    assert!(!snaps.contains_key(&call_named(&ast, "inner_zzz")));
}

/// Stage 3a-1 REPLACED the blanket `&&`/`||` decline: one recognised
/// conjunct now narrows the truthy edge (probe c1a, reference fires), while
/// the falsey edge of the same predicate still narrows nothing (c1g). The
/// full matrix lives in
/// [`class_narrowing_tests::class_narrowing_stage3a1_compound_predicate_matrix`].
#[test]
fn class_narrowing_logical_predicate_narrows_truthy_only() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash) && value.foo\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert_eq!(
        snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str),
        Some("Hash")
    );
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash) && value.foo\n    1\n  else\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// Decline: a project declaration shadowing the constant kills the
/// narrowing entirely (never narrow to the project nominal in this slice).
#[test]
fn class_narrowing_shadowed_constant_declines() {
    let (ast, snaps) = class_snaps(
        b"class Hash\nend\ndef f(value)\n  if value.is_a?(Hash)\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// Decline: a local with a concrete (non-Dynamic/Top) carrier is untouched
/// (`narrow_class_other` narrows Dynamic/Top ONLY).
#[test]
fn class_narrowing_non_dynamic_local_declines() {
    let (ast, snaps) = class_snaps(
        b"value = \"str\"\nif value.is_a?(Hash)\n  value.frobnicate_zzz\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// Decline: a mutator call on the local kills the fact for SUBSEQUENT uses
/// (the mutator call itself still records — its receiver read precedes the
/// mutation).
#[test]
fn class_narrowing_mutator_call_invalidates_subsequent_uses() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash)\n    value.merge!(a: 1)\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// R1 decline: an expression-position rebind in an argument list threads
/// IMMEDIATELY — `f(value = x, value.frobnicate_zzz)` inside an `is_a?`
/// branch must record nothing (Ruby evaluates arguments left-to-right, so
/// the second argument reads the rebound local).
#[test]
fn class_narrowing_arg_position_rebind_invalidates_sibling_use() {
    let (ast, snaps) = class_snaps(
        b"def f(value, x)\n  if value.is_a?(Hash)\n    g(value = x, value.frobnicate_zzz)\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// R2 decline: early-return propagation is STATEMENT-position only — an
/// expression-position conditional with a terminating falsey arm
/// (`f(value.is_a?(Hash) ? value : raise)`) must NOT narrow the statements
/// after it (unprobed oracle behavior). The statement-position a5 idiom
/// (its own test above) keeps propagating.
#[test]
fn class_narrowing_expression_position_if_never_propagates() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  g(value.is_a?(Hash) ? value : raise)\n  value.frobnicate_zzz\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    // Assignment-RHS position is expression position too.
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  y = value.is_a?(Hash) ? value : raise\n  value.frobnicate_zzz\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// R3 decline: a nested guard CONFLICTING with the outer fact
/// (`if v.is_a?(Hash)` then inner `if v.is_a?(String)`) narrows nothing —
/// the reference's carrier is `Nominal[Hash]` at the inner guard (Bot on a
/// disjoint re-narrow), out of the Dynamic-only envelope. A SAME-class
/// re-guard keeps the fact.
#[test]
fn class_narrowing_nested_conflicting_guard_declines() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash)\n    if value.is_a?(String)\n      value.frobnicate_zzz\n    end\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    // Same-class re-guard: the fact survives (a no-op re-narrowing).
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash)\n    if value.is_a?(Hash)\n      value.frobnicate_zzz\n    end\n  end\nend\n",
    );
    assert_eq!(
        snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str),
        Some("Hash")
    );
}

/// The POSITION matrix (docs/notes/20260807-block-narrowing-position-rule
/// .md): a block body and a `case`/`when` clause narrow ONLY from statement
/// position or an assignment RHS; a receiver, an argument or a `return`
/// operand narrows nothing. `if`/ternary is the exception (p4, p8 narrow in
/// every position). Every row was measured against the pinned reference —
/// `Some(c)` means the reference FIRES and rigor-rs must record `c`, `None`
/// means the reference is SILENT and rigor-rs must record nothing.
///
/// Safe-nav is NOT the axis: s3/s4 (`h&.transform_values { … }` in
/// statement position) fire on both engines, which is why PR #63's
/// `if !safe_nav` block decline was wrong.
#[test]
fn class_narrowing_position_matrix() {
    // (row, source, expected narrowed class of the `frobnicate_zzz` call)
    let rows: &[(&str, &[u8], Option<&str>)] = &[
        // --- block bodies: statement position / assignment RHS -> narrows
        ("s1", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\nend\n", Some("String")),
        ("s2", b"def f(h)\n  h.transform_values do |v|\n    v.is_a?(String) ? v.frobnicate_zzz : v\n  end\nend\n", Some("String")),
        ("s3", b"def f(h)\n  h&.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\nend\n", Some("String")),
        ("s4", b"def f(h)\n  h&.transform_values do |v|\n    v.is_a?(String) ? v.frobnicate_zzz : v\n  end\nend\n", Some("String")),
        ("s8", b"def f(h)\n  x = h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  x\nend\n", Some("String")),
        ("s10", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  nil\nend\n", Some("String")),
        // --- block bodies: receiver / argument / `return` -> declines
        ("s5", b"def f(h)\n  h&.transform_values do |v|\n    v.is_a?(String) ? v.frobnicate_zzz : v\n  end&.compact\nend\n", None),
        ("s6", b"def f(h)\n  h&.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }&.compact\nend\n", None),
        ("s7", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }.compact\nend\n", None),
        ("s9", b"def g(y)\n  y\nend\n\ndef f(h)\n  g(h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v })\nend\n", None),
        ("s11", b"def f(h)\n  h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }.compact.to_a\nend\n", None),
        ("s12", b"def f(h)\n  return h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\nend\n", None),
        ("s13", b"def g(y)\n  y\nend\n\ndef f(h)\n  x = g(h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v })\n  x\nend\n", None),
        // --- `case`/`when`: the same positional rule
        ("p6", b"def f(v)\n  case v\n  when Hash\n    v.frobnicate_zzz\n  end\nend\n", Some("Hash")),
        ("p1", b"def f(v)\n  x = case v\n      when Hash\n        v.frobnicate_zzz\n      end\n  x\nend\n", Some("Hash")),
        ("p2", b"def g(y)\n  y\nend\n\ndef f(v)\n  g(case v\n    when Hash\n      v.frobnicate_zzz\n    end)\nend\n", None),
        ("p3", b"def f(v)\n  (case v\n   when Hash\n     v.frobnicate_zzz\n   end).to_s\nend\n", None),
        ("p7", b"def f(v)\n  return case v\n         when Hash\n           v.frobnicate_zzz\n         end\nend\n", None),
        // --- `if`/ternary: the EXCEPTION — narrows in every position
        ("p4", b"def g(y)\n  y\nend\n\ndef f(v)\n  g(v.is_a?(Hash) ? v.frobnicate_zzz : v)\nend\n", Some("Hash")),
        ("p8", b"def f(v)\n  (v.is_a?(Hash) ? v.frobnicate_zzz : v).to_s\nend\n", Some("Hash")),
        // --- nesting / carrier corners (all oracle-measured)
        ("p5", b"def f(h)\n  h.each do |a|\n    a.each do |v|\n      v.is_a?(String) ? v.frobnicate_zzz : v\n    end\n  end\nend\n", Some("String")),
        ("x1", b"def g(y)\n  y\nend\n\ndef f(h, k)\n  g(case k\n    when Integer\n      h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n    end)\nend\n", None),
        ("x2", b"def g(y)\n  y\nend\n\ndef f(h, k)\n  g(k ? h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v } : nil)\nend\n", None),
        ("x3", b"def f(h)\n  a, b = h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  [a, b]\nend\n", Some("String")),
        ("x4", b"def f(h, x)\n  x ||= h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  x\nend\n", Some("String")),
        ("x5", b"def f(h, k)\n  if k\n    h.transform_values { |v| v.is_a?(String) ? v.frobnicate_zzz : v }\n  end\nend\n", Some("String")),
    ];
    for (row, src, expected) in rows {
        let (ast, snaps) = class_snaps(src);
        let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
        assert_eq!(got, *expected, "position matrix row {row}");
    }
}

/// CARRIER FIDELITY (docs/notes/20260808-narrowing-carrier-fidelity-fp.md):
/// the `narrow_class_other` Dynamic-only gate is a SUBSET rule only over
/// carriers both engines type `Dynamic`/`Top`. rigor-rs collapses a long
/// tail of carriers to `Dynamic[top]` that the reference types precisely, so
/// on those our gate fires where theirs declines. [`coarse_locals`] +
/// [`narrowable_binding`] turn the gate into an ALLOW-list; this matrix pins
/// every measured member and every measured decline.
///
/// Every row is oracle-measured against the pinned reference from a fresh
/// cwd with `--no-cache`. `Some(c)` — the reference FIRES and rigor-rs must
/// record `c`. `None` — rigor-rs must record NOTHING, either because the
/// reference is SILENT (a would-be false positive: the `fp*` rows) or
/// because the decline costs coverage the reference has (the `cost*` rows,
/// a strict subset).
#[test]
fn class_narrowing_carrier_fidelity_matrix() {
    // The guard/use tail every row shares. `if`/ternary narrows in every
    // position (p4/p8), so the row's whole point is the local's BINDING.
    const G: &str = "  h.is_a?(Hash) ? h.frobnicate_zzz : h\n";
    let bound = |expr: &str| format!("def f(spec, list, cond)\n  h = {expr}\n{G}end\n");
    let rows: Vec<(&str, String, Option<&str>)> = vec![
        // ---- ALLOW-list: measured Dynamic on BOTH engines ---------------
        // A method / block / keyword / optional / rest / block parameter.
        ("ok_param", format!("def f(h)\n{G}end\n"), Some("Hash")),
        ("ok_kwarg", format!("def f(k: nil)\n  h = k\n{G}end\n"), Some("Hash")),
        ("ok_optarg", format!("def f(o = nil)\n  h = o\n{G}end\n"), Some("Hash")),
        ("ok_restarg", format!("def f(*a)\n  h = a\n{G}end\n"), Some("Hash")),
        ("ok_blockarg", format!("def f(&blk)\n  h = blk\n{G}end\n"), Some("Hash")),
        // `@ivar` / `$gvar` / `@@cvar` reads — the reference types none.
        ("ok_ivar", bound("@x"), Some("Hash")),
        ("ok_gvar", bound("$gx"), Some("Hash")),
        (
            "ok_cvar",
            format!("class C\n  def f\n    h = @@cx\n  {G}  end\nend\n"),
            Some("Hash"),
        ),
        // A call THROUGH a narrowable receiver (an untyped receiver resolves
        // no method on either side), incl. safe-nav, a block, `[]`, chains.
        ("ok_call", bound("spec.unknown_zzz"), Some("Hash")),
        ("ok_call_chain", bound("spec.foo_zzz.bar_zzz"), Some("Hash")),
        ("ok_call_index", bound("spec[0]"), Some("Hash")),
        ("ok_call_safenav", bound("spec&.dup"), Some("Hash")),
        ("ok_call_block", bound("spec.map { |x| x }"), Some("Hash")),
        ("ok_call_ivar_recv", bound("@obj.foo_zzz"), Some("Hash")),
        ("ok_call_gvar_recv", bound("$gobj.foo_zzz"), Some("Hash")),
        // Destructuring loses precision on BOTH sides — even from a Logical.
        ("ok_multiwrite", format!("def f(spec)\n  a, h = spec\n{G}end\n"), Some("Hash")),
        (
            "ok_multiwrite_logical",
            format!("def f(spec)\n  a, h = (spec || {{}})\n{G}end\n"),
            Some("Hash"),
        ),
        // ---- DECLINES that close a live FP (reference SILENT) -----------
        // `Logical`: the measured archetype. `analyse_or` builds a UNION.
        ("fp_or", bound("spec || {}"), None),
        ("fp_and", bound("spec && {}"), None),
        ("fp_or_nested", bound("(spec || other_zzz) || {}"), None),
        ("fp_paren", bound("(spec || {})"), None),
        ("fp_opwrite", format!("def f(spec)\n  h = spec\n  h ||= {{}}\n{G}end\n"), None),
        // A project method whose return TAIL is a `Logical` — reached
        // through an implicit-self call and through `self.`.
        (
            "fp_insource_logical",
            format!("def mk\n  unknown_zzz || {{}}\nend\n\ndef f\n  h = mk\n{G}end\n"),
            None,
        ),
        (
            "fp_self_insource_logical",
            format!(
                "class C\n  def f\n    h = self.mk\n  {G}  end\n\n  def mk\n    unknown_zzz || {{}}\n  end\nend\n"
            ),
            None,
        ),
        (
            "fp_recv_insource_logical",
            format!(
                "class D\n  def mk\n    unknown_zzz || {{}}\n  end\nend\n\ndef f\n  d = D.new\n  h = d.mk\n{G}end\n"
            ),
            None,
        ),
        // A loop's value (`nil` on the reference).
        ("fp_while", format!("def f(cond)\n  h = while cond\n    break({{}})\n  end\n{G}end\n"), None),
        ("fp_for", format!("def f(list)\n  h = for i in list\n    nil\n  end\n{G}end\n"), None),
        // `begin`/`rescue` and the `rescue` modifier — a UNION.
        (
            "fp_beginrescue",
            format!("def f(spec)\n  h = begin\n    spec\n  rescue StandardError\n    {{}}\n  end\n{G}end\n"),
            None,
        ),
        ("fp_rescue_mod", bound("(spec rescue {})"), None),
        // A `rescue => e` capture: the reference binds the exception CLASS.
        (
            "fp_rescue_bind",
            format!("def f\n  begin\n    nil\n  rescue StandardError => h\n  {G}  end\nend\n"),
            None,
        ),
        // `case`/`in` and `if`/ternary AS EXPRESSIONS — a UNION the
        // reference keeps and our `Algebra::join` collapses into Dynamic.
        (
            "fp_case",
            format!("def f(cond, spec)\n  h = case cond\n  when 1 then spec\n  else {{}}\n  end\n{G}end\n"),
            None,
        ),
        (
            "fp_case_in",
            format!("def f(cond, spec)\n  h = case cond\n  in Integer then spec\n  else {{}}\n  end\n{G}end\n"),
            None,
        ),
        ("fp_ternary", bound("cond ? spec : {}"), None),
        (
            "fp_if",
            format!("def f(cond, spec)\n  h = if cond\n    spec\n  else\n    {{}}\n  end\n{G}end\n"),
            None,
        ),
        // `Range`, `self`, a lambda/proc — all precisely typed by the
        // reference, all `Dynamic[top]` here.
        ("fp_range_lit", bound("(1..2)"), None),
        ("fp_range_dyn", bound("(spec..spec)"), None),
        ("fp_self", format!("class C\n  def f\n    h = self\n  {G}  end\nend\n"), None),
        ("fp_lambda", bound("->(x) { x }"), None),
        ("fp_proc", bound("proc { |x| x }"), None),
        // Kernel methods with a precise RBS return, reached receiverless —
        // the reason an implicit-self call cannot be allow-listed.
        ("fp_method_ref", bound("__method__"), None),
        ("fp_binding", bound("binding"), None),
        ("fp_caller", bound("caller"), None),
        ("fp_block_given", bound("block_given?"), None),
        // `defined?` (`String?`), a `*splat` (`Array`), a `return` operand.
        ("fp_defined", bound("defined?(spec)"), None),
        ("fp_splat", bound("*spec"), None),
        ("fp_return", bound("(return {} if cond)"), None),
        // A receiver the reference types precisely enough to resolve the
        // method on: `self`, a constant.
        ("fp_const_recv", bound("Float::INFINITY.abs"), None),
        // ---- DECLINES that COST coverage (reference FIRES) --------------
        ("cost_yield", format!("def f\n  h = yield\n{G}end\n"), None),
        ("cost_super", format!("class C\n  def f\n    h = super\n  {G}  end\nend\n"), None),
        ("cost_implicit_self", bound("unknown_zzz"), None),
        (
            "cost_implicit_self_insource",
            format!("def mk\n  unknown_zzz\nend\n\ndef f\n  h = mk\n{G}end\n"),
            None,
        ),
        ("cost_const_read", bound("XCONST_ZZZ"), None),
        ("cost_const_recv", bound("File.foo_zzz"), None),
        ("cost_str_recv", bound("\"s\".foo_zzz"), None),
        ("cost_self_recv", format!("class C\n  def f\n    h = self.unknown_zzz\n  {G}  end\nend\n"), None),
        (
            "cost_call_on_coarse",
            format!("def f(spec)\n  a = spec || {{}}\n  h = a.dup\n{G}end\n"),
            None,
        ),
        (
            "cost_case_noelse",
            format!("def f(cond, spec)\n  h = case cond\n  when 1 then spec\n  end\n{G}end\n"),
            None,
        ),
        (
            "cost_begin_ensure",
            format!("def f(spec)\n  h = begin\n    spec\n  ensure\n    nil\n  end\n{G}end\n"),
            None,
        ),
    ];
    for (row, src, expected) in &rows {
        let (ast, snaps) = class_snaps(src.as_bytes());
        let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
        assert_eq!(got, *expected, "carrier-fidelity row {row}");
    }
}

/// The coarse-carrier decline is SCOPED: a name made coarse in one `def`
/// must not disable narrowing for the same name in another `def` (a
/// whole-file set would silence common names like `h`/`value` project-wide).
#[test]
fn class_narrowing_coarse_set_is_per_scope() {
    let src = b"def a(spec)\n  h = spec || {}\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n\ndef b(h)\n  h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n";
    let (ast, snaps) = class_snaps(src);
    let calls: Vec<NodeId> = ast
        .iter()
        .filter_map(|(id, n)| match n {
            Node::Call { method, .. } if method == "frobnicate_zzz" => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(snaps.get(&calls[0]), None, "the Logical-bound `h` must decline");
    assert_eq!(
        snaps.get(&calls[1]).map(String::as_str),
        Some("Hash"),
        "the parameter `h` in another def must still narrow"
    );
}

/// STAGE 3b-1 (docs/notes/20260807-narrowing-stage3-spec.md § "3b-1"): the
/// unmodeled-statement-form decision table. Every row was measured against
/// the pinned reference from a fresh cwd with `--no-cache` — `Some(c)` means
/// the reference FIRES and rigor-rs must record `c`, `None` means rigor-rs
/// must record NOTHING (either the reference is silent, or the row is a
/// deliberate coverage decline).
///
/// The guard is always `return unless v.is_a?(String)` and the witnessed
/// call is always `v.frobnicate_zzz`, so a row's whole point is the
/// STATEMENT FORM the use sits in. 3b-1 mints no facts: every `Some` row is
/// a use recorded under a fact stage 1-2 already established.
#[test]
fn class_narrowing_stage3b1_statement_form_matrix() {
    const G: &str = "  return unless v.is_a?(String)\n";
    let guarded = |body: &str| format!("def f(v, cache, obj, cond, list)\n{G}{body}end\n");
    // (row, source, expected narrowed class of the `frobnicate_zzz` call)
    let mut rows: Vec<(&str, String, Option<&str>)> = vec![
        // --- d4-d7: ivar / gvar / cvar / constant write VALUES -----------
        // DECLINED (see the arm's comment): the reference FIRES on all
        // four, but descending this one arm surfaced a pre-existing
        // carrier-fidelity FP over the standing sweep. The e-family rows
        // below pin the half that DID ship — facts survive these writes.
        ("d4", guarded("  @x = v.frobnicate_zzz\n"), None),
        ("d5", guarded("  $gx = v.frobnicate_zzz\n"), None),
        ("d6", guarded("  @@cx = v.frobnicate_zzz\n"), None),
        (
            "d7",
            // A constant write is only legal at top level / in a class body.
            "v = $stdin\nif v.is_a?(String)\n  XCONST_ZZZ = v.frobnicate_zzz\nend\n".to_string(),
            None,
        ),
        // The two SWEEP FPs that forced the d4-d7 decline, reduced. Both
        // are `narrow_class_other` carrier gaps: rigor-rs typed a `Logical`
        // (fp1) and a project-method return ending in one (fp2) as
        // `Dynamic[top]` where the reference produces a union, so the
        // reference's Dynamic-only gate declined and ours did not.
        //
        // The gap is CLOSED as of the carrier-fidelity fix
        // (docs/notes/20260808-narrowing-carrier-fidelity-fp.md): both
        // shapes now decline on the CARRIER, at the guard, in every
        // statement form — see `class_narrowing_carrier_fidelity_matrix`
        // and the rules-layer `coarse_carrier_narrowing_is_silent_end_to_end`.
        // These rows stay pinned here as the d4-d7 regression tripwire;
        // re-enabling that arm is its own slice and its own gate run.
        (
            "fp1",
            "def f(spec)\n  h = spec || {}\n  @spec = h.is_a?(Hash) ? h.frobnicate_zzz : h\nend\n"
                .to_string(),
            None,
        ),
        (
            "fp2",
            "class C\n  def config\n    c = mk\n    raise ArgumentError unless c.is_a?(Hash)\n\n    @config = c.frobnicate_zzz\n  end\n\n  def mk\n    unknown_zzz || {}\n  end\nend\n"
                .to_string(),
            None,
        ),
        // --- d1/d10/d11: recovered op-assign carriers (bare local reads) --
        ("d1", guarded("  cache[v] ||= v.frobnicate_zzz\n"), Some("String")),
        ("d10a", guarded("  cache[v] += v.frobnicate_zzz\n"), Some("String")),
        ("d10b", guarded("  cache[v] &&= v.frobnicate_zzz\n"), Some("String")),
        ("d11", guarded("  obj.attr ||= v.frobnicate_zzz\n"), Some("String")),
        // d2 — the mastodon archetype: an op-assign whose RHS is a nested
        // conditional. The recovered carrier flattens the `if`, but the use
        // still records under the OUTER fact.
        (
            "d2",
            guarded("  cache[v] ||= if cond\n    v\n  else\n    v.frobnicate_zzz\n  end\n"),
            Some("String"),
        ),
        // --- d25/g6: a recovered carrier / `rescue` modifier as an RHS ----
        ("d25", guarded("  x = *v.frobnicate_zzz\n  x\n"), Some("String")),
        ("g6", guarded("  x = (v.frobnicate_zzz rescue nil)\n  x\n"), Some("String")),
        // --- d14-d17: literal containers ---------------------------------
        ("d14", guarded("  a, b = v.frobnicate_zzz, 1\n  [a, b]\n"), Some("String")),
        ("d15", guarded("  x = [v.frobnicate_zzz]\n  x\n"), Some("String")),
        ("d16", guarded("  x = { k: v.frobnicate_zzz }\n  x\n"), Some("String")),
        ("d17", guarded("  x = \"#{v.frobnicate_zzz}\"\n  x\n"), Some("String")),
        ("d17b", guarded("  x = :\"#{v.frobnicate_zzz}\"\n  x\n"), Some("String")),
        // --- d19/d20/f7/f8: begin/rescue/else/ensure ---------------------
        (
            "d19",
            guarded("  begin\n    v.frobnicate_zzz\n  rescue StandardError\n    nil\n  end\n"),
            Some("String"),
        ),
        (
            "d20",
            guarded(
                "  x = begin\n    v.frobnicate_zzz\n  rescue StandardError\n    nil\n  end\n  x\n",
            ),
            Some("String"),
        ),
        (
            "f7",
            guarded("  begin\n    nil\n  rescue StandardError\n    v.frobnicate_zzz\n  end\n"),
            Some("String"),
        ),
        ("f8", guarded("  begin\n    nil\n  ensure\n    v.frobnicate_zzz\n  end\n"), Some("String")),
        // --- g1: loop PREDICATE (`while`/`until`/`for` collection) -------
        ("g1", guarded("  while v.frobnicate_zzz\n    break\n  end\n"), Some("String")),
        ("g1d", guarded("  until v.frobnicate_zzz\n    break\n  end\n"), Some("String")),
        // g1b/g1c: the `for` COLLECTION is evaluated ONCE, before the index
        // rebind — the reference fires even when the index is the narrowed
        // local itself.
        ("g1b", guarded("  for i in v.frobnicate_zzz\n    nil\n  end\n"), Some("String")),
        ("g1c", guarded("  for v in v.frobnicate_zzz\n    nil\n  end\n"), Some("String")),
        // --- f1-f4b: `&&`/`||` no longer clear ---------------------------
        ("f1", guarded("  cond && v.frobnicate_zzz\n"), Some("String")),
        ("f2", guarded("  cond || v.frobnicate_zzz\n"), Some("String")),
        ("f3", guarded("  g(cond && v.frobnicate_zzz)\n"), Some("String")),
        ("f4", guarded("  x = cond && v.frobnicate_zzz\n  x\n"), Some("String")),
        ("f4b", guarded("  cond && 1\n  v.frobnicate_zzz\n"), Some("String")),
        // --- e-family: fact SURVIVAL past the new statement arms ---------
        ("e1", guarded("  @x = 1\n  v.frobnicate_zzz\n"), Some("String")),
        ("e6", guarded("  $gx = 1\n  v.frobnicate_zzz\n"), Some("String")),
        ("e7", guarded("  @@cx = 1\n  v.frobnicate_zzz\n"), Some("String")),
        ("e3", guarded("  cache[:k] ||= 1\n  v.frobnicate_zzz\n"), Some("String")),
        ("e9", guarded("  cache\n  v.frobnicate_zzz\n"), Some("String")),
        // --- ALREADY CLOSED ON MASTER: assert they STAY closed -----------
        // A regression in any of these is otherwise silent (the spec's
        // "Where the evidence note's reading was wrong" correction).
        ("d8", guarded("  cache[v] = v.frobnicate_zzz\n"), Some("String")),
        ("d9", guarded("  obj.attr = v.frobnicate_zzz\n"), Some("String")),
        ("d12a", guarded("  @x ||= v.frobnicate_zzz\n"), Some("String")),
        ("d12b", guarded("  @x += v.frobnicate_zzz\n"), Some("String")),
        ("d13", guarded("  $gx ||= v.frobnicate_zzz\n"), Some("String")),
        ("d23", guarded("  yield v.frobnicate_zzz\n"), Some("String")),
        // d24 (`defined?(v.frobnicate_zzz)`) is RETIRED at the `v0.3.4` pin.
        // Upstream #318 stopped every engine walk descending into a
        // `defined?` operand — the call is not reachable code on either side
        // any more, so rigor-rs no longer lowers one and there is no node
        // left to carry a narrowing fact. The behaviour it used to pin now
        // lives in `rigor-parse`'s `defined_operand_drops_calls_but_keeps_
        // local_reads` and harness fixture 97.
        ("g2", guarded("  super(v.frobnicate_zzz)\n"), Some("String")),
        ("g5", guarded("  v.frobnicate_zzz rescue nil\n"), Some("String")),
        // --- DECLINES (each load-bearing) --------------------------------
        // f10a: the `for` index rebinds `v` and the reference is SILENT
        // here. This row is why `Loop` bodies are not descended at all
        // (`Node::Loop::index` now names the rebind, rigor-rs#151, but the
        // body descent is a separate slice).
        ("f10a", guarded("  for v in list\n    v.frobnicate_zzz\n  end\n"), None),
        // f10b/d21/g3: `for` with a distinct index, a `while` body and a
        // `break` operand — the reference FIRES on all three; declined as
        // collateral of the f10a decline. Stage 3b-2.
        ("f10b", guarded("  for i in list\n    v.frobnicate_zzz\n  end\n"), None),
        ("d21", guarded("  while cond\n    v.frobnicate_zzz\n  end\n"), None),
        ("g3", guarded("  while cond\n    break v.frobnicate_zzz\n  end\n"), None),
        // post1/post2: the reference KEEPS the fact past a `begin`/loop;
        // we clear (unprobed at spec time ⇒ decline, a strict subset).
        (
            "post1",
            guarded("  begin\n    nil\n  rescue StandardError\n    nil\n  end\n  v.frobnicate_zzz\n"),
            None,
        ),
        ("post2", guarded("  while cond\n    nil\n  end\n  v.frobnicate_zzz\n"), None),
        // rescuebind: `rescue => v` REBINDS the narrowed local with no
        // `LocalVariableWrite` node. The reference narrows it to the
        // EXCEPTION class (`for StandardError`), so keeping the `String`
        // fact would be a live FP.
        (
            "rescuebind",
            guarded("  begin\n    nil\n  rescue StandardError => v\n    v.frobnicate_zzz\n  end\n"),
            None,
        ),
        // c5: a `while` PREDICATE mints nothing for the body (reference
        // silent — evidence note).
        ("c5", "def f(v)\n  while v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n".to_string(), None),
        // c2a/c2c: `Logical` MINTING stays out of slice (stage 3a-2) in
        // statement AND argument position.
        ("c2a", "def f(v)\n  v.is_a?(String) && v.frobnicate_zzz\nend\n".to_string(), None),
        ("c2c", "def f(v)\n  g(v.is_a?(String) && v.frobnicate_zzz)\nend\n".to_string(), None),
        // blk4/blk6: a BLOCK inside a literal container or a loop predicate
        // is not descended (container elements and a loop predicate are
        // EXPRESSION position). The reference does narrow there — recorded
        // coverage gaps, not FPs.
        (
            "blk4",
            "def f(v)\n  x = [[1].map { v.is_a?(String) ? v.frobnicate_zzz : v }]\n  x\nend\n"
                .to_string(),
            None,
        ),
        (
            "blk6",
            "def f(v)\n  while [1].map { v.is_a?(String) ? v.frobnicate_zzz : v }\n    break\n  end\nend\n"
                .to_string(),
            None,
        ),
        // --- INVALIDATION through the new arms ---------------------------
        // A rebind nested in an ivar-write value threads IMMEDIATELY.
        ("inv1", guarded("  @x = (v = cond)\n  v.frobnicate_zzz\n"), None),
        // A conditionally-executed rebind in a statement `&&` kills by span.
        ("inv2", guarded("  cond && (v = cache)\n  v.frobnicate_zzz\n"), None),
        // A rebind in an earlier array element kills the later sibling.
        ("inv3", guarded("  x = [v = cache, v.frobnicate_zzz]\n  x\n"), None),
        // A mutator inside a `begin` body kills the following use.
        (
            "inv4",
            guarded("  begin\n    v.merge!(a: 1)\n    v.frobnicate_zzz\n  rescue StandardError\n    nil\n  end\n"),
            None,
        ),
    ];
    rows.sort_by_key(|(row, _, _)| *row);
    for (row, src, expected) in &rows {
        let (ast, snaps) = class_snaps(src.as_bytes());
        let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
        assert_eq!(got, *expected, "stage 3b-1 matrix row {row}\n--- source ---\n{src}");
    }
}

/// STAGE 3a-1 — compound predicate analysis (`&&` / `||` / `!`) and the
/// both-direction termination propagation
/// (docs/notes/20260807-narrowing-stage3-spec.md).
///
/// Every row is oracle-measured against the pinned reference `v0.3.1` from
/// a fresh cwd with `--no-cache` and the checkout plugin path pinned.
/// `Some(c)` — the reference FIRES and rigor-rs must record `c`. `None` —
/// rigor-rs must record nothing, either because the reference is SILENT
/// (a would-be false positive — the control rows) or because the decline
/// costs coverage the reference has (a strict subset). The row name is the
/// probe name; see the note's "3a-1 BUILT" outcome table.
#[test]
fn class_narrowing_stage3a1_compound_predicate_matrix() {
    const USE: &str = "v.frobnicate_zzz";
    const G: &str = "v.is_a?(String)";
    let f = |body: &str| format!("def f(v, w, a, b)\n{body}\nend\n");
    let mut rows: Vec<(&str, String, Option<&str>)> = vec![
        // ---- c1: one recognised conjunct narrows the TRUTHY edge --------
        ("c1a", f(&format!("  {USE} if {G} && v.length > 2")), Some("String")),
        ("c1b", f(&format!("  if v.frozen? && {G}\n    {USE}\n  end")), Some("String")),
        (
            "c1c",
            f(&format!("  if v.frozen? && {G} && v.length > 2\n    {USE}\n  end")),
            Some("String"),
        ),
        ("x_and_kw", f(&format!("  if {G} and v.length > 2\n    {USE}\n  end")), Some("String")),
        // c1g control: the falsey edge of a plain `&&` stays UNNARROWED.
        (
            "c1g",
            f(&format!("  if {G} && v.length > 2\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        // f12 control: an unrecognised `||` disjunct kills the truthy join.
        ("f12", f(&format!("  if {G} || {G} || v.nil?\n    {USE}\n  end")), None),
        // ---- c4: `!` swaps the edges ------------------------------------
        ("c4d", f(&format!("  if !{G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
        ("c4f", f(&format!("  unless !{G}\n    {USE}\n  end")), Some("String")),
        ("x_not_kw", f(&format!("  if not {G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
        ("x_double_bang", f(&format!("  if !!{G}\n    {USE}\n  end")), Some("String")),
        (
            "x_bang_of_and",
            f(&format!("  if !({G} && v.length > 2)\n    1\n  else\n    {USE}\n  end")),
            Some("String"),
        ),
        ("u_bang_nonguard", f(&format!("  if !v.nil?\n    {USE}\n  end")), None),
        // ---- termination propagation, BOTH directions -------------------
        ("c4a", f(&format!("  return if !{G}\n  {USE}")), Some("String")),
        ("c4b", f(&format!("  if !{G}\n    return\n  end\n  {USE}")), Some("String")),
        ("f22", f(&format!("  raise \"x\" if !{G}\n  {USE}")), Some("String")),
        ("f16", f(&format!("  if {G}\n    1\n  else\n    return\n  end\n  {USE}")), Some("String")),
        ("t_c1d", f(&format!("  return unless {G} && v.length > 2\n  {USE}")), Some("String")),
        ("t_c1d_or", f(&format!("  return if !{G} || v.nil?\n  {USE}")), Some("String")),
        (
            "t_unless_and",
            f(&format!("  unless {G} && v.length > 2\n    return\n  end\n  {USE}")),
            Some("String"),
        ),
        // CONTROL, measured reference-silent: both branches terminate, so
        // the statements after are unreachable. Propagating either map here
        // would be a live FP.
        ("t_both_terminate", f(&format!("  if !{G}\n    return\n  else\n    return\n  end\n  {USE}")), None),
        // A write inside the conditional's span declines the propagation
        // (the reference still narrows — coverage cost, not an FP).
        ("t_write_in_span", f(&format!("  if !{G}\n    v = 1\n    return\n  end\n  {USE}")), None),
        // A rebind AFTER the propagated guard kills the fact (the reference
        // fires a DIFFERENT diagnostic there, `for 1`).
        ("t_write_after_guard", f(&format!("  return if !{G}\n  v = 1\n  {USE}")), None),
        ("t_elsif", f(&format!("  if v.nil?\n    1\n  elsif !{G}\n    2\n  else\n    {USE}\n  end")), Some("String")),
        // ---- (a) `&&` falsey JOIN with two guards -----------------------
        (
            "a_same_local_disjoint_else",
            f(&format!("  if {G} && v.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        (
            "a_two_locals_else",
            f(&format!("  if {G} && w.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        // The SPEC CORRECTION: "b wins a same-local collision" would fire
        // `for Hash` here; the reference reaches `Bot` and is SILENT.
        (
            "a_same_local_disjoint_then",
            f(&format!("  if {G} && v.is_a?(Hash)\n    {USE}\n  end")),
            None,
        ),
        // ---- (b) `&&` falsey join, SAME class both sides ----------------
        (
            "b2_and_bang_same",
            f(&format!("  if !{G} && !{G}\n    1\n  else\n    {USE}\n  end")),
            Some("String"),
        ),
        // Different classes join to a UNION in the reference (`Hash |
        // String`) — declined until 3a-4, coverage cost only.
        (
            "b2_and_bang_diff",
            f(&format!("  if !{G} && !v.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        (
            "b2_and_bang_two_locals",
            f(&format!("  if !{G} && !w.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        ("b_same_class_then", f(&format!("  if {G} && {G}\n    {USE}\n  end")), Some("String")),
        ("b_same_class_else", f(&format!("  if {G} && {G}\n    1\n  else\n    {USE}\n  end")), None),
        // ---- `||` edge algebra ------------------------------------------
        ("x_or_same_class", f(&format!("  if {G} || {G}\n    {USE}\n  end")), Some("String")),
        // A `||` of DIFFERENT classes is a union in the reference — 3a-4.
        ("x_or_diff_class", f(&format!("  if {G} || v.is_a?(Hash)\n    {USE}\n  end")), None),
        (
            "x_or_falsey_bang",
            f(&format!("  if !{G} || v.nil?\n    1\n  else\n    {USE}\n  end")),
            Some("String"),
        ),
        (
            "o_or_none_then_bang",
            f(&format!("  if v.nil? || !{G}\n    1\n  else\n    {USE}\n  end")),
            Some("String"),
        ),
        (
            "o_or_bang_bang_diff",
            f(&format!("  if !{G} || !v.is_a?(Hash)\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        (
            "o_and_bang_then_cond",
            f(&format!("  if !{G} && v.nil?\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        (
            "u_and_nested_or",
            f(&format!("  if ({G} || v.is_a?(Hash)) && v.length > 2\n    {USE}\n  end")),
            None,
        ),
        ("u_or_nested_and", f(&format!("  if ({G} && v.length > 2) || v.nil?\n    {USE}\n  end")), None),
        (
            "u_and_or_falsey",
            f(&format!("  if !{G} && (v.nil? || v.frozen?)\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        ("n_unless_bang", f(&format!("  unless !{G}\n    {USE}\n  end")), Some("String")),
        (
            "n_unless_and_else",
            f(&format!("  unless {G} && v.length > 2\n    1\n  else\n    {USE}\n  end")),
            Some("String"),
        ),
        (
            "n_unless_or_bang",
            f(&format!("  unless !{G} || v.nil?\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        // Two locals, both narrowed by the same compound predicate.
        (
            "a_two_locals_then",
            f(&format!("  if {G} && w.is_a?(Hash)\n    {USE}\n    w.other_zzz\n  end")),
            Some("String"),
        ),
        // ---- position: `if`/ternary narrows in EVERY position -----------
        ("q_ternary_bang", f(&format!("  g(!{G} ? 1 : {USE})")), Some("String")),
        ("q_ternary_and", f(&format!("  g({G} && v.length > 2 ? {USE} : 1)")), Some("String")),
        // ---- DECLINES, each measured reference-FIRING (coverage cost) ---
        // `===` narrows in the reference; this slice keeps it Bot-only.
        ("e3_case_eq_bang", f(&format!("  if !(String === v)\n    1\n  else\n    {USE}\n  end")), None),
        // A same-local `&&` collision in a SUBCLASS relation: the reference
        // keeps the more specific class; review R3 drops the fact.
        ("s_num_then_int", f("  if v.is_a?(Numeric) && v.is_a?(Integer)\n    v.frobnicate_zzz\n  end"), None),
        ("s_int_then_num", f("  if v.is_a?(Integer) && v.is_a?(Numeric)\n    v.frobnicate_zzz\n  end"), None),
        // (d) the carrier ALLOW-list gate stays in force PER LOCAL: a local
        // bound from a `Logical` declines even on the new falsey edge.
        (
            "d_logical_carrier",
            f("  v2 = a || b\n  if !v2.is_a?(String)\n    1\n  else\n    v2.frobnicate_zzz\n  end"),
            None,
        ),
        // …and its control: the same shape on a PARAMETER narrows.
        ("d_param_control", f(&format!("  if !{G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
        // A conjunct narrowing only the OTHER local leaves this one alone.
        (
            "x_and_two_locals_falsey_only_v",
            f(&format!("  if {G} && w.is_a?(String)\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        // ---- the CONJUNCT-INTERFERENCE battery -------------------------
        // 54 measured `X && guard` / `guard && X` rows (probes4/probes5)
        // found exactly three FP mechanisms; everything else is inert. The
        // inert representatives are pinned as POSITIVES so a future
        // over-broad interference rule cannot quietly delete them.
        ("L_len_cmp", f(&format!("  if v.length > 2 && {G}\n    {USE}\n  end")), Some("String")),
        ("L_frozen", f(&format!("  if v.frozen? && {G}\n    {USE}\n  end")), Some("String")),
        ("R_frozen", f(&format!("  if {G} && v.frozen?\n    {USE}\n  end")), Some("String")),
        ("L_bare_v", f(&format!("  if v && {G}\n    {USE}\n  end")), Some("String")),
        ("R_bare_v", f(&format!("  if {G} && v\n    {USE}\n  end")), Some("String")),
        ("L_bare_w", f(&format!("  if w && {G}\n    {USE}\n  end")), Some("String")),
        ("L_respond", f(&format!("  if v.respond_to?(:foo) && {G}\n    {USE}\n  end")), Some("String")),
        ("R_respond", f(&format!("  if {G} && v.respond_to?(:foo)\n    {USE}\n  end")), Some("String")),
        ("L_empty", f(&format!("  if v.empty? && {G}\n    {USE}\n  end")), Some("String")),
        ("L_cmp_ge", f(&format!("  if v >= 2 && {G}\n    {USE}\n  end")), Some("String")),
        ("R_cmp_ge", f(&format!("  if {G} && v >= 2\n    {USE}\n  end")), Some("String")),
        ("L_between", f(&format!("  if v.between?(1, 3) && {G}\n    {USE}\n  end")), Some("String")),
        ("L_startwith", f(&format!("  if v.start_with?(\"a\") && {G}\n    {USE}\n  end")), Some("String")),
        ("L_call_arg", f(&format!("  if w.include?(v) && {G}\n    {USE}\n  end")), Some("String")),
        ("L_eq_one", f(&format!("  if v == 1 && {G}\n    {USE}\n  end")), Some("String")),
        ("R_eq_one", f(&format!("  if {G} && v == 1\n    {USE}\n  end")), Some("String")),
        ("L_bang_nilq", f(&format!("  if !v.nil? && {G}\n    {USE}\n  end")), Some("String")),
        ("R_bang_nilq", f(&format!("  if {G} && !v.nil?\n    {USE}\n  end")), Some("String")),
        ("L_neq_nil", f(&format!("  if v != nil && {G}\n    {USE}\n  end")), Some("String")),
        ("R_neq_nil", f(&format!("  if {G} && v != nil\n    {USE}\n  end")), Some("String")),
        ("other_nilq", f(&format!("  if w.nil? && {G}\n    {USE}\n  end")), Some("String")),
        ("matchop_keep", f(&format!("  if v =~ /a/ && {G}\n    {USE}\n  end")), Some("String")),
        ("R_caseeq_same", f(&format!("  if String === v && {G}\n    {USE}\n  end")), Some("String")),
        // …and the three FP mechanisms, each measured reference-SILENT.
        ("L_nilq", f(&format!("  if v.nil? && {G}\n    {USE}\n  end")), None),
        ("R_nilq", f(&format!("  if {G} && v.nil?\n    {USE}\n  end")), None),
        ("mid_nilq", f(&format!("  if v.frozen? && v.nil? && {G}\n    {USE}\n  end")), None),
        ("R_eq_nil", f(&format!("  if {G} && v == nil\n    {USE}\n  end")), None),
        // `== nil` on the LEFT is reference-FIRING; declining it is the
        // coverage price of the one rule that closes `R_eq_nil`.
        ("L_eq_nil", f(&format!("  if v == nil && {G}\n    {USE}\n  end")), None),
        ("L_caseeq", f(&format!("  if String === v && v.is_a?(Hash)\n    {USE}\n  end")), None),
        ("R_caseeq", f(&format!("  if v.is_a?(Hash) && String === v\n    {USE}\n  end")), None),
        // A named-capture `=~` binds `v` invisibly: decline the predicate.
        (
            "matchwrite",
            "def f(s)\n  if /(?<v>a)/ =~ s && v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n".to_string(),
            None,
        ),
        // …including when the reference AGREES with the narrowing — the
        // decline is uniform because the binding is arena-invisible.
        (
            "matchwrite_str",
            "def f(s)\n  if /(?<v>a)/ =~ s && v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n".to_string(),
            None,
        ),
        // A `||` whose disjuncts pin different classes joins to a union.
        ("or_nilq", f(&format!("  if v.nil? || {G}\n    {USE}\n  end")), None),
        (
            "bang_nilq_falsey",
            f(&format!("  if !v.nil? && !{G}\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
        (
            "nilq_falsey",
            f(&format!("  if v.nil? && !{G}\n    1\n  else\n    {USE}\n  end")),
            None,
        ),
    ];
    rows.sort_by_key(|(row, _, _)| *row);
    for (row, src, expected) in &rows {
        let (ast, snaps) = class_snaps(src.as_bytes());
        let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
        assert_eq!(got, *expected, "stage 3a-1 matrix row {row}\n--- source ---\n{src}");
    }
}

/// STAGE 3a-1 follow-up — `branch_terminates` recognising `next` / `break`
/// (docs/notes/20260807-narrowing-stage3-spec.md, the 2026-08-08 section).
///
/// Same convention as the 3a-1 matrix: `Some(c)` — the reference FIRES and
/// rigor-rs must record `c`; `None` — rigor-rs must record nothing, either
/// because the reference is SILENT (a would-be false positive) or because
/// the decline costs coverage the reference has (a strict subset). Every row
/// is oracle-measured against the pinned reference `v0.3.1` from a fresh cwd
/// with `--no-cache` and the checkout plugin path pinned; the row name is
/// the probe name in the note's table.
#[test]
fn class_narrowing_next_break_termination_matrix() {
    const USE: &str = "v.frobnicate_zzz";
    const G: &str = "v.is_a?(String)";
    // A method wrapper whose body runs INSIDE an `each` block, which is
    // where `next`/`break` are legal Ruby.
    let blk = |body: &str| format!("def f(v, w, xs)\n  xs.each do |x|\n{body}\n  end\nend\n");
    let mut rows: Vec<(&str, String, Option<&str>)> = vec![
        // ---- the archetype, both jumps, both carriers -------------------
        ("p1_next_block", blk(&format!("    next unless {G}\n    {USE}")), Some("String")),
        ("p2_break_block", blk(&format!("    break unless {G}\n    {USE}")), Some("String")),
        (
            "p3_next_blockparam",
            "def f(xs)\n  xs.each do |x|\n    next unless x.is_a?(String)\n    x.frobnicate_zzz\n  end\nend\n".to_string(),
            Some("String"),
        ),
        (
            "p3b_break_blockparam",
            "def f(xs)\n  xs.each do |x|\n    break unless x.is_a?(String)\n    x.frobnicate_zzz\n  end\nend\n".to_string(),
            Some("String"),
        ),
        // `!` swap: `next if !guard` carries the falsey map.
        ("p11_bang_next_if", blk(&format!("    next if !{G}\n    {USE}")), Some("String")),
        // A VALUED `next`/`break` exits the branch exactly like the
        // argument-less form — the reference's
        // `branch_unconditionally_exits?` does not look at the jump's
        // arguments, and the `StatementsKind::Jump` carrier (issue #140)
        // now preserves that tag for `stmt_terminates`.
        (
            "p16_next_with_value",
            format!("def f(v, xs)\n  xs.map do |x|\n    next 0 unless {G}\n    {USE}\n  end\nend\n"),
            Some("String"),
        ),
        (
            "p16b_break_with_value",
            format!("def f(v, xs)\n  xs.map do |x|\n    break 0 unless {G}\n    {USE}\n  end\nend\n"),
            Some("String"),
        ),
        // The compound census shape (`next unless job && x.is_a?(Hash)`).
        ("p17_next_compound", blk(&format!("    next unless w && {G}\n    {USE}")), Some("String")),
        ("q13_next_or_guard", blk(&format!("    next if !{G} || v.empty?\n    {USE}")), Some("String")),
        ("r9_kind_of", blk("    next unless v.kind_of?(String)\n    v.frobnicate_zzz"), Some("String")),
        ("r10_instance_of", blk("    next unless v.instance_of?(String)\n    v.frobnicate_zzz"), Some("String")),
        // The jump need only be the branch's LAST statement …
        (
            "q6_next_not_last",
            blk(&format!("    unless {G}\n      w.warn('x')\n      next\n    end\n    {USE}")),
            Some("String"),
        ),
        // … and a jump followed by dead code does NOT terminate the branch
        // (`.last` is not a `next`) — the reference is silent too.
        (
            "q7_next_first_not_last",
            blk(&format!("    unless {G}\n      next\n      w.warn('x')\n    end\n    {USE}")),
            None,
        ),
        // The block need not be a loop at all — the recognition is syntactic
        // on the reference and here.
        (
            "q16_define_method",
            "class K\n  define_method(:f) do |v|\n    next unless v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n".to_string(),
            Some("String"),
        ),
        (
            "q15_lambda_rhs",
            "def f(v)\n  g = lambda do |x|\n    next unless v.is_a?(String)\n    v.frobnicate_zzz\n  end\n  g\nend\n".to_string(),
            Some("String"),
        ),
        // ---- CONTROLS: reference-SILENT, so recording would be an FP ----
        // The use BEFORE the guard.
        ("p4_use_before", blk(&format!("    {USE}\n    next unless {G}")), None),
        // `next if guard` — the truthy edge terminates and the falsey map of
        // an atomic class guard is EMPTY.
        ("p10_next_if_positive", blk(&format!("    next if {G}\n    {USE}")), None),
        // A rebind inside the conditional's span, and one after the guard.
        ("q3_write_in_span", blk(&format!("    next unless (v = w).is_a?(String)\n    {USE}")), None),
        ("q17_rebind_then_use", blk(&format!("    next unless {G}\n    v = w\n    {USE}")), None),
        // A fact minted inside a block NEVER escapes it (`join_cenv` keeps
        // only `Bot`) — for `next`, for `break`, and out of a NESTED block.
        (
            "p9_after_block",
            format!("def f(v, xs)\n  xs.each do |x|\n    next unless {G}\n  end\n  {USE}\nend\n"),
            None,
        ),
        (
            "p9b_after_block_break",
            format!("def f(v, xs)\n  xs.each do |x|\n    break unless {G}\n  end\n  {USE}\nend\n"),
            None,
        ),
        (
            "p13_nested_block",
            format!("def f(v, xs, ys)\n  xs.each do |x|\n    ys.each do |y|\n      next unless {G}\n    end\n    {USE}\n  end\nend\n"),
            None,
        ),
        // …nor past an inner `if` inside the block (the join clears it).
        ("q10_after_nested_if", blk(&format!("    if w\n      next unless {G}\n    end\n    {USE}")), None),
        // A block in ARGUMENT position is not descended at all.
        (
            "r7_block_arg_position",
            format!("def f(v, xs, sink)\n  sink.push(xs.each do |x|\n    next unless {G}\n    {USE}\n  end)\nend\n"),
            None,
        ),
        // ---- DECLINES: the reference fires, we do not (strict subset) ----
        // A `while`/`until` BODY is never descended (stage 3b-2).
        (
            "p5_next_in_while",
            format!("def f(v, n)\n  while n > 0\n    next unless {G}\n    {USE}\n  end\nend\n"),
            None,
        ),
        // `throw` / `exit` / `abort` / `fail` / `redo` all terminate on the
        // reference; only `raise` is ported (out of this slice).
        ("p15_throw", blk(&format!("    throw :done unless {G}\n    {USE}")), None),
        ("p8_redo", blk(&format!("    redo unless {G}\n    {USE}")), None),
        // BOTH branches jumping: the reference propagates the TRUTHY map
        // (`eval_if:495` needs only a present then-branch), we decline —
        // `truthy_terminates != falsey_terminates` is the subset rule.
        (
            "q19_break_both_terminate",
            blk(&format!("    if {G}\n      break\n    else\n      break\n    end\n    {USE}")),
            None,
        ),
        // A `case`/`when` clause ending in `next`: `class_flow_case` has no
        // termination propagation (stage 3a-4).
        (
            "q11_next_case_when",
            blk(&format!("    case v\n    when Integer then nil\n    else next\n    end\n    {USE}")),
            None,
        ),
        // The carrier ALLOW-list still declines per local (PR #72).
        (
            "q4_coarse_carrier",
            format!("def f(xs, a, b)\n  v = a || b\n  xs.each do |x|\n    next unless {G}\n    {USE}\n  end\nend\n"),
            None,
        ),
    ];
    rows.sort_by_key(|(row, _, _)| *row);
    for (row, src, expected) in &rows {
        let (ast, snaps) = class_snaps(src.as_bytes());
        let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
        assert_eq!(got, *expected, "next/break termination row {row}\n--- source ---\n{src}");
    }
}

/// STAGE 3a-1 × PR #73: the disjoint-guard `Bot` collapse composes with the
/// new compound-predicate edges. `true` — the call site must be DEAD (the
/// reference is silent because its carrier collapsed to `Bot`); `false` —
/// it must stay live (the reference FIRES, so suppressing would lose a real
/// diagnostic). Every row is a live false positive on master except the
/// `must_fire_*` controls, which pin the anti-over-suppression half.
#[test]
fn class_narrowing_stage3a1_bot_composition_matrix() {
    let rows: &[(&str, &str, bool)] = &[
        // `!guard` + termination: the falsey map carries the guard past the
        // `return`, and the Array carrier collapses against Hash.
        ("c_bang_return", "def f\n  v = [1, 2]\n  return if !v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", true),
        // `===` under `!`, same shape.
        ("e3_case_eq_bot", "def f\n  v = [1, 2]\n  return if !(Hash === v)\n  v.frobnicate_zzz\nend\n", true),
        // A `&&` whose recognised conjunct collapses.
        ("k_bot_and_cond", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) && v.frozen?\n    v.frobnicate_zzz\n  end\nend\n", true),
        // An `||` truthy JOIN where BOTH disjuncts collapse — the union
        // `Hash | String` is `Bot | Bot`.
        ("k_bot_or_bot", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) || v.is_a?(String)\n    v.frobnicate_zzz\n  end\nend\n", true),
        ("k_bot_or_same", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) || v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", true),
        // A same-local `&&` collision on a PRECISE carrier: the second
        // conjunct collapses what the first left alone, in either order.
        ("p_and_collide_precise", "def f\n  v = [1, 2]\n  if v.is_a?(Array) && v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", true),
        ("p_and_collide_precise2", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) && v.is_a?(Array)\n    v.frobnicate_zzz\n  end\nend\n", true),
        ("p_bang_and_precise", "def f\n  v = [1, 2]\n  return if !(v.is_a?(Hash) && v.nil?)\n  v.frobnicate_zzz\nend\n", true),
        // ---- must-still-fire controls (reference FIRES) -----------------
        // An unrecognised disjunct empties the `||` truthy join.
        ("must_fire_or_cond", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) || v.frozen?\n    v.frobnicate_zzz\n  end\nend\n", false),
        // The truthy edge of `!guard` carries NOTHING.
        ("must_fire_bang_then", "def f\n  v = [1, 2]\n  if !v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", false),
        // The falsey edge of a plain `&&` carries nothing either.
        ("must_fire_else_of_and", "def f\n  v = [1, 2]\n  if v.is_a?(Hash) && v.frozen?\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", false),
        // An `&&` falsey join that drops leaves the else edge un-collapsed.
        ("must_fire_bang_and_else", "def f\n  v = [1, 2]\n  if !v.is_a?(Hash) && v.frozen?\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", false),
        // ---- the `nil?` / `== nil` collapse -----------------------------
        // `narrow_nil` (`narrowing.rb:90`) sends every precise carrier to
        // `Bot`, so a nil test on an Array-literal local silences its calls.
        ("nilq_bot_then", "def f\n  v = [1, 2]\n  if v.nil?\n    v.frobnicate_zzz\n  end\nend\n", true),
        ("nilq_bot_return", "def f\n  v = [1, 2]\n  return unless v.nil?\n  v.frobnicate_zzz\nend\n", true),
        ("eqnil_bot_then", "def f\n  v = [1, 2]\n  if v == nil\n    v.frobnicate_zzz\n  end\nend\n", true),
        ("nilq_bot_and", "def f\n  v = [1, 2]\n  if v.nil? && v.frozen?\n    v.frobnicate_zzz\n  end\nend\n", true),
        // …and its must-still-fire twin: the FALSEY edge of `nil?` is
        // `narrow_non_nil`, which leaves a precise carrier alone.
        ("must_fire_nilq_else", "def f\n  v = [1, 2]\n  if v.nil?\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", false),
    ];
    for (row, src, dead) in rows {
        let ast = lower_src(src.as_bytes());
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let scopes = lexical_scopes(&ast);
        let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let pass = typer.class_narrowing_pass(&ast, &mut i);
        let call = call_named(&ast, "frobnicate_zzz");
        assert_eq!(pass.dead.contains(&call), *dead, "3a-1 Bot row {row}");
    }
}

/// SEQUENTIAL disjoint/refining guards — the [`Typer::apply_guards`]
/// sequential-guard meet plus the pre-join propagation in
/// [`Typer::class_flow_if`]. Every row is oracle-measured (pin `v0.3.1`,
/// 2026-08-08 `seqprobe` matrix): `Some(class)` — the reference fires
/// `for <class>` and the snapshot must carry it; `(None, true)` — the
/// reference's meet reached `Bot` and the call site must be DEAD (every
/// `dead: true` row except the harness-shape controls was a live FP on
/// master); `(None, false)` — a recorded DECLINE: the reference fires but
/// we drop the fact (never an FP, only coverage).
#[test]
fn class_narrowing_sequential_guard_meet_matrix() {
    let rows: &[(&str, &str, Option<&str>, bool)] = &[
        // ---- the FP family: disjoint sequential pairs reach Bot ---------
        ("seq_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
        ("seq_raise", "def f(v)\n  raise ArgumentError unless v.is_a?(String)\n  raise ArgumentError unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
        ("seq_bang", "def f(v)\n  return unless v.is_a?(String)\n  return if !v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
        // A third guard cannot revive the collapsed local.
        ("seq_third", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash)\n  return unless v.is_a?(String)\n  v.frobnicate_zzz\nend\n", None, true),
        // `instance_of?` collapses on a bare name mismatch — even a
        // SUBCLASS name (the reference tests `context.exact` before the
        // hierarchy; both probes are reference-silent).
        ("seq_exact_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.instance_of?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
        ("seq_exact_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  return unless v.instance_of?(Integer)\n  v.frobnicate_zzz\nend\n", None, true),
        // Non-mintable second guards feed the same meet: `===`, `nil?`.
        ("seq_caseeq_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless Hash === v\n  v.frobnicate_zzz\nend\n", None, true),
        ("seq_nilq", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.nil?\n  v.frobnicate_zzz\nend\n", None, true),
        // An `||` union whose EVERY member is disjoint.
        ("seq_or_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash) || v.is_a?(Array)\n  v.frobnicate_zzz\nend\n", None, true),
        // The `next` spelling inside a block (the r8 shape).
        ("blk_next_disjoint", "def f(xs)\n  xs.each do |v|\n    next unless v.is_a?(String)\n    next unless v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", None, true),
        // A NON-terminating second conditional: the branch-edge meet.
        ("br_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  if v.is_a?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", None, true),
        ("br_exact_disjoint", "def f(v)\n  return unless v.is_a?(String)\n  if v.instance_of?(Hash)\n    v.frobnicate_zzz\n  end\nend\n", None, true),
        // A use between the guards fires; the use AFTER the collapse is
        // dead (the reference reports only the first).
        ("seq_use_between", "def f(v)\n  return unless v.is_a?(String)\n  v.frobnicate_yyy\n  return unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
        // ---- refinement / no-op: the reference FIRES and so must we -----
        // A subclass guard refines to the MORE SPECIFIC class …
        ("seq_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  return unless v.is_a?(Integer)\n  v.frobnicate_zzz\nend\n", Some("Integer"), false),
        // … a superclass guard is a no-op (the carrier stays) …
        ("seq_superclass", "def f(v)\n  return unless v.is_a?(Integer)\n  return unless v.is_a?(Numeric)\n  v.frobnicate_zzz\nend\n", Some("Integer"), false),
        // … a same-class re-guard keeps, `===` included …
        ("seq_same", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(String)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
        ("seq_caseeq_same", "def f(v)\n  return unless v.is_a?(String)\n  return unless String === v\n  v.frobnicate_zzz\nend\n", Some("String"), false),
        // … and `===` refines too (updating an existing fact is not
        // minting — the reference fires `for Integer`).
        ("seq_caseeq_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  return unless Integer === v\n  v.frobnicate_zzz\nend\n", Some("Integer"), false),
        ("blk_next_subclass", "def f(xs)\n  xs.each do |v|\n    next unless v.is_a?(Numeric)\n    next unless v.is_a?(Integer)\n    v.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
        // Refinement through a branch edge (non-terminating conditional).
        ("br_subclass", "def f(v)\n  return unless v.is_a?(Numeric)\n  if v.is_a?(Integer)\n    v.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
        ("br_superclass", "def f(v)\n  return unless v.is_a?(Integer)\n  if v.is_a?(Numeric)\n    v.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
        // The ELSE edge of a disjoint branch keeps the incoming fact.
        ("br_else_keeps", "def f(v)\n  return unless v.is_a?(String)\n  if v.is_a?(Hash)\n    1\n  else\n    v.frobnicate_zzz\n  end\nend\n", Some("String"), false),
        // ---- must-still-fire controls -----------------------------------
        ("ctrl_single", "def f(v)\n  return unless v.is_a?(String)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
        // A rebind between the guards resets the meet: the second guard
        // mints fresh (the reference fires `for Hash`).
        ("ctrl_write_between", "def f(v, w)\n  return unless v.is_a?(String)\n  v = w\n  return unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", Some("Hash"), false),
        // ---- the union family, and the `Unknown` arm the re-pin moved ---
        // An `||` union with a LIVE member meets per member: `Bot ∪
        // String` is the carrier and the reference fires `for String`.
        ("seq_or_mixed", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash) || v.is_a?(String)\n  v.frobnicate_zzz\nend\n", Some("String"), false),
        // An unresolvable ordering now WIDENS (upstream #533 item 4,
        // `70ca7e74`). These five rows asserted `Some("String")` until the
        // `v0.3.4 → v0.3.8` re-pin, on the retired `:unknown stays
        // conservative` rule; re-measured against `ffb456b0` (2026-09-09,
        // fresh cwd, `--no-cache`) all five are reference-SILENT, so each
        // was a live false positive. They are now `Widened` ⇒ DEAD.
        ("seq_projclass", "class ProjBare; end\n\ndef f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(ProjBare)\n  v.frobnicate_zzz\nend\n", None, true),
        ("seq_projsub", "class ProjKlass < Hash; end\n\ndef f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(ProjKlass)\n  v.frobnicate_zzz\nend\n", None, true),
        // One unorderable MEMBER widens the whole `||` union, even though
        // its `Hash` member alone would be `Bot`.
        ("seq_projsub_or", "class ProjKlass < Hash; end\n\ndef f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(Hash) || v.is_a?(ProjKlass)\n  v.frobnicate_zzz\nend\n", None, true),
        // The OTHER `Unknown`: two RBS-space names our resolver cannot
        // order. The reference proves them disjoint and is silent (the S2
        // probe r7); before the re-pin the fact DROPPED (silent but live),
        // now it widens (silent and suppressed) — same observable answer.
        ("seq_ns_unknown_drop", "def f(v)\n  return unless v.is_a?(File::Stat)\n  return unless v.is_a?(URI::HTTP)\n  v.frobnicate_zzz\nend\n", None, true),
        // The widening STICKS here: a later orderable guard neither
        // re-mints nor collapses. A recorded COVERAGE GAP, not parity — the
        // reference re-narrows its `untyped` carrier through
        // `narrow_class_other` and fires `for Hash` (measured at the pin;
        // rows b20a/b34b are the same shape). Suppressing is the FP-safe
        // side, and re-minting over a widened fact is unprobed guesswork.
        ("seq_unknown_then_known", "def f(v)\n  return unless v.is_a?(String)\n  return unless v.is_a?(UnknownZzzClass)\n  return unless v.is_a?(Hash)\n  v.frobnicate_zzz\nend\n", None, true),
    ];
    for (row, src, expected, dead) in rows {
        let ast = lower_src(src.as_bytes());
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let scopes = lexical_scopes(&ast);
        let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let pass = typer.class_narrowing_pass(&ast, &mut i);
        let call = call_named(&ast, "frobnicate_zzz");
        assert_eq!(
            pass.calls.get(&call).map(String::as_str),
            *expected,
            "sequential-guard row {row} snapshot\n--- source ---\n{src}"
        );
        assert_eq!(pass.dead.contains(&call), *dead, "sequential-guard row {row} dead");
        if *row == "seq_use_between" {
            let first = call_named(&ast, "frobnicate_yyy");
            assert_eq!(
                pass.calls.get(&first).map(String::as_str),
                Some("String"),
                "sequential-guard row {row}: the use BETWEEN the guards fires"
            );
            assert!(!pass.dead.contains(&first), "row {row}: first use stays live");
        }
    }
}

/// SEQUENTIAL guards on a stage-3a-3 CHAIN address — the chain twin of
/// [`class_narrowing_sequential_guard_meet_matrix`]. Every row was measured
/// against the pinned oracle (`v0.3.1`, 2026-08-09 `chain_*` probe matrix,
/// one FRESH cwd per scenario, `--no-cache`): `Some(class)` — the reference
/// fires `for <class>` and the snapshot must carry it; `(None, true)` — the
/// meet reached `Bot` and the call site must be DEAD; `(None, false)` — a
/// recorded DECLINE (the reference may fire; we drop the fact — coverage,
/// never an FP).
///
/// `chain_or_disjoint` and `chain_third` were LIVE false positives on
/// master: the union guard skipped the `classes.len() == 1` mint gate and
/// the disjoint re-guard could only REMOVE (never collapse), so a stale or
/// re-minted fact witnessed `for String` where the reference is silent.
#[test]
fn class_narrowing_chain_guard_meet_matrix() {
    let rows: &[(&str, &str, Option<&str>, bool)] = &[
        // ---- the FP family: disjoint sequential pairs reach Bot ---------
        ("chain_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
        // A THIRD guard cannot revive the collapsed address (live FP).
        ("chain_third", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", None, true),
        ("chain_bang", "def f(h)\n  return unless h.last.is_a?(String)\n  return if !h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
        // An `||` union whose EVERY member is disjoint (live FP: the mint
        // gate skipped the 2-class guard and the stale fact survived).
        ("chain_or_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash) || h.last.is_a?(Array)\n  h.last.frobnicate_zzz\nend\n", None, true),
        // `instance_of?` collapses on a bare name mismatch BEFORE the
        // hierarchy — a SUBCLASS name included.
        ("chain_exact_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.instance_of?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
        ("chain_exact_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  return unless h.last.instance_of?(Integer)\n  h.last.frobnicate_zzz\nend\n", None, true),
        // A NON-terminating second conditional: the branch-edge meet.
        ("chain_br_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  if h.last.is_a?(Hash)\n    h.last.frobnicate_zzz\n  end\nend\n", None, true),
        // A use between the guards fires; the use after the collapse is dead.
        ("chain_ctrl_use_between", "def f(h)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_yyy\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", None, true),
        // ---- refinement / no-op: the reference FIRES and so must we -----
        ("chain_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  return unless h.last.is_a?(Integer)\n  h.last.frobnicate_zzz\nend\n", Some("Integer"), false),
        ("chain_superclass", "def f(h)\n  return unless h.last.is_a?(Integer)\n  return unless h.last.is_a?(Numeric)\n  h.last.frobnicate_zzz\nend\n", Some("Integer"), false),
        ("chain_same", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
        ("chain_br_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  if h.last.is_a?(Integer)\n    h.last.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
        ("chain_br_superclass", "def f(h)\n  return unless h.last.is_a?(Integer)\n  if h.last.is_a?(Numeric)\n    h.last.frobnicate_zzz\n  end\nend\n", Some("Integer"), false),
        // The ELSE edge of a disjoint branch keeps the incoming fact.
        ("chain_br_else_keeps", "def f(h)\n  return unless h.last.is_a?(String)\n  if h.last.is_a?(Hash)\n    1\n  else\n    h.last.frobnicate_zzz\n  end\nend\n", Some("String"), false),
        // ---- the union family, and the `Unknown` arm the re-pin moved ---
        // `Bot ∪ String` is the carrier.
        ("chain_or_mixed", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash) || h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
        // The chain twin of `seq_projclass`/`seq_projsub`/`seq_projsub_or`.
        // These asserted `Some("String")` on the retired `:unknown stays
        // conservative` rule; re-measured against `ffb456b0` all three are
        // reference-SILENT (each a live false positive), and the widening
        // makes them DEAD. `chain_r7` below is the RBS-space `Unknown`, which
        // was silent through the DROP and is silent through the widening.
        ("chain_projclass", "class ProjBare; end\n\ndef f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(ProjBare)\n  h.last.frobnicate_zzz\nend\n", None, true),
        ("chain_projsub", "class ProjKlass < Hash; end\n\ndef f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(ProjKlass)\n  h.last.frobnicate_zzz\nend\n", None, true),
        ("chain_projsub_or", "class ProjKlass < Hash; end\n\ndef f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.is_a?(Hash) || h.last.is_a?(ProjKlass)\n  h.last.frobnicate_zzz\nend\n", None, true),
        // ---- must-still-fire controls -----------------------------------
        ("chain_ctrl_single", "def f(h)\n  return unless h.last.is_a?(String)\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
        // A rebind of the ROOT resets the address: the second guard mints.
        ("chain_ctrl_rebind", "def f(h, w)\n  return unless h.last.is_a?(String)\n  h = w\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", Some("Hash"), false),
        // A call ON the root invalidates the address (the existing
        // `invalidate_chain_after_call` port), so the second guard mints.
        ("chain_ctrl_pop_between", "def f(h)\n  return unless h.last.is_a?(String)\n  h.pop\n  return unless h.last.is_a?(Hash)\n  h.last.frobnicate_zzz\nend\n", Some("Hash"), false),
        // ---- declines that STAY -----------------------------------------
        // Two RBS-space names our resolver cannot order: the reference
        // proves them disjoint and is silent. Before the `v0.3.8` re-pin the
        // fact DROPPED here; it now WIDENS, so the site is dead instead of
        // merely factless — the same observable silence.
        ("chain_r7", "def f(h)\n  return unless h.last.is_a?(File::Stat)\n  return unless h.last.is_a?(URI::HTTP)\n  h.last.frobnicate_zzz\nend\n", None, true),
        // RECOGNITION gap (not this slice): `guard_predicate` requires a
        // bare LOCAL operand, so `===` / `nil?` on a chain receiver is
        // never a chain guard at all and the incoming fact dies at the
        // join. The reference fires on the first three — pure coverage.
        ("chain_caseeq_same", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless String === h.last\n  h.last.frobnicate_zzz\nend\n", None, false),
        ("chain_caseeq_subclass", "def f(h)\n  return unless h.last.is_a?(Numeric)\n  return unless Integer === h.last\n  h.last.frobnicate_zzz\nend\n", None, false),
        ("chain_nilq", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless h.last.nil?\n  h.last.frobnicate_zzz\nend\n", None, false),
        ("chain_caseeq_disjoint", "def f(h)\n  return unless h.last.is_a?(String)\n  return unless Hash === h.last\n  h.last.frobnicate_zzz\nend\n", None, false),
    ];
    assert_eq!(rows.len(), 26, "the chain probe matrix has 26 oracle-measured rows");
    for (row, src, expected, dead) in rows {
        let ast = lower_src(src.as_bytes());
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let scopes = lexical_scopes(&ast);
        let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let pass = typer.class_narrowing_pass(&ast, &mut i);
        let call = call_named(&ast, "frobnicate_zzz");
        assert_eq!(
            pass.calls.get(&call).map(String::as_str),
            *expected,
            "chain-guard row {row} snapshot\n--- source ---\n{src}"
        );
        assert_eq!(pass.dead.contains(&call), *dead, "chain-guard row {row} dead");
        if *row == "chain_ctrl_use_between" {
            let first = call_named(&ast, "frobnicate_yyy");
            assert_eq!(
                pass.calls.get(&first).map(String::as_str),
                Some("String"),
                "chain-guard row {row}: the use BETWEEN the guards fires"
            );
            assert!(!pass.dead.contains(&first), "row {row}: first use stays live");
        }
    }
}

/// JOIN RETENTION — a fact minted BEFORE a conditional survives it
/// ([`retain_joined_facts`]). Master blanket-wiped every `Narrowed` local
/// and every chain fact at each `if`/`unless`/`case` merge, so a fact died
/// at ANY later intervening conditional, terminating or not, related or not.
///
/// Every row was measured against the PINNED oracle (`v0.3.2`/`c6b91b9e`,
/// 2026-08-09, one fresh temp cwd per case, `--no-cache`, both reference
/// libs on `-I`). `Some(class)` — the reference fires `for <class>` and the
/// snapshot must carry it; `(None, true)` — the site must be DEAD; `(None,
/// false)` — no fact (either the reference is silent too, or a recorded
/// DECLINE, marked per row).
#[test]
fn class_narrowing_join_retention_matrix() {
    let rows: &[(&str, &str, Option<&str>, bool)] = &[
        // ---- the retention family: master silent, reference FIRES --------
        ("baseline_single_guard", "def f(a)\n  return unless a.is_a?(String)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // A guard on a DIFFERENT local killed the first one's fact.
        ("double_guard", "def f(a, b)\n  return unless a.is_a?(String)\n  return unless b.is_a?(Hash)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // A wholly unrelated NON-terminating `if`, with and without `else`.
        ("unrelated_nonterm_if", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("unrelated_nonterm_if_else", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  else\n    x = 2\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("unless_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  unless b\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("modifier_if_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  x = 1 if b\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("nested_intervening_if", "def f(a, b, c)\n  return unless a.is_a?(String)\n  if b\n    if c\n      x = 1\n    end\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("two_intervening_ifs", "def f(a, b, c)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  if c\n    y = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // The reference does NOT prune the statements after a conditional
        // whose branches BOTH terminate — the fact rides through.
        ("both_branches_terminate", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    return\n  else\n    return\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // …and one that terminates on a single edge whose guard map is empty
        // (the propagation carries nothing; only the retention fires here).
        ("single_terminating_unrelated", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    return\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // EXPRESSION position: the reference retains there too, so the
        // restore is NOT gated on `stmt_position` (unlike the propagation).
        ("expr_position_ternary", "def f(a, b)\n  return unless a.is_a?(String)\n  x = b ? 1 : 2\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("expr_position_if", "def f(a, b)\n  return unless a.is_a?(String)\n  x = if b\n    1\n  else\n    2\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // `case`: an unrelated subject, a `when` clause and an `in` clause.
        ("case_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  case b\n  when Integer\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("case_in_intervening", "def f(a, b)\n  return unless a.is_a?(String)\n  case b\n  in Integer\n    x = 1\n  else\n    x = 2\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // A guard on a CHAIN of another local (`b.length`), and the chain
        // twin of the whole family (a chain fact across an intervening if).
        ("three_guard_chain", "def f(a, b)\n  return unless a.is_a?(String)\n  return unless b.is_a?(String)\n  return unless b.length.is_a?(Integer)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("chain_intervening_if", "def f(h, b)\n  return unless h.last.is_a?(String)\n  if b\n    x = 1\n  end\n  h.last.frobnicate_zzz\nend\n", Some("String"), false),
        // The CENSUS row, reduced from gitlab-foss
        // `lib/bulk_imports/object_counter.rb:52`: a non-narrowing guard on
        // the SAME local (`empty?` / `key?` are not class guards, so they
        // contribute no guard map and the fact must simply survive).
        ("object_counter_reduced", "def f(x)\n  return unless x.is_a?(Hash)\n  return if x.empty?\n  x.frobnicate_zzz\nend\n", Some("Hash"), false),
        ("object_counter_block_form", "def f(x)\n  return unless x.is_a?(Hash)\n  if x.empty?\n    return\n  end\n  x.frobnicate_zzz\nend\n", Some("Hash"), false),
        ("nonnarrowing_guard_same_var", "def f(x)\n  return unless x.is_a?(Hash)\n  return if x.key?(:a)\n  x.frobnicate_zzz\nend\n", Some("Hash"), false),
        // The `if` with an `else` lowers its else clause to a clause-less
        // `BeginRescue` carrier; unwrapping it is what makes the `_else`
        // rows above pass (an else body of ANY shape used to wipe the edge).
        ("else_body_is_a_bare_literal", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    nil\n  else\n    nil\n  end\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        // ---- rows that already matched, and must not regress ------------
        ("unrelated_if_before_guard", "def f(a, b)\n  if b\n    x = 1\n  end\n  return unless a.is_a?(String)\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("intervening_method_call", "def f(a, b)\n  return unless a.is_a?(String)\n  b.to_s\n  a.frobnicate_zzz\nend\n", Some("String"), false),
        ("use_inside_nonterm_branch", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    a.frobnicate_zzz\n  end\nend\n", Some("String"), false),
        // ---- FP HAZARDS: every one measured reference-SILENT -------------
        // 1. A rebind of the target inside ONE branch. The reference fires a
        //    real union (`for 1 | String`) — the separate widen gap — so we
        //    must stay silent rather than witness `for String`.
        ("write_to_a_in_if", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    a = 1\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        ("branch_rebind_one_side", "def f(a, b, w)\n  return unless a.is_a?(String)\n  if b\n    a = w\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        ("rebind_in_else_only", "def f(a, b, w)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  else\n    a = w\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        ("ternary_rebinds_target", "def f(a, b, w)\n  return unless a.is_a?(String)\n  x = b ? (a = w) : 2\n  a.frobnicate_zzz\nend\n", None, false),
        // 4. A `case`/`in` pattern clause is NOT descended, so its rebind is
        //    invisible to the edge evidence — the span kill is what holds.
        ("case_in_rebinds_target", "def f(a, b)\n  return unless a.is_a?(String)\n  case b\n  in Integer\n    a = 1\n  else\n    a = 2\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        // A REBIND of a chain ROOT inside a branch kills the address.
        ("chain_root_rebind_in_if", "def f(h, b, w)\n  return unless h.last.is_a?(String)\n  if b\n    h = w\n  end\n  h.last.frobnicate_zzz\nend\n", None, false),
        // A plain CALL on the chain root inside a branch invalidates the
        // address (`invalidate_chain_after_call`) — invisible to `writes`,
        // caught only by the edge disagreement.
        ("chain_call_on_root_in_branch", "def f(h, b)\n  return unless h.last.is_a?(String)\n  if b\n    h.size\n  end\n  h.last.frobnicate_zzz\nend\n", None, false),
        ("chain_mutator_on_root_in_branch", "def f(h, b)\n  return unless h.last.is_a?(String)\n  if b\n    h.pop\n  end\n  h.last.frobnicate_zzz\nend\n", None, false),
        // 2. The conditional's OWN guard targets: a disjoint re-guard must
        //    still reach `Bot` and the use must be DEAD, both inside the
        //    branch and — the row that was a LIVE FP on master — after a
        //    later guard whose meet now sees the RESTORED incoming fact.
        ("own_guard_disjoint_after", "def f(a)\n  return unless a.is_a?(String)\n  if a.is_a?(Hash)\n    a.frobnicate_zzz\n  end\nend\n", None, true),
        ("guard_then_if_then_disjoint_guard", "def f(a, b)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  return unless a.is_a?(Hash)\n  a.frobnicate_zzz\nend\n", None, true),
        // …and the refining twin still fires `for Integer`.
        ("guard_then_if_then_subclass_guard", "def f(a, b)\n  return unless a.is_a?(Numeric)\n  if b\n    x = 1\n  end\n  return unless a.is_a?(Integer)\n  a.frobnicate_zzz\nend\n", Some("Integer"), false),
        // 5. The guard's OWN `if` with BOTH branches terminating stays
        //    silent (reference-measured) — there is no PRE-join fact to put
        //    back, so the retention cannot resurrect the declined
        //    propagation. Its positive-guard twin is a DECLINE below.
        ("t_both_terminate_negated", "def f(a)\n  if !a.is_a?(String)\n    return\n  else\n    return\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        // A `Bot` fact riding through an intervening `if` still suppresses.
        ("bot_intervening_if", "def f(b)\n  v = [1, 2]\n  return unless v.is_a?(Hash)\n  if b\n    x = 1\n  end\n  v.frobnicate_zzz\nend\n", None, true),
        // ---- DECLINES: the reference FIRES, we stay silent (coverage) ----
        // The conditional's own guard target after a NON-terminating merge:
        // the reference unions the edges back to the incoming class. We
        // exclude every target the edges disagree on, which is the whole
        // FP-safety of hazard 2 — this is the price.
        ("d_own_guard_target_after_join", "def f(a)\n  return unless a.is_a?(String)\n  if a.is_a?(Hash)\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        // The guard's own `if`, both branches terminating, guard on the
        // TRUTHY edge: the reference fires `for String`, we have no pre-join
        // fact and the propagation declines when both branches terminate.
        ("d_t_both_terminate_positive", "def f(a)\n  if a.is_a?(String)\n    return\n  else\n    return\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        // The `case` SUBJECT is excluded from the restore by construction.
        ("d_case_subject_is_target", "def f(a)\n  return unless a.is_a?(String)\n  case a\n  when Integer\n    x = 1\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        // A MUTATION of the target inside a branch drops the fact
        // (`kill_cenv_narrowed`); the reference keeps `Array` and fires.
        ("d_mutator_in_branch", "def f(a, b)\n  return unless a.is_a?(Array)\n  if b\n    a.push(1)\n  end\n  a.frobnicate_zzz\nend\n", None, false),
        // A `Narrowed` fact still does not enter a BLOCK body, nor survive a
        // block CALL — the block-boundary rules (`n_escape_after_if`, the
        // next/break matrix p9/p13) are deliberately untouched by this slice.
        ("d_use_in_block_after_if", "def f(a, b, xs)\n  return unless a.is_a?(String)\n  if b\n    x = 1\n  end\n  xs.each do |y|\n    a.frobnicate_zzz\n  end\nend\n", None, false),
        ("d_join_inside_block_use_outside", "def f(a, b, xs)\n  return unless a.is_a?(String)\n  xs.each do |i|\n    if b\n      x = 1\n    end\n  end\n  a.frobnicate_zzz\nend\n", None, false),
    ];
    assert_eq!(rows.len(), 42, "the join-retention matrix has 42 oracle-measured rows");
    for (row, src, expected, dead) in rows {
        let ast = lower_src(src.as_bytes());
        let index = CoreIndex::new();
        let source = SourceIndex::build(&ast, &index);
        let scopes = lexical_scopes(&ast);
        let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
        let mut i = Interner::new();
        let pass = typer.class_narrowing_pass(&ast, &mut i);
        let call = call_named(&ast, "frobnicate_zzz");
        assert_eq!(
            pass.calls.get(&call).map(String::as_str),
            *expected,
            "join-retention row {row} snapshot\n--- source ---\n{src}"
        );
        assert_eq!(pass.dead.contains(&call), *dead, "join-retention row {row} dead");
    }
}

/// Decline: safe-nav dispatch on the narrowed local never records.
#[test]
fn class_narrowing_safe_nav_declines() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash)\n    value&.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// `elsif` chains narrow each truthy arm independently (the chained `If`
/// lowers into the else branch).
#[test]
fn class_narrowing_elsif_arms_narrow_independently() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  if value.is_a?(Hash)\n    value.frobnicate_zzz\n  elsif value.is_a?(String)\n    value.other_zzz\n  end\nend\n",
    );
    assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
    assert_eq!(snaps.get(&call_named(&ast, "other_zzz")).map(String::as_str), Some("String"));
}

/// a3: `case value / when Hash / when String` narrows per clause; the
/// `else` body (a negative edge) is never narrowed.
#[test]
fn class_narrowing_case_when_narrows_per_clause() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  case value\n  when Hash\n    value.frobnicate_zzz\n  when String\n    value.frobnicate_yyy\n  else\n    value.else_zzz\n  end\nend\n",
    );
    assert_eq!(snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str), Some("Hash"));
    assert_eq!(
        snaps.get(&call_named(&ast, "frobnicate_yyy")).map(String::as_str),
        Some("String")
    );
    assert!(!snaps.contains_key(&call_named(&ast, "else_zzz")));
}

/// a6 decline: a multi-condition clause (`when Hash, String` — a union in
/// the reference) narrows NOTHING in this slice.
#[test]
fn class_narrowing_multi_condition_when_declines() {
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  case value\n  when Hash, String\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// Decline: a non-local subject, a non-constant condition, and a rebind
/// inside the clause body all narrow nothing.
#[test]
fn class_narrowing_case_declines() {
    // Subject is a call, not a bare local.
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  case value.foo\n  when Hash\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    // Condition is a literal, not a static constant.
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  case value\n  when 1\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    // Rebind inside the clause body invalidates subsequent uses.
    let (ast, snaps) = class_snaps(
        b"def f(value, other)\n  case value\n  when Hash\n    value = other\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// Decline: a shadowed constant in a `when` condition narrows nothing, and
/// facts do not enter a block body inside a clause.
#[test]
fn class_narrowing_case_shadow_and_block_declines() {
    let (ast, snaps) = class_snaps(
        b"class Hash\nend\ndef f(value)\n  case value\n  when Hash\n    value.frobnicate_zzz\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
    let (ast, snaps) = class_snaps(
        b"def f(value)\n  case value\n  when Hash\n    [1].each { |_i| value.frobnicate_zzz }\n  end\nend\n",
    );
    assert!(!snaps.contains_key(&call_named(&ast, "frobnicate_zzz")));
}

/// STAGE 3a-3 — single-hop chain guards, LOCAL roots
/// (docs/notes/20260807-narrowing-stage3-spec.md, "3a-3 BUILT").
///
/// Same convention as the 3a-1 and `next`/`break` matrices: `Some(c)` — the
/// reference FIRES and rigor-rs must record `c`; `None` — rigor-rs records
/// nothing, either because the reference is SILENT (recording would be a
/// live false positive) or because a decline costs coverage the reference
/// has (a strict subset, never an FP). Every row is oracle-measured against
/// the pinned reference `v0.3.1` from a FRESH temp cwd with `--no-cache`
/// and the checkout plugin path pinned; the row name is the probe name in
/// the note's tables.
///
/// The expectation is on the FIRST `frobnicate_zzz` call unless the row
/// name says otherwise (`f11` asserts both).
#[test]
fn class_narrowing_stage3a3_chain_guard_matrix() {
    const USE: &str = "h.last.frobnicate_zzz";
    const G: &str = "h.last.is_a?(String)";
    let f = |body: &str| format!("def f(h, g, xs, cond)\n{body}\nend\n");
    let rows: Vec<(&str, String, Option<&str>)> = vec![
        // ---- e: the spec's own c7 matrix, reproduced --------------------
        ("c7a", f(&format!("  {USE} if {G}")), Some("String")),
        // c7b: an IVAR root. The arena's `VariableRead` is NAMELESS, so
        // `stable_chain_address` cannot key it — a recorded coverage gap
        // (the reference fires `for String`).
        (
            "c7b_ivar",
            "class K\n  def f\n    @h.last.frobnicate_zzz if @h.last.is_a?(String)\n  end\nend\n"
                .to_string(),
            None,
        ),
        // c7c/f23: the reference KEEPS the fact through an argument-position
        // mention of the root; we kill on ANY mention. Coverage only.
        ("c7c_arg_mention", f(&format!("  if {G}\n    g(h)\n    {USE}\n  end")), None),
        ("f23_push", f(&format!("  if {G}\n    xs.push(h)\n    {USE}\n  end")), None),
        // c7d: a call whose RECEIVER is the root invalidates — reference
        // silent, so keeping the fact would be a live FP.
        ("c7d_pop", f(&format!("  if {G}\n    h.pop\n    {USE}\n  end")), None),
        // c7e: arguments on the hop ⇒ no stable address, on both engines.
        (
            "c7e_args_on_hop",
            f("  h.fetch(0).frobnicate_zzz if h.fetch(0).is_a?(String)"),
            None,
        ),
        // c7g: the REBIND control. The reference fires here, but with a
        // DIFFERENT diagnostic (`for nil`, folding the rebound `[].last`),
        // so the write-kill must make us silent.
        ("c7g_rebind", f(&format!("  if {G}\n    h = []\n    {USE}\n  end")), None),
        ("c7h_inert", f(&format!("  if {G}\n    x = 1\n    {USE}\n  end")), Some("String")),
        ("h1_return_unless", f(&format!("  return unless {G}\n  {USE}")), Some("String")),
        // ---- a: the chain guard as a CONJUNCT ---------------------------
        ("a_conj_right_then", f(&format!("  if cond && {G}\n    {USE}\n  end")), Some("String")),
        ("a_conj_left_then", f(&format!("  if {G} && cond\n    {USE}\n  end")), Some("String")),
        (
            "a_conj_elsif",
            f(&format!("  if g\n    1\n  elsif cond && {G}\n    {USE}\n  end")),
            Some("String"),
        ),
        // The `&&` FALSEY edge of an atomic chain guard is empty — the
        // reference is silent in the `else`, so narrowing there would be an FP.
        ("a_conj_else_ctl", f(&format!("  if cond && {G}\n    1\n  else\n    {USE}\n  end")), None),
        ("a_conj_mid", f(&format!("  if g && {G} && cond\n    {USE}\n  end")), Some("String")),
        // An `||` with an unrecognised disjunct joins in the un-narrowed
        // scope ⇒ nothing, on both engines.
        ("a_or_disjunct_ctl", f(&format!("  if {G} || cond\n    {USE}\n  end")), None),
        // A LOCAL guard and a CHAIN guard in one predicate are independent
        // targets — both apply.
        (
            "a_conj_localguard_mix",
            "def f(h, v)\n  if v.is_a?(Integer) && h.last.is_a?(String)\n    h.last.frobnicate_zzz\n  end\nend\n"
                .to_string(),
            Some("String"),
        ),
        // ---- b: the `!` swap and falsey-edge termination ----------------
        ("b_bang_else", f(&format!("  if !{G}\n    1\n  else\n    {USE}\n  end")), Some("String")),
        ("b_bang_then_ctl", f(&format!("  if !{G}\n    {USE}\n  end")), None),
        ("b_return_if_bang", f(&format!("  return if !{G}\n  {USE}")), Some("String")),
        (
            "b_unless_stmt",
            f(&format!("  unless {G}\n    1\n  else\n    {USE}\n  end")),
            Some("String"),
        ),
        (
            "b_return_unless_compound",
            f(&format!("  return unless cond && {G}\n  {USE}")),
            Some("String"),
        ),
        ("b_raise_unless", f(&format!("  raise 'x' unless {G}\n  {USE}")), Some("String")),
        // The `next`/`break` termination slice composes with chain facts.
        (
            "b_next_unless",
            f(&format!("  xs.each do |_x|\n    next unless {G}\n    {USE}\n  end")),
            Some("String"),
        ),
        // ---- c: disjoint / `Bot` interaction ----------------------------
        // A PRECISE chain carrier: the reference collapses `h.last`
        // (`Integer`) to `Bot` under a `String` guard and is SILENT. Our
        // carrier gate reads the SAME node's type and declines the mint —
        // the same silence by a different route, and no chain `Bot` fact.
        ("c_bot_precise_root", f("  h = [1, 2]\n  h.last.frobnicate_zzz if h.last.is_a?(String)"), None),
        // The must-still-fire twin of the row above.
        ("c_must_still_fire", f(&format!("  {USE} if {G}")), Some("String")),
        // A LOCAL collapsed to `Bot` beside a chain guard does NOT suppress
        // the chain witness — the reference fires (`out.dead` keys on the
        // local's own calls, and the chain call's receiver is not that local).
        (
            "c_chain_and_local_bot",
            "def f(h)\n  v = [1, 2]\n  if v.is_a?(String) && h.last.is_a?(String)\n    h.last.frobnicate_zzz\n  end\nend\n"
                .to_string(),
            Some("String"),
        ),
        // ---- d: the same address re-guarded in sequence -----------------
        // The sequential-disjoint hazard. The reference carries `String`
        // into the second guard and collapses to `Bot`; without the
        // pre-join re-seed in `class_flow_if` we would witness `for Hash`.
        (
            "d_seq_two_returns_disjoint",
            f(&format!("  return unless {G}\n  return unless h.last.is_a?(Hash)\n  {USE}")),
            None,
        ),
        (
            "d_seq_and_disjoint",
            f(&format!("  if {G} && h.last.is_a?(Hash)\n    {USE}\n  end")),
            None,
        ),
        // A SUBCLASS re-guard REFINES to the more specific class. Was a
        // recorded decline (the blind R3 drop) until the 2026-08-09
        // chain-guard meet; the reference fires `for Integer` (probe
        // `chain_subclass`) and now so do we — see
        // `class_narrowing_chain_guard_meet_matrix`.
        (
            "d_seq_subclass",
            f("  return unless h.last.is_a?(Numeric)\n  return unless h.last.is_a?(Integer)\n  h.last.frobnicate_zzz"),
            Some("Integer"),
        ),
        (
            "d_seq_same_class",
            f(&format!("  return unless {G}\n  return unless {G}\n  {USE}")),
            Some("String"),
        ),
        ("d_nested_reguard", f(&format!("  if {G}\n    if h.last.is_a?(Hash)\n      {USE}\n    end\n  end")), None),
        // ---- carrier hazards on the ADDRESS -----------------------------
        // A `||`-bound ROOT still narrows: the PR #72 carrier ALLOW-LIST is
        // a per-LOCAL rule and must NOT be applied to a chain address (the
        // reference fires — applying it would be pure coverage loss).
        ("k_root_or_union", "def f(a, b)\n  h = a || b\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
        ("k_root_splat", "def f(spec)\n  h = *spec\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
        ("k_root_from_call", "def f(x)\n  h = x.fetch(:a)\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
        ("k_root_kwarg", "def f(h: nil)\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(), Some("String")),
        // Precise carriers the reference collapses — all reference-SILENT,
        // all declined by the Dynamic/Top carrier gate.
        ("k_root_hash_lit", "def f\n  h = { a: 1 }\n  h.size.frobnicate_zzz if h.size.is_a?(String)\nend\n".to_string(), None),
        ("k_root_str_lit", "def f\n  h = 'abc'\n  h.upcase.frobnicate_zzz if h.upcase.is_a?(Hash)\nend\n".to_string(), None),
        ("k_root_int_lit", "def f\n  h = 3\n  h.succ.frobnicate_zzz if h.succ.is_a?(String)\nend\n".to_string(), None),
        // ---- guard family / shape variants ------------------------------
        ("m_kind_of", f("  h.last.frobnicate_zzz if h.last.kind_of?(String)"), Some("String")),
        ("m_instance_of", f("  h.last.frobnicate_zzz if h.last.instance_of?(String)"), Some("String")),
        // DECLINE: `===` is non-mintable (3a-1's own finding) and never
        // produces a chain target. The reference narrows through it.
        ("m_case_eq", f("  h.last.frobnicate_zzz if String === h.last"), None),
        // DECLINE: safe-nav, on the hop or on the use. Reference fires.
        ("m_safe_nav_hop", f("  h&.last.frobnicate_zzz if h&.last.is_a?(String)"), None),
        ("m_safe_nav_use", f(&format!("  h.last&.frobnicate_zzz if {G}")), None),
        // A block on the hop: no stable address, reference silent too.
        ("m_block_on_hop", f("  h.map { |x| x }.frobnicate_zzz if h.map { |x| x }.is_a?(String)"), None),
        // The OUTER call's own arguments and block are irrelevant — the
        // reference narrows the RECEIVER expression, so both fire.
        ("m_use_with_args", f(&format!("  h.last.frobnicate_zzz(1) if {G}")), Some("String")),
        ("m_use_with_block", f(&format!("  h.last.frobnicate_zzz {{ |x| x }} if {G}")), Some("String")),
        // Single-hop only; a different method or root is a different address.
        ("m_two_hop", f("  h.first.last.frobnicate_zzz if h.first.last.is_a?(String)"), None),
        ("m_different_method", f(&format!("  h.first.frobnicate_zzz if {G}")), None),
        ("m_different_root", f(&format!("  g.last.frobnicate_zzz if {G}")), None),
        // DECLINE: a project declaration shadowing the guard class declines
        // the whole guard (shared with stages 1-2). Reference fires.
        (
            "m_shadowed_const",
            "class String\nend\ndef f(h)\n  h.last.frobnicate_zzz if h.last.is_a?(String)\nend\n".to_string(),
            None,
        ),
        ("m_dynamic_const", "def f(h, c)\n  h.last.frobnicate_zzz if h.last.is_a?(c)\nend\n".to_string(), None),
        // ---- invalidation -----------------------------------------------
        ("n_root_mutator", f(&format!("  if {G}\n    h << 1\n    {USE}\n  end")), None),
        // A call whose receiver is the ADDRESS (not the root) does NOT
        // invalidate, on either engine.
        ("n_call_on_address", f(&format!("  if {G}\n    h.last.strip\n    {USE}\n  end")), Some("String")),
        ("n_address_receiver_call", f(&format!("  if {G}\n    h.last << g\n    {USE}\n  end")), Some("String")),
        ("n_root_write_after", f(&format!("  if {G}\n    {USE}\n    h = xs\n  end")), Some("String")),
        ("n_root_opwrite", f(&format!("  if {G}\n    h += xs\n    {USE}\n  end")), None),
        // DECLINE: chain facts do not cross a block boundary, in or out.
        ("n_into_block", f(&format!("  if {G}\n    xs.each {{ |_x| {USE} }}\n  end")), None),
        ("n_after_block", f(&format!("  if {G}\n    xs.each {{ |_x| 1 }}\n    {USE}\n  end")), None),
        // A fact minted in a branch does NOT escape it — reference-silent.
        ("n_escape_after_if", f(&format!("  if {G}\n    1\n  end\n  {USE}")), None),
        ("n_in_nested_if", f(&format!("  if {G}\n    if cond\n      {USE}\n    end\n  end")), Some("String")),
        ("n_root_as_arg_to_mutator", f(&format!("  if {G}\n    xs.fill(h)\n    {USE}\n  end")), None),
        ("n_use_before_guard", f(&format!("  {USE}\n  return unless {G}")), None),
        // ---- the three verified corpus shapes, reduced ------------------
        // (over a TOP-LEVEL guard class: the corpus rows themselves name
        // `Bundler::Source::Git`, and `check_narrowed_call`'s
        // `knows_toplevel_class` gate cannot witness a NAMESPACED class —
        // a pre-existing consumption limit this slice does not change, and
        // the reason the gap diff is 0. See the note's "BUILT" section.)
        (
            "w1_elsif_conj",
            "def f(dep, defn_dep, cond)\n  if dep.nil?\n    1\n  elsif cond && defn_dep.source.is_a?(String)\n    defn_dep.source.frobnicate_zzz\n  end\nend\n".to_string(),
            Some("String"),
        ),
        (
            "w2_index_write_if_mod",
            "def f(dep)\n  details = {}\n  details[:commit_sha] = dep.source.frobnicate_zzz if dep.source.instance_of?(String)\n  details\nend\n".to_string(),
            Some("String"),
        ),
        (
            "w3_return_unless",
            "def f(dep)\n  return unless dep.source.is_a?(String)\n\n  dep.source.frobnicate_zzz\nend\n".to_string(),
            Some("String"),
        ),
        (
            "w4_return_if_mod",
            "def f(spec)\n  return spec.source.frobnicate_zzz if spec.source.instance_of?(String)\n\n  nil\nend\n".to_string(),
            Some("String"),
        ),
        // ---- residual composition --------------------------------------
        ("x_two_addresses_one_root", f("  if h.first.is_a?(String) && h.last.is_a?(Hash)\n    h.first.frobnicate_zzz\n  end"), Some("String")),
        ("x_same_addr_two_roots", f(&format!("  if {G} && g.last.is_a?(Hash)\n    {USE}\n  end")), Some("String")),
        ("x_chain_in_begin", f(&format!("  return unless {G}\n\n  begin\n    {USE}\n  rescue StandardError\n    nil\n  end")), Some("String")),
        // DECLINE (carried from 3b-1): survival PAST a `begin` — a
        // `begin`/`rescue` is not a conditional join, so the join-retention
        // slice does not reach it.
        ("x_chain_after_begin", f(&format!("  return unless {G}\n\n  begin\n    1\n  rescue StandardError\n    nil\n  end\n  {USE}")), None),
        // …but survival past a `case` is CLOSED by the join-retention slice
        // (2026-08-09): the subject is an unrelated local, every clause was
        // descended and left the address alone, so the pre-`case` fact comes
        // back. Re-measured against the v0.3.2 oracle: the reference fires
        // `for String` here.
        ("x_chain_after_case", f(&format!("  return unless {G}\n\n  case cond\n  when 1 then 2\n  end\n  {USE}")), Some("String")),
        ("x_chain_in_loop_pred", f(&format!("  return unless {G}\n\n  while {USE}\n    break\n  end")), Some("String")),
        ("x_chain_in_case_clause", f(&format!("  return unless {G}\n\n  case cond\n  when 1 then {USE}\n  end")), Some("String")),
        ("x_chain_in_array_lit", f(&format!("  return unless {G}\n\n  x = [{USE}]\n  x")), Some("String")),
        ("x_chain_ternary", f(&format!("  return unless {G}\n\n  cond ? {USE} : 1")), Some("String")),
        // DECLINE: 3a-2 (`Logical` statement minting) is DEFERRED, so
        // `guard or raise` mints nothing. The reference fires.
        ("x_chain_or_raise", f(&format!("  {G} or raise 'no'\n  {USE}")), None),
        ("x_chain_guard_root_also_local", f(&format!("  if h.is_a?(Array) && {G}\n    {USE}\n  end")), Some("String")),
        // A rebind of the root inside the conditional's span declines the
        // propagation; a rebind before the use kills the fact.
        ("x_root_rebind_in_span", f(&format!("  if {G}\n    h = xs\n  end\n  {USE}")), None),
        ("x_root_rebind_then_return", f(&format!("  return unless {G}\n\n  h = xs\n  {USE}")), None),
        ("x_chain_multiwrite_root", f(&format!("  if {G}\n    h, _y = xs, 1\n    {USE}\n  end")), None),
        // DECLINE: an `||` of DIFFERENT classes is a real union (3a-4).
        ("x_chain_reguard_or", f(&format!("  if {G} || h.last.is_a?(Hash)\n    {USE}\n  end")), None),
        ("x_chain_bang_or", f(&format!("  return if !{G} || h.last.nil?\n\n  {USE}")), Some("String")),
        // A nested `def` is an independent scope — no fact crosses in.
        ("x_chain_nested_def", f(&format!("  return unless {G}\n\n  def q(h)\n    {USE}\n  end")), None),
        ("x_chain_use_as_arg", f(&format!("  return unless {G}\n\n  g({USE})")), Some("String")),
        ("x_chain_use_as_return", f(&format!("  return unless {G}\n\n  return {USE}")), Some("String")),
    ];
    for (row, src, expected) in &rows {
        let (ast, snaps) = class_snaps(src.as_bytes());
        let got = snaps.get(&call_named(&ast, "frobnicate_zzz")).map(String::as_str);
        assert_eq!(got, *expected, "stage 3a-3 matrix row {row}\n--- source ---\n{src}");
    }
}

/// f11 — the fact SURVIVES its own re-read: BOTH chain uses in one branch
/// are recorded (the reference fires twice). Split out of the matrix
/// because `call_named` only reaches the first call of a name.
#[test]
fn class_narrowing_stage3a3_chain_fact_survives_its_own_reread() {
    let src = b"def f(h)\n  if h.last.is_a?(String)\n    h.last.frobnicate_zzz\n    h.last.frobnicate_zzz\n  end\nend\n";
    let (ast, snaps) = class_snaps(src);
    let uses: Vec<_> = ast
        .iter()
        .filter_map(|(id, n)| match n {
            Node::Call { method, .. } if method == "frobnicate_zzz" => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(uses.len(), 2, "two chain uses");
    for id in uses {
        assert_eq!(snaps.get(&id).map(String::as_str), Some("String"), "f11: both uses record");
    }
}
