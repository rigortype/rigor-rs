use super::*;
use rigor_parse::{lower, parse, Node};

/// The collection-shape snapshot map for `src`, wired exactly as the analyze
/// pass wires it (per-file source index + lexical scopes).
fn coll_snaps(src: &[u8]) -> (LoweredAst, HashMap<NodeId, &'static str>) {
    let ast = lower(&parse(src));
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let scopes = lexical_scopes(&ast);
    let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
    let mut i = Interner::new();
    let snaps = typer.collection_shape_snapshots(&ast, &mut i);
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

fn snap(src: &[u8], method: &str) -> Option<&'static str> {
    let (ast, snaps) = coll_snaps(src);
    snaps.get(&call_named(&ast, method)).copied()
}

// --- FIRES ------------------------------------------------------------

/// m01: straight-line `<<` widens the seed to `Array`, and the branch-
/// contained mutation that follows joins IDENTICALLY (both edges already
/// `Nominal[Array]`), so the use after the `if` still dispatches on Array.
#[test]
fn coll_m01_straight_line_then_branch_mutation_fires() {
    assert_eq!(
        snap(
            b"def f(c)\n  output = []\n  output << 'a'\n  output << 'b' if c\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Array"),
    );
}

/// m02: a block-contained `<<` on a captured local REPLACES the outer
/// binding (`widen_after_block`) — an unmutated seed is enough.
#[test]
fn coll_m02_each_block_mutation_fires() {
    assert_eq!(
        snap(
            b"def f(xs)\n  output = []\n  xs.each do |x|\n    output << x\n  end\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Array"),
    );
}

/// m02b: the mutation inside the block is itself BRANCH-contained. The
/// reference's `widen_after_block` is a syntactic walk against the outer
/// scope (its doc names `arr.push(x) if cond`), so this still fires — unlike
/// the same shape in the METHOD body (m20), which goes through `Scope#join`.
/// The gitlab jira-tracker / ddl-lock survey rows have exactly this shape.
#[test]
fn coll_m02b_branch_contained_block_mutation_fires() {
    assert_eq!(
        snap(
            b"def f(xs, c)\n  output = []\n  xs.each do |x|\n    if c\n      output << x\n    end\n  end\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Array"),
    );
    // …and the modifier form behind a `next` guard (the ddl-lock row).
    assert_eq!(
        snap(
            b"def f(xs)\n  output = []\n  xs.each do |x|\n    next if x.nil?\n\n    output << x if x\n  end\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Array"),
    );
}

/// A block-local seed must not leak outwards: only a local ALREADY carrying
/// a collection at the call can widen.
#[test]
fn coll_block_mutation_needs_outer_carrier() {
    assert_eq!(
        snap(
            b"def f(xs)\n  xs.each do |x|\n    inner = []\n    inner << x\n  end\n  inner.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m03: `[]=` on a `{}` seed widens to `Hash` and STAYS Hash across further
/// index assignments (the already-nominal carrier re-asserts itself).
#[test]
fn coll_m03_hash_index_assign_fires() {
    assert_eq!(
        snap(
            b"def f(v)\n  project = {}\n  project[:a] = v\n  project[:b] = v\n  project.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Hash"),
    );
}

/// m06: no alias tracking — `b = a; b << 1` widens only `b`; `a` keeps its
/// `Tuple[]`, which dispatches as Array all the same
/// (`receiver_descriptor:209`). BOTH uses fire.
#[test]
fn coll_m06_alias_both_locals_fire() {
    let (ast, snaps) =
        coll_snaps(b"def f\n  a = []\n  b = a\n  b << 1\n  a.first_zzz\n  b.second_zzz\nend\n");
    assert_eq!(snaps.get(&call_named(&ast, "first_zzz")).copied(), Some("Array"));
    assert_eq!(snaps.get(&call_named(&ast, "second_zzz")).copied(), Some("Array"));
}

/// m07: an escape into an UNRESOLVED callee does not widen (the reference
/// does not model unknown-callee mutation either).
#[test]
fn coll_m07_unknown_callee_escape_fires() {
    assert_eq!(
        snap(
            b"def f\n  output = []\n  output << 'a'\n  helper_zzz(output)\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Array"),
    );
}

/// m14: a multi-write seeds each target with its own `Tuple[]`; mutating one
/// leaves the other a bare Tuple. Both dispatch as Array.
#[test]
fn coll_m14_multi_write_seeds_both_fire() {
    let (ast, snaps) =
        coll_snaps(b"def f\n  a, b = [], []\n  a << 1\n  a.first_zzz\n  b.second_zzz\nend\n");
    assert_eq!(snaps.get(&call_named(&ast, "first_zzz")).copied(), Some("Array"));
    assert_eq!(snaps.get(&call_named(&ast, "second_zzz")).copied(), Some("Array"));
}

/// m17: `push` then `concat` — a chain of mutators all keeps the nominal.
#[test]
fn coll_m17_push_then_concat_fires() {
    assert_eq!(
        snap(
            b"def f(xs)\n  output = []\n  output.push('a')\n  output.concat(xs)\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Array"),
    );
}

/// m19: straight-line widening BEFORE a `case` makes every clause's join
/// edge agree, so the use after the `case` still dispatches on Array. This
/// is the exact contrast with m18 below.
#[test]
fn coll_m19_prewidened_case_mutation_fires() {
    assert_eq!(
        snap(
            b"def f(x)\n  output = []\n  output << 'a'\n  case x\n  when 1 then output << 'b'\n  when 2 then output << 'c'\n  end\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        Some("Array"),
    );
}

/// A plain literal seed with NO mutation at all still dispatches as Array /
/// Hash inside a `def` body (`Tuple`/`HashShape` project to the collection
/// descriptor) — the base case the mutation rows build on.
#[test]
fn coll_bare_literal_seed_fires() {
    assert_eq!(
        snap(b"def f\n  output = [1, 2]\n  output.frobnicate_zzz\nend\n", "frobnicate_zzz"),
        Some("Array"),
    );
    assert_eq!(
        snap(b"def f\n  h = { a: 1 }\n  h.frobnicate_zzz\nend\n", "frobnicate_zzz"),
        Some("Hash"),
    );
}

// --- SILENT (the FP-safety envelope) ----------------------------------

/// m04: a straight-line rebind kills the carrier.
#[test]
fn coll_m04_rebind_silent() {
    assert_eq!(
        snap(
            b"def f(x)\n  output = []\n  output << 'a'\n  output = x\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m05: a BRANCH rebind kills it too — the join sees two different carriers.
#[test]
fn coll_m05_branch_rebind_silent() {
    assert_eq!(
        snap(
            b"def f(x, c)\n  output = []\n  output << 'a'\n  output = x if c\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m08: a Dynamic (parameter) seed is never MINTED into a collection by a
/// mutation — the decline that keeps us out of the reference's own
/// runtime-wrong `[]=`-on-a-String rows (bucket E, probe c12).
#[test]
fn coll_m08_param_seed_silent() {
    assert_eq!(snap(b"def f(a)\n  a << 1\n  a.frobnicate_zzz\nend\n", "frobnicate_zzz"), None);
}

/// m13: same, for a local bound to a Dynamic value.
#[test]
fn coll_m13_dynamic_carrier_mutation_silent() {
    assert_eq!(
        snap(
            b"def f(x)\n  output = x\n  output << 1\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m15: a REBIND inside a block body kills the outer carrier (only a kept
/// nominal ever propagates out of a block).
#[test]
fn coll_m15_block_rebind_silent() {
    assert_eq!(
        snap(
            b"def f(xs)\n  output = []\n  output << 'a'\n  xs.each { |_x| output = nil }\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m18 — LOAD-BEARING: a `case`-contained mutation on a NOT-yet-widened seed
/// leaves `Tuple[] | Array[…]` after the reference's `Scope#join`, and
/// `receiver_descriptor` has no `Type::Union` arm, so the reference is
/// SILENT. We must never model that union.
#[test]
fn coll_m18_unwidened_case_mutation_silent() {
    assert_eq!(
        snap(
            b"def f(x)\n  output = []\n  case x\n  when 1 then output << 'a'\n  when 2 then output << 'b'\n  end\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m20 — LOAD-BEARING, the `if` twin of m18.
#[test]
fn coll_m20_unwidened_if_mutation_silent() {
    assert_eq!(
        snap(
            b"def f(c)\n  output = []\n  output << 'a' if c\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m16: an op-write (`output += [1]`) FIRES in the reference (the `Tuple +
/// Tuple` fold keeps the literal shape) — a deliberate coverage give-up
/// here, per §5.5 of the spec. Pinned so the give-up stays visible.
#[test]
fn coll_m16_op_write_silent_coverage_giveup() {
    assert_eq!(
        snap(
            b"def f\n  output = []\n  output += [1]\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// m09 / bucket B: an IVAR carrier is never typed by this slice (the
/// reference DOES fire cross-method; that substrate is its own future
/// slice).
#[test]
fn coll_ivar_carrier_silent() {
    assert_eq!(
        snap(
            b"class K\n  def g\n    @h = {}\n    @h[:a] = 1\n    @h.frobnicate_zzz\n  end\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// Safe-nav dispatch is outside the envelope on BOTH sides: a `&.` mutation
/// widens nothing and a `&.` use records nothing.
#[test]
fn coll_safe_nav_silent() {
    assert_eq!(
        snap(
            b"def f\n  output = []\n  output << 'a'\n  output&.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// `while`/`until` bodies are unmodeled (the reference fires — probe m10 —
/// but its `break`/`next` join edges are unprobed, so stage 1 declines).
#[test]
fn coll_while_loop_silent() {
    assert_eq!(
        snap(
            b"def f(c)\n  output = []\n  while c\n    output << 1\n  end\n  output.frobnicate_zzz\nend\n",
            "frobnicate_zzz",
        ),
        None,
    );
}

/// A `def` body is an INDEPENDENT local scope: a top-level seed never leaks
/// into it.
#[test]
fn coll_def_body_scope_isolation_silent() {
    assert_eq!(
        snap(b"output = []\ndef f\n  output.frobnicate_zzz\nend\n", "frobnicate_zzz"),
        None,
    );
}
