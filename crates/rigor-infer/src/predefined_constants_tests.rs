use super::*;
use rigor_parse::{lower, parse, Node};
use rigor_types::{Scalar, Type};

/// rigor-rs#656 — `PredefinedConstantRefinements` tier 1. The type of the
/// receiver of the LAST receiver-bearing call in `src` — the same wiring as
/// the stage-2 tests so the shadow gate's `enclosing_prefix` is live.
fn ty_of_last_call_receiver(src: &[u8]) -> (Interner, TypeId) {
    let ast = lower(&parse(src));
    let index = CoreIndex::new();
    let source = SourceIndex::build(&ast, &index);
    let scopes = lexical_scopes(&ast);
    let typer = Typer::with_source(&index, &source).with_lexical_scopes(&scopes);
    let mut i = Interner::new();
    let env = TypeEnv::new();
    let recv = ast
        .iter()
        .filter_map(|(_id, n)| match n {
            Node::Call { receiver: Some(r), .. } => Some(*r),
            _ => None,
        })
        .last()
        .unwrap();
    let ty = typer.type_of(&ast, recv, &env, &mut i);
    (i, ty)
}

/// `Constant[Float]` carrying exactly `want`'s bits.
fn is_float_constant(i: &Interner, ty: TypeId, want: f64) -> bool {
    matches!(i.get(ty), Type::Constant(Scalar::Float(f)) if f.to_bits() == want.to_bits())
}

/// FIRE (the issue's two repros): the six exact-value Float constants fold
/// to `Constant[Float]` — each a bit pattern `Constant[…]` pins and the
/// diagnostic layer renders `Infinity` / `3.141592653589793` / … verbatim.
#[test]
fn folded_constants_type_their_literal_value() {
    for (src, want) in [
        (b"Float::INFINITY.bogus\n".as_slice(), f64::INFINITY),
        (b"Math::PI.bogus\n".as_slice(), std::f64::consts::PI),
        // A `::`-rooted read lands on the same flat lookup.
        (b"::Math::PI.bogus\n".as_slice(), std::f64::consts::PI),
        (b"Math::E.bogus\n".as_slice(), std::f64::consts::E),
        (b"Float::MAX.bogus\n".as_slice(), f64::MAX),
        // Upstream `Float::MIN` is DBL_MIN — `MIN_POSITIVE`, not `f64::MIN`.
        (b"Float::MIN.bogus\n".as_slice(), f64::MIN_POSITIVE),
        (b"Float::EPSILON.bogus\n".as_slice(), f64::EPSILON),
        // A lexically nested use site folds too (no project write shadows).
        (
            b"module M\n  Math::PI.bogus\nend\n".as_slice(),
            std::f64::consts::PI,
        ),
    ] {
        let (i, ty) = ty_of_last_call_receiver(src);
        assert!(
            is_float_constant(&i, ty, want),
            "{:?} -> {:?}",
            std::str::from_utf8(src).unwrap(),
            i.get(ty)
        );
    }
}

/// DECLINE — the `constant_shadowers`/`WRITTEN_CANDIDATE` stop: a project
/// `=` write binding the folded name (or a lexical candidate of it) stops
/// the fold, never minting `Constant[3.14…]` over the project's own
/// constant. Under-emit by design: the reference types the written VALUE.
#[test]
fn project_writes_shadow_the_fold() {
    for src in [
        b"Math::PI = 5\nMath::PI.bogus\n".as_slice(),
        b"module Math\n  PI = 5\nend\nMath::PI.bogus\n".as_slice(),
        b"module Float\n  INFINITY = 1\nend\nFloat::INFINITY.bogus\n".as_slice(),
        b"M::Math::PI = 5\nmodule M\n  Math::PI.bogus\nend\n".as_slice(),
        b"module N\n  Math::PI = 5\nend\nMath::PI.bogus\n".as_slice(),
        // A non-literal write shadows identically.
        b"def compute = 9.9\nMath::PI = compute()\nMath::PI.bogus\n".as_slice(),
    ] {
        let (i, ty) = ty_of_last_call_receiver(src);
        assert!(
            matches!(i.get(ty), Type::Dynamic(_)),
            "{:?} -> {:?}",
            std::str::from_utf8(src).unwrap(),
            i.get(ty)
        );
    }
}

/// FIRE — writes that do NOT shadow (oracle-probed): `||=`/`&&=`/`+=`
/// path writes, multiwrite targets, a bare `PI` at top level (a different
/// candidate), and an unrelated `Foo::PI` path write.
#[test]
fn non_shadowing_write_forms_still_fold() {
    for src in [
        b"Math::PI ||= 5\nMath::PI.bogus\n".as_slice(),
        b"Math::PI &&= 5\nMath::PI.bogus\n".as_slice(),
        b"Math::PI += 1\nMath::PI.bogus\n".as_slice(),
        b"X, Math::PI = 1, 5\nMath::PI.bogus\n".as_slice(),
        b"PI = 5\nMath::PI.bogus\n".as_slice(),
        b"Foo::PI = 5\nMath::PI.bogus\n".as_slice(),
    ] {
        let (i, ty) = ty_of_last_call_receiver(src);
        assert!(
            is_float_constant(&i, ty, std::f64::consts::PI),
            "{:?} -> {:?}",
            std::str::from_utf8(src).unwrap(),
            i.get(ty)
        );
    }
}
