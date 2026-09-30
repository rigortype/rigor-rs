use super::*;
use rigor_parse::{lower, parse, Node};

fn ty_of_last_recv_call(src: &[u8]) -> String {
    let ast = lower(&parse(src));
    let index = CoreIndex::new();
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
}

/// Slice 4: a class-method call on a core Singleton types its unanimous RBS
/// return (source-range Nominal for classes outside the 9-class core table).
#[test]
fn singleton_rbs_return_types_time_now() {
    // Core-table return resolves to the core Nominal directly
    // (`describe` renders a Nominal by id: Integer = Class<1>).
    assert_eq!(ty_of_last_recv_call(b"s = Integer.sqrt(4)\n"), "Class<1>");
    // Divergent overloads (Regexp.last_match) stay Dynamic on THIS path.
    assert_eq!(ty_of_last_recv_call(b"m = Regexp.last_match(2)\n"), "Dynamic[top]");
}

/// Issue #118 — upstream #521 (`3d5dddbb`) in the GENERIC receiver
/// dispatch. Tier 3's flat slot may answer a bare `Nominal[C]` only when the
/// reference's join over the surviving overloads IS one. With a
/// reference-untyped argument two shapes decline: a NILABLE return
/// (`String#[] -> String?`, whose four overloads agree, so the slot's
/// all-agree collapse cannot see the nil bit) and an ERASURE-only agreement
/// (`Array#product`'s `Array[[E, X]]` vs `Array[Array[E | U]]`, which the
/// flat slot reads as "both Array"). The controls keep their answers: an
/// overload set that genuinely agrees on a bare class, and any call whose
/// argument the reference can value-fold.
#[test]
fn generic_dispatch_declines_under_an_untyped_argument() {
    // (1) nilable join — issue #118's own row.
    assert_eq!(ty_of_last_recv_call(b"def f(u)\n  \"abc\"[u]\nend\n"), "Dynamic[top]");
    assert_eq!(
        ty_of_last_recv_call(b"def f(u)\n  \"abc\".byteslice(u)\nend\n"),
        "Dynamic[top]"
    );
    // (2) the candidates agree only after erasure.
    assert_eq!(
        ty_of_last_recv_call(b"def f(u)\n  [1, 2].product(u)\nend\n"),
        "Dynamic[top]"
    );
    // (3) CONTROL — one matching overload returning a bare `String`: both
    // engines keep witnessing on it.
    assert_eq!(
        ty_of_last_recv_call(b"def f(u)\n  \"abc\".center(u)\nend\n"),
        "Class<0>"
    );
    // (4) CONTROL — a LITERAL argument is not reference-untyped, and the
    // reference constant-folds the call to a `String` value, so withholding
    // here would LOSE a matched row. Since issue #121 the port folds it too.
    assert_eq!(ty_of_last_recv_call(b"def f\n  \"abc\"[0]\nend\n"), "Constant[\"a\"]");
}

/// Issue #146 — a `Constant` receiver's call folds MEMBER-WISE when an
/// argument reaches more than one distinct precise value (`v = 1; v = 2 if
/// c`): the reference answers the union of the per-member folds — `"abc"[v]`
/// -> `"b" | "c"`, `1.fdiv(v)` -> `1.0 | 0.5` — a carrier no negative rule
/// fires on, never the flat `method_return` class. Tier 3 withholds the
/// nominal on that `multi` reach; a single reaching value keeps it, and a
/// literal argument still pins its folded `Constant`. The Kernel folds are
/// deliberately untouched — the reference joins THEIR overloads to the
/// conversion class on the same input (`Float(v)` fires `for Float`).
#[test]
fn constant_receiver_declines_under_a_multi_value_argument() {
    // The issue's row 2, verbatim.
    assert_eq!(
        ty_of_last_recv_call(b"def g(c)\n  v = 1\n  v = 2 if c\n  \"abc\"[v]\nend\n"),
        "Dynamic[top]"
    );
    // The same shape on another value-pinned receiver, and at top level
    // (where the propagate'd scope joins the same writes).
    assert_eq!(
        ty_of_last_recv_call(b"def g(c)\n  v = 1\n  v = 2 if c\n  1.fdiv(v)\nend\n"),
        "Dynamic[top]"
    );
    assert_eq!(
        ty_of_last_recv_call(b"v = 1\nv = 2 if $c\n\"abc\"[v]\n"),
        "Dynamic[top]"
    );
    // CONTROLS.
    assert_eq!(
        ty_of_last_recv_call(b"def g\n  v = 1\n  \"abc\"[v]\nend\n"),
        "Class<0>"
    );
    assert_eq!(ty_of_last_recv_call(b"\"abc\"[1]\n"), "Constant[\"b\"]");
    // And the Kernel folds keep their answers on the same `1 | 2` local —
    // the reference joins their overloads to the conversion class
    // (`Float(v)` -> `Float`, so `.to_s` -> `String`); the `multi` flag is
    // only for a `Constant` receiver's member-wise fold.
    assert_eq!(
        ty_of_last_recv_call(b"def g(c)\n  v = 1\n  v = 2 if c\n  Float(v).to_s\nend\n"),
        "Class<0>"
    );
}

/// Issue #121 — a class-GUARDED parameter is refused by the untyped
/// allow-list, but a NILABLE return still gives up the flat slot for it: the
/// reference cannot fold a guarded parameter, so its join keeps the nil arm.
/// The controls keep their answers: a precise write reaching the read, a
/// non-nilable return, and the erasure family (a typed argument can narrow
/// `Array#product` back to overloads the reference fires on).
#[test]
fn nilable_dispatch_declines_under_a_guarded_parameter() {
    let guarded = |body: &str| {
        let src = format!("def f(u)\n  return unless u.is_a?(Integer)\n  {body}\nend\n");
        ty_of_last_recv_call(src.as_bytes())
    };
    assert_eq!(guarded("\"abc\"[u]"), "Dynamic[top]");
    assert_eq!(guarded("\"abc\".byteslice(u)"), "Dynamic[top]");
    assert_eq!(guarded("[1, 2].index(u)"), "Dynamic[top]");
    // CONTROLS.
    assert_eq!(guarded("\"abc\".center(u)"), "Class<0>");
    assert_ne!(guarded("[1, 2].product(u)"), "Dynamic[top]");
    assert_eq!(
        ty_of_last_recv_call(
            b"def f(u)\n  u = 1\n  return unless u.is_a?(Integer)\n  \"abc\"[u]\nend\n"
        ),
        "Class<0>"
    );
    // A `case` guard's narrowed `Nominal[Integer]` is not a foldable
    // `Constant` either — the reference's `String | nil` join stands —
    // and issue #332's `pins_one_constant` gate now sees that (this row's
    // `Class<0>` used to be a recorded FP).
    assert_eq!(
        ty_of_last_recv_call(b"def f(u)\n  case u\n  when Integer then \"abc\"[u]\n  end\nend\n"),
        "Dynamic[top]"
    );
}

/// Slice 2/3: Kernel#Array folds by argument type; rand types by arity.
///
/// The two UNTYPED-parameter rows moved at the `v0.3.4 → v0.3.8` re-pin:
/// upstream #521 (`3d5dddbb`) stops pinning one overload when the argument
/// cannot discriminate, so `Array(c)` and `rand(c)` over a bare parameter
/// now answer `Dynamic[top]` on both engines (fixture 99 rows a4/a5, and
/// fixture 67's own `Array(config).presence` / `rand(n).frobnicate`, which
/// were two of the four re-pin false positives).
#[test]
fn kernel_array_and_rand_type() {
    let ty = |src: &[u8]| -> String {
        let ast = lower(&parse(src));
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .filter_map(|(id, n)| {
                matches!(n, Node::Call { receiver: None, .. }).then_some(id)
            })
            .last()
            .unwrap();
        let t = typer.type_of(&ast, call_id, &env, &mut i);
        rigor_types::describe(&i, t)
    };
    // Tuple identity / nil collapse / scalar wrap / nominal fallback.
    assert_eq!(ty(b"Array([1, 2])\n"), "Tuple[Constant[1], Constant[2]]");
    assert_eq!(ty(b"Array(nil)\n"), "Tuple[]");
    assert_eq!(ty(b"Array(5)\n"), "Tuple[Constant[5]]");
    // An UNTYPED argument declines since #521 (both `Array` overloads match
    // and their returns differ) — the nominal `Class<4>` this row asserted
    // before the re-pin was the retracted pin.
    assert_eq!(ty(b"def f(c)\n  Array(c)\nend\n"), "Dynamic[top]");
    // …but a REBOUND local is typed on the REFERENCE, so the decline must
    // not reach it and the nominal fallback stands. (rigor-rs's own env is
    // empty inside a method body — see `arg_reach` — so the
    // answer here is the nominal `Class<4>`, not the Tuple the reference
    // sees; that gap is older than this slice.)
    assert_eq!(ty(b"def f(c)\n  c = [1, 2]\n  Array(c)\nend\n"), "Class<4>");
    // #1021: a CONDITIONAL rebind leaves the parameter reachable — the
    // reference's `Dynamic[top] | [1, 2]` is imprecise and declines too.
    assert_eq!(ty(b"def f(c)\n  c = [1, 2] if c.nil?\n  Array(c)\nend\n"), "Dynamic[top]");
    assert_eq!(ty(b"def f(c)\n  c ||= [1, 2]\n  Array(c)\nend\n"), "Dynamic[top]");
    // rand: 0-arg Float (Class<2>); a non-Range 1-arg with a TYPED argument
    // Integer (Class<1>, the reference's measured overload pick); an untyped
    // argument declines (#521), and a Range arg declines as before.
    assert_eq!(ty(b"rand\n"), "Class<2>");
    assert_eq!(ty(b"rand(5)\n"), "Class<1>");
    assert_eq!(ty(b"def f(c)\n  rand(c)\nend\n"), "Dynamic[top]");
    assert_eq!(ty(b"def f(c)\n  c = 5\n  rand(c)\nend\n"), "Class<1>");
    assert_eq!(ty(b"rand(1..5)\n"), "Dynamic[top]");
    // A union's precise members still pin `rand` (only `(int)` accepts a
    // String member, grid G1), unless one is a Range (or `0`).
    assert_eq!(ty(b"def f(c)\n  c = 'x' if c.nil?\n  rand(c)\nend\n"), "Class<1>");
    assert_eq!(ty(b"def f(c)\n  c = (1..2) if c.nil?\n  rand(c)\nend\n"), "Dynamic[top]");
}

/// The #521 decline over roots that are NOT a `def` local — ivar, cvar,
/// gvar, a proc-like parameter and an unresolvable constant (fixture 105).
/// `Dynamic[top]` is the decline; `Class<4>` is nominal `Array`, i.e. the
/// fold kept its answer because the reference types the root too.
#[test]
fn untyped_argument_roots_beyond_def_locals() {
    let ty = |src: &[u8]| -> String {
        let ast = lower(&parse(src));
        let index = CoreIndex::new();
        let typer = Typer::new(&index);
        let mut i = Interner::new();
        let env = TypeEnv::new();
        let call_id = ast
            .iter()
            .filter_map(|(id, n)| match n {
                Node::Call { receiver: None, method, .. }
                    if matches!(method.as_str(), "Float" | "Integer" | "Array" | "rand") =>
                {
                    Some(id)
                }
                _ => None,
            })
            .last()
            .unwrap();
        let t = typer.type_of(&ast, call_id, &env, &mut i);
        rigor_types::describe(&i, t)
    };
    // IVAR: no write in the class at all, and a write from an untyped ctor
    // parameter, are both the carrier; a ctor literal is not. A non-ctor
    // untyped write is `Dynamic[top] | nil` on the reference (its
    // read-before-write pass adds the nil) — imprecise since #1021, so it
    // declines too, as does an untyped write beside a typed one.
    assert_eq!(ty(b"class C\n  def v\n    Array(@x)\n  end\nend\n"), "Dynamic[top]");
    assert_eq!(
        ty(b"class C\n  def initialize(c)\n    @x = c\n  end\n  def v\n    Array(@x)\n  end\nend\n"),
        "Dynamic[top]"
    );
    assert_eq!(
        ty(b"class C\n  def initialize\n    @x = 'l'\n  end\n  def v\n    Array(@x)\n  end\nend\n"),
        "Class<4>"
    );
    assert_eq!(
        ty(b"class C\n  def s(c)\n    @x = c\n  end\n  def v\n    Array(@x)\n  end\nend\n"),
        "Dynamic[top]"
    );
    assert_eq!(
        ty(b"class C\n  def initialize(c)\n    @x = c\n  end\n  def r\n    @x = 1\n  end\n  def v\n    Array(@x)\n  end\nend\n"),
        "Dynamic[top]"
    );
    // …unless the reading def itself definitely rebinds it first.
    assert_eq!(
        ty(b"class C\n  def initialize(c)\n    @x = c\n  end\n  def v\n    @x = 1\n    Array(@x)\n  end\nend\n"),
        "Class<4>"
    );
    // CVAR: a class-body write is never recorded; a `def`-body one is.
    assert_eq!(
        ty(b"class C\n  @@n = nil\n  def v\n    Array(@@n)\n  end\nend\n"),
        "Dynamic[top]"
    );
    assert_eq!(
        ty(b"class C\n  def s\n    @@n = 'l'\n  end\n  def v\n    Array(@@n)\n  end\nend\n"),
        "Class<4>"
    );
    // GVAR: program-wide, so a top-level write counts.
    assert_eq!(ty(b"def f\n  Array($g)\nend\n"), "Dynamic[top]");
    assert_eq!(ty(b"$g = nil\ndef f\n  Array($g)\nend\n"), "Class<4>");
    assert_eq!(
        ty(b"$g = nil\ndef w(c)\n  $g = c\nend\ndef f\n  Array($g)\nend\n"),
        "Dynamic[top]"
    );
    // PROC-LIKE parameters are untyped; an ORDINARY block's parameter is
    // typed from the RBS yield and must keep its pin.
    assert_eq!(ty(b"F = ->(a) { Array(a) }\n"), "Dynamic[top]");
    assert_eq!(ty(b"F = lambda { |a| Array(a) }\n"), "Dynamic[top]");
    assert_eq!(ty(b"F = Proc.new { |a| Array(a) }\n"), "Dynamic[top]");
    assert_eq!(ty(b"[1].each { |a| Array(a) }\n"), "Class<4>");
    // A `->` body's local write never binds on the reference; the
    // `lambda {}` spelling's does.
    assert_eq!(ty(b"def f\n  ->(y) { y = 1; Array(y) }\nend\n"), "Dynamic[top]");
    assert_eq!(ty(b"def f\n  lambda { |y| y = 1; Array(y) }\nend\n"), "Class<4>");
    // CONSTANTS: only an unresolvable BARE name declines.
    assert_eq!(ty(b"def f\n  Array(NOPE_ZZZ)\nend\n"), "Dynamic[top]");
    assert_eq!(ty(b"def f\n  Array(String)\nend\n"), "Class<4>");
    assert_eq!(ty(b"def f\n  Array(Errno::ENOENT)\nend\n"), "Class<4>");
    assert_eq!(ty(b"def f\n  Array(ENV)\nend\n"), "Class<4>");
}
